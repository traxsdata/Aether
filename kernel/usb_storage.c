/*
 * usb_storage.c - USB mass storage over bulk-only transport (BOT).
 *
 * The class driver speaks the SCSI transparent command set: TEST UNIT READY,
 * READ CAPACITY(10), READ(10), WRITE(10) and SYNCHRONIZE CACHE(10). One LUN
 * per device is supported; external hubs and UAS are deliberately out of
 * scope yet. The disk registers through blk.h, so AFS and the installer see
 * it exactly like SATA or NVMe.
 */
#include "../include/abi.h"
#include "blk.h"
#include "cpu.h"
#include "kernel.h"
#include "usb.h"

#define BOT_CBW_SIGNATURE 0x43425355u
#define BOT_CSW_SIGNATURE 0x53425355u
#define BOT_TIMEOUT_MS 2000
#define BOT_MAX_TRANSFER (4 * 1024)

#define SCSI_TEST_UNIT_READY 0x00
#define SCSI_INQUIRY 0x12
#define SCSI_READ_CAPACITY10 0x25
#define SCSI_READ10 0x28
#define SCSI_WRITE10 0x2A
#define SCSI_SYNCHRONIZE10 0x35

struct usb_storage {
    int used;
    struct blkdev dev;
    struct usb_device *usb;
    struct usb_endpoint *in, *out;
    uint8_t iface;
    uint32_t tag;
    uint32_t block_size;
    int failed;
};

static struct usb_storage disks[USB_MAX_DEVICES];
static int ndisks;

static uint32_t be32(const uint8_t *p) {
    return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
}

static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static void put_be32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

static void put_le32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

static int bot_reset(struct usb_storage *s) {
    int r = usb_control(s->usb, 0x21, 0xFF, 0, s->iface, 0, 0); /* Bulk-Only Mass Storage Reset */
    if (r < 0) return r;
    /* Clear a possible stall on both bulk endpoints. */
    usb_control(s->usb, 0x02, 1, 0, (uint16_t)(0x80 | s->in->number), 0, 0);
    usb_control(s->usb, 0x02, 1, 0, (uint16_t)s->out->number, 0, 0);
    return 0;
}

static int read_data(struct usb_storage *s, void *buf, uint32_t len) {
    uint8_t *p = buf;
    while (len) {
        uint32_t n = MIN(len, BOT_MAX_TRANSFER);
        int r = usb_ep_sync(s->usb, s->in, 0, p, n, BOT_TIMEOUT_MS);
        if (r) return r;
        p += n;
        len -= n;
    }
    return 0;
}

static int write_data(struct usb_storage *s, const void *buf, uint32_t len) {
    const uint8_t *p = buf;
    while (len) {
        uint32_t n = MIN(len, BOT_MAX_TRANSFER);
        int r = usb_ep_sync(s->usb, s->out, p, 0, n, BOT_TIMEOUT_MS);
        if (r) return r;
        p += n;
        len -= n;
    }
    return 0;
}

/* One CBW, optional data stage, one CSW. Returns 0 only when the device
 * reports success and the tag still matches. */
static int bot_command(struct usb_storage *s, const uint8_t *cdb, uint8_t cdb_len, void *data, uint32_t len,
                       int in) {
    uint8_t cbw[31], csw[13];
    memset(cbw, 0, sizeof cbw);
    put_le32(cbw, BOT_CBW_SIGNATURE);
    put_le32(cbw + 4, ++s->tag);
    put_le32(cbw + 8, len);
    cbw[12] = in ? 0x80 : 0; /* direction */
    cbw[13] = 0;             /* LUN */
    cbw[14] = cdb_len;
    memcpy(cbw + 15, cdb, cdb_len);
    if (usb_ep_sync(s->usb, s->out, cbw, 0, sizeof cbw, BOT_TIMEOUT_MS)) {
        bot_reset(s);
        return -E_IO;
    }
    if (len) {
        int r = in ? read_data(s, data, len) : write_data(s, data, len);
        if (r) {
            bot_reset(s);
            return r;
        }
    }
    if (usb_ep_sync(s->usb, s->in, 0, csw, sizeof csw, BOT_TIMEOUT_MS)) {
        bot_reset(s);
        return -E_IO;
    }
    if (le32(csw) != BOT_CSW_SIGNATURE || le32(csw + 4) != s->tag) {
        kprintf("[usb] storage: bad CSW (device stalled)\n");
        bot_reset(s);
        return -E_IO;
    }
    if (csw[12]) {
        kprintf("[usb] storage: SCSI status %u, residue %u\n", csw[12], le32(csw + 8));
        return -E_IO;
    }
    return 0;
}

static int scsi_ready(struct usb_storage *s) {
    uint8_t cdb[6] = {SCSI_TEST_UNIT_READY};
    for (int i = 0; i < 4; i++)
        if (!bot_command(s, cdb, sizeof cdb, 0, 0, 1)) return 0;
    return -E_IO;
}

static int scsi_capacity(struct usb_storage *s, uint32_t *blocks, uint32_t *block_size) {
    uint8_t cdb[10] = {SCSI_READ_CAPACITY10};
    uint8_t out[8];
    int r = bot_command(s, cdb, sizeof cdb, out, sizeof out, 1);
    if (r) return r;
    *blocks = be32(out) + 1;
    *block_size = be32(out + 4);
    if (*block_size != 512 || !*blocks) return -E_INVAL;
    return 0;
}

static int scsi_rw(struct usb_storage *s, uint64_t lba, uint32_t count, void *buf, int write) {
    uint32_t sectors = (uint32_t)(MAX(count, 1) * (s->block_size / 512));
    uint8_t cdb[10] = {write ? SCSI_WRITE10 : SCSI_READ10};
    put_be32(cdb + 2, (uint32_t)lba);
    cdb[7] = (uint8_t)(sectors >> 8);
    cdb[8] = (uint8_t)sectors;
    return bot_command(s, cdb, sizeof cdb, buf, count * 512, !write);
}

static int usb_read(struct blkdev *d, uint64_t lba, uint32_t count, void *buf) {
    struct usb_storage *s = d->priv;
    if (s->failed) return BLK_EIO;
    int r = scsi_rw(s, lba, count, buf, 0);
    if (r) s->failed = 1;
    return r ? BLK_EIO : 0;
}

static int usb_write(struct blkdev *d, uint64_t lba, uint32_t count, const void *buf) {
    struct usb_storage *s = d->priv;
    if (s->failed) return BLK_EIO;
    int r = scsi_rw(s, lba, count, (void *)buf, 1);
    if (r) s->failed = 1;
    return r ? BLK_EIO : 0;
}

static int usb_flush(struct blkdev *d) {
    struct usb_storage *s = d->priv;
    uint8_t cdb[10] = {SCSI_SYNCHRONIZE10};
    int r = bot_command(s, cdb, sizeof cdb, 0, 0, 0);
    return r ? BLK_EIO : 0;
}

void usb_storage_probe(struct usb_device *d, uint8_t iface, struct usb_endpoint *in, struct usb_endpoint *out) {
    if (ndisks >= USB_MAX_DEVICES) {
        kprintf("[usb] slot %u: too many storage devices\n", d->slot);
        return;
    }
    struct usb_storage *s = &disks[ndisks];
    memset(s, 0, sizeof *s);
    s->usb = d;
    s->in = in;
    s->out = out;
    s->iface = iface;
    bot_reset(s);
    if (scsi_ready(s)) {
        kprintf("[usb] slot %u: storage device did not become ready\n", d->slot);
        return;
    }
    uint32_t blocks = 0, block_size = 0;
    if (scsi_capacity(s, &blocks, &block_size)) {
        kprintf("[usb] slot %u: READ CAPACITY failed (only 512-byte sectors are supported)\n", d->slot);
        return;
    }
    s->block_size = block_size;
    s->used = 1;
    snprintf(s->dev.name, sizeof s->dev.name, "usb%d", ndisks);
    snprintf(s->dev.model, sizeof s->dev.model, "USB mass storage %04x:%04x", d->vendor, d->product);
    s->dev.sectors = (uint64_t)blocks * (block_size / 512);
    s->dev.read = usb_read;
    s->dev.write = usb_write;
    s->dev.flush = usb_flush;
    s->dev.priv = s;
    kprintf("[usb] slot %u: mass storage ready, %lu sectors (%lu MiB)\n", d->slot, s->dev.sectors,
            s->dev.sectors / 2048);
    blk_register(&s->dev);
    ndisks++;
}
