/*
 * update.c - installation and A/B system updates.
 *
 * The installer lays down a GPT with five partitions:
 *
 *   ESP            64 MiB FAT image holding \EFI\BOOT\BOOTX64.EFI
 *   boot control    1 MiB, three copied records with a CRC and generation
 *   system A      256 MiB raw sysimage (kernel + system app packages)
 *   system B      256 MiB raw sysimage
 *   data           the rest, an encrypted AFS volume
 *
 * An update never touches the running slot or the user's data. It writes the
 * inactive system partition, verifies the image hash again while writing,
 * and asks the loader to try that slot on the next boot. The loader counts
 * attempts; this kernel marks a slot good once its storage path is up.
 */
#include "update.h"
#include "../common/bootcontrol.h"
#include "../common/gpt.h"
#include "../common/release_key.h"
#include "../common/rsa_verify.h"
#include "../common/sysimage.h"
#include "afs.h"
#include "blk.h"
#include "cpu.h"
#include "crypto.h"
#include "kernel.h"
#include "mm.h"
#include "rng.h"
#include "../include/bootinfo.h"

#define MAX_WHOLE_DISKS 4
#define MAX_IMAGE_BYTES (256ull * 1024 * 1024)
#define BOOT_IMAGE_MIN (1ull * 1024 * 1024)

struct install_disk {
    struct blkdev *dev;
    uint8_t guid[16];
    int has_gpt;
    int has_install;
    int boot_disk;
    struct gpt_partition boot, sys[2], data;
    int sys_present[2];
    struct blkdev *data_dev;
};

static struct install_disk disks[MAX_WHOLE_DISKS];
static int ndisks;
static int scanned;

static struct install_disk *first_install(void);

extern uint32_t boot_flags;
extern uint32_t boot_slot;
extern uint8_t boot_disk_guid[16];

static int dev_read(void *ctx, uint64_t lba, uint32_t count, void *buf) {
    return blk_read(ctx, lba, count, buf) ? -1 : 0;
}

static int dev_write(void *ctx, uint64_t lba, uint32_t count, const void *buf) {
    return blk_write(ctx, lba, count, buf) ? -1 : 0;
}

static void parts_clear(struct install_disk *id) {
    id->has_gpt = id->has_install = 0;
    id->boot_disk = 0;
    id->sys_present[0] = id->sys_present[1] = 0;
    id->data_dev = 0;
    memset(id->guid, 0, sizeof id->guid);
    memset(&id->boot, 0, sizeof id->boot);
    memset(id->sys, 0, sizeof id->sys);
    memset(&id->data, 0, sizeof id->data);
}

static void disk_scan(struct install_disk *id) {
    parts_clear(id);
    static struct gpt_partition parts[GPT_ENTRIES];
    struct gpt_io io = {id->dev, id->dev->sectors, dev_read, dev_write, {0}, {0}};
    int n = gpt_load(&io, parts, GPT_ENTRIES);
    if (n <= 0) return;
    id->has_gpt = 1;
    memcpy(id->guid, io.loaded_disk_guid, 16);
    id->boot_disk = (boot_flags & BOOT_FLAG_INSTALLED) && !memcmp(id->guid, boot_disk_guid, 16);
    if (!gpt_find_type(parts, n, gpt_type_aether_boot, 0, &id->boot)) return;
    if (id->boot.last_lba - id->boot.first_lba + 1 < BOOTCONTROL_COPIES) return;
    id->sys_present[0] = gpt_find_type(parts, n, gpt_type_aether_system_a, 0, &id->sys[0]);
    id->sys_present[1] = gpt_find_type(parts, n, gpt_type_aether_system_b, 0, &id->sys[1]);
    if (!id->sys_present[0] && !id->sys_present[1]) return;
    if (gpt_find_type(parts, n, gpt_type_aether_data, 0, &id->data)) {
        char name[16];
        snprintf(name, sizeof name, "%sp5", id->dev->name);
        id->data_dev = blk_partition(id->dev, id->data.first_lba, id->data.last_lba - id->data.first_lba + 1,
                                     name, "aether data");
    }
    id->has_install = 1;
}

void update_init(void) {
    if (scanned) return;
    scanned = 1;
    ndisks = 0;
    int n = blk_count();
    for (int i = 0; i < n && ndisks < MAX_WHOLE_DISKS; i++) {
        struct blkdev *d = blk_get(i);
        if (!d || !strncmp(d->name, "ram", 3)) continue;
        if (!strncmp(d->name, "loop", 4)) continue;
        /* Partitions are created after this scan, so at this point every
         * non-RAM device is a whole disk. */
        disks[ndisks].dev = d;
        disk_scan(&disks[ndisks]);
        ndisks++;
    }
    for (int i = 0; i < ndisks; i++)
        if (disks[i].has_install)
            kprintf("[update] installed system on %s (%d system slot%s)\n", disks[i].dev->name,
                    disks[i].sys_present[0] + disks[i].sys_present[1],
                    disks[i].sys_present[0] + disks[i].sys_present[1] == 1 ? "" : "s");
}

static struct install_disk *first_install(void) {
    if (boot_flags & BOOT_FLAG_INSTALLED) {
        for (int i = 0; i < ndisks; i++)
            if (disks[i].boot_disk) return &disks[i];
        return 0;
    }
    for (int i = 0; i < ndisks; i++)
        if (disks[i].has_install) return &disks[i];
    return 0;
}

struct blkdev *update_data_partition(void) {
    struct install_disk *id = first_install();
    return id && id->data_dev ? id->data_dev : 0;
}

/* ---- Boot control ------------------------------------------------------ */

static int bootcontrol_read(struct install_disk *id, struct bootcontrol *out) {
    uint8_t raw[BOOTCONTROL_COPIES * 512];
    struct gpt_io io = {id->dev, id->dev->sectors, dev_read, dev_write, {0}, {0}};
    if (io.read(io.ctx, id->boot.first_lba, BOOTCONTROL_COPIES, raw)) return -E_IO;
    int best = -1;
    for (int i = 0; i < BOOTCONTROL_COPIES; i++) {
        struct bootcontrol *c = (struct bootcontrol *)(raw + i * 512);
        if (!bootcontrol_valid(c)) continue;
        if (best < 0 || c->generation > ((struct bootcontrol *)(raw + best * 512))->generation) best = i;
    }
    if (best < 0) return -E_INVAL;
    *out = *(struct bootcontrol *)(raw + best * 512);
    return 0;
}

static int bootcontrol_write(struct install_disk *id, struct bootcontrol *bc) {
    uint8_t raw[BOOTCONTROL_COPIES * 512];
    bc->generation++;
    bootcontrol_finish(bc);
    for (int i = 0; i < BOOTCONTROL_COPIES; i++) memcpy(raw + i * 512, bc, sizeof *bc);
    return dev_write(id->dev, id->boot.first_lba, BOOTCONTROL_COPIES, raw) ? -E_IO : 0;
}

static int system_header(struct install_disk *id, int slot, struct sysimage_header *h) {
    if (slot < 0 || slot > 1 || !id->sys_present[slot]) return -E_NOENT;
    uint64_t bytes = (id->sys[slot].last_lba - id->sys[slot].first_lba + 1) * 512;
    if (dev_read(id->dev, id->sys[slot].first_lba, SYSIMAGE_HEADER_SIZE / 512, h)) return -E_IO;
    const char *why = sysimage_check(h, bytes);
    return why ? -E_INVAL : 0;
}

static uint32_t running_slot(void) {
    if ((boot_flags & BOOT_FLAG_INSTALLED) && boot_slot < 2) return boot_slot;
    return 0;
}

void update_boot_good(void) {
    struct install_disk *id = first_install();
    if (!id || !(boot_flags & BOOT_FLAG_INSTALLED) || boot_slot >= 2) return;
    struct bootcontrol bc;
    int rr = bootcontrol_read(id, &bc);
    if (rr) {
        kprintf("[update] boot control is unreadable (%d)\n", rr);
        return;
    }
    struct sysimage_header h;
    uint32_t version = 0;
    if (!system_header(id, (int)boot_slot, &h)) version = h.version_code;
    int changed = bc.good != boot_slot || bc.active != boot_slot || bc.pending != BOOTCONTROL_NONE ||
                  bc.attempts != 0 || (version && bc.min_version != version);
    if (!changed) return;
    kprintf("[update] slot %u: active %u, good %u, pending %u, attempts %u, version %u\n", boot_slot, bc.active,
            bc.good, bc.pending, bc.attempts, version);
    bc.good = boot_slot;
    bc.active = boot_slot;
    bc.pending = BOOTCONTROL_NONE;
    bc.attempts = 0;
    /* The running slot has reached the kernel's storage path. Make its
     * version the rollback floor; a failed staged update must be able to
     * fall back to this older slot and boot it. */
    if (version) bc.min_version = version;
    if (!bootcontrol_write(id, &bc))
        kprintf("[update] slot %u is marked good\n", boot_slot);
}

void update_info(struct update_info *out) {
    memset(out, 0, sizeof *out);
    out->boot_flags = boot_flags;
    out->boot_slot = boot_slot;
    out->pending_slot = BOOTCONTROL_NONE;
    struct install_disk *id = first_install();
    if (!id) return;
    out->installed = 1;
    out->slots = (uint32_t)(id->sys_present[0] + id->sys_present[1]);
    int any = id->sys_present[0] ? 0 : 1;
    out->slot_bytes = (id->sys_present[any] ? id->sys[any].last_lba - id->sys[any].first_lba + 1 : 0) * 512;
    out->running_slot = running_slot();
    struct bootcontrol bc;
    if (!bootcontrol_read(id, &bc)) {
        out->active_slot = bc.active;
        out->good_slot = bc.good;
        out->pending_slot = bc.pending;
        out->attempts = bc.attempts;
    }
    struct sysimage_header h;
    for (int i = 0; i < 2; i++) {
        if (!id->sys_present[i]) continue;
        if (!system_header(id, i, &h)) {
            if (i == (int)out->running_slot) {
                strlcpy(out->running_version, h.version, sizeof out->running_version);
                strlcpy(out->running_build, h.build, sizeof out->running_build);
            } else {
                strlcpy(out->other_version, h.version, sizeof out->other_version);
                strlcpy(out->other_build, h.build, sizeof out->other_build);
            }
        }
    }
}

/* ---- Image validation and writing -------------------------------------- */

static uint8_t canonical_header[SYSIMAGE_HEADER_SIZE];

/* Hash the canonical header (image_sha256 and signature zeroed) plus the
 * payload, check the self-hash, and verify the release RSA signature. */
static int hash_image(const struct sysimage_header *h, const void *image, uint8_t out[32]) {
    if (h->image_size < h->header_size || h->image_size > MAX_IMAGE_BYTES) return -E_INVAL;
    memcpy(canonical_header, image, sizeof canonical_header);
    memset(canonical_header + offsetof(struct sysimage_header, image_sha256), 0, 32);
    memset(canonical_header + offsetof(struct sysimage_header, signature), 0, SYSIMAGE_SIGNATURE_SIZE);
    struct sha256_ctx c;
    sha256_init(&c);
    sha256_update(&c, canonical_header, sizeof canonical_header);
    sha256_update(&c, (const uint8_t *)image + h->header_size, h->image_size - h->header_size);
    uint8_t digest[32];
    sha256_final(&c, digest);
    if (!crypto_memeq(digest, h->image_sha256, 32)) {
        kprintf("[update] system image failed its digest check\n");
        return -E_IO;
    }
    if (!rsa_verify_sha256(digest, h->signature, aether_release_modulus)) {
        kprintf("[update] system image signature is not from the release key\n");
        return -E_DENIED;
    }
    memcpy(out, digest, 32);
    return 0;
}

static int write_part(struct blkdev *d, const struct gpt_partition *p, const void *image, uint64_t len) {
    uint64_t bytes = (p->last_lba - p->first_lba + 1) * 512;
    if (!len || len > bytes || (len % 512)) return -E_INVAL;
    /* Invalidate the old header first: a power loss mid-write must not leave
     * the loader with a plausible header for a half-written image. */
    uint8_t zero[4096];
    memset(zero, 0, sizeof zero);
    if (blk_write(d, p->first_lba, sizeof zero / 512, zero)) return -E_IO;
    if (blk_write(d, p->first_lba, (uint32_t)(len / 512), image)) return -E_IO;
    if (blk_flush(d)) return -E_IO;
    return 0;
}

struct range_ctx {
    struct blkdev *parent;
    uint64_t first;
};

static int range_read(struct blkdev *d, uint64_t lba, uint32_t count, void *buf) {
    struct range_ctx *r = d->priv;
    return blk_read(r->parent, r->first + lba, count, buf);
}

static int range_write(struct blkdev *d, uint64_t lba, uint32_t count, const void *buf) {
    struct range_ctx *r = d->priv;
    return blk_write(r->parent, r->first + lba, count, buf);
}

static int range_flush(struct blkdev *d) {
    struct range_ctx *r = d->priv;
    return blk_flush(r->parent);
}

static int format_data(struct blkdev *disk, const struct gpt_partition *p, const char *pw, size_t pwlen) {
    struct range_ctx r = {disk, p->first_lba};
    struct blkdev vol;
    memset(&vol, 0, sizeof vol);
    strlcpy(vol.name, "install-data", sizeof vol.name);
    strlcpy(vol.model, "aether data", sizeof vol.model);
    vol.sectors = p->last_lba - p->first_lba + 1;
    vol.read = range_read;
    vol.write = range_write;
    vol.flush = range_flush;
    vol.priv = &r;
    int err = afs_format_partition(&vol, pw, pwlen);
    if (!err) blk_flush(disk);
    return err;
}

static int valid_boot_image(const void *data, uint64_t len, uint64_t max) {
    if (len < BOOT_IMAGE_MIN || len > max || (len % 512)) return 0;
    const uint8_t *p = data;
    if (p[0] != 0xEB && p[0] != 0xE9) return 0;
    if (p[510] != 0x55 || p[511] != 0xAA) return 0;
    /* Enough of the FAT BPB to reject arbitrary blobs. The loader, not the
     * kernel, is the code that will execute; this check is only a sanity
     * boundary before the image is allowed onto the ESP. */
    uint16_t bps = (uint16_t)(p[11] | p[12] << 8);
    uint8_t spc = p[13];
    if (bps != 512 && bps != 1024 && bps != 2048 && bps != 4096) return 0;
    if (spc == 0 || (spc & (spc - 1))) return 0;
    if ((p[14] | p[15] << 8) == 0 || p[16] == 0) return 0;
    return 1;
}

int update_install(int disk, const void *boot_image, uint64_t boot_len, const void *system_image, uint64_t system_len,
                   const char *passphrase, size_t passphrase_len) {
    if (disk < 0 || disk >= ndisks) return -E_INVAL;
    struct install_disk *id = &disks[disk];
    struct blkdev *mounted = update_data_partition();
    if ((boot_flags & BOOT_FLAG_INSTALLED) && mounted && blk_parent(mounted) == id->dev) return -E_PERM;
    if (id->dev->sectors < 2ull * 1024 * 1024) return -E_NOSPACE; /* less than 1 GiB */
    const struct sysimage_header *h = system_image;
    if (system_len < sizeof *h) return -E_INVAL;
    const char *why = sysimage_check(h, MAX_IMAGE_BYTES);
    if (why) {
        kprintf("[update] installation refused: %s\n", why);
        return -E_INVAL;
    }
    if (h->image_size != system_len) return -E_INVAL;
    uint8_t image_digest[32];
    int err = hash_image(h, system_image, image_digest);
    if (err) return err;

    /* The bootloader is the first code that runs, so the signed system image
     * names the exact boot image that goes with it. Without this, a fresh
     * install trusts a boot image only because it came from the same release
     * JSON as the (correctly signed) system image. */
    uint8_t digest[32];
    int all_zero = 1;
    for (int i = 0; i < 32; i++) all_zero &= h->boot_sha256[i] == 0;
    if (all_zero) {
        kprintf("[update] installation refused: the system image doesn't pin a boot image\n");
        return -E_INVAL;
    }
    sha256((const uint8_t *)boot_image, boot_len, digest);
    if (!crypto_memeq(digest, h->boot_sha256, 32)) {
        kprintf("[update] installation refused: the boot image doesn't match the signed system image\n");
        return -E_DENIED;
    }

    uint64_t first[5], last[5];
    if (gpt_layout(id->dev->sectors, first, last)) return -E_NOSPACE;
    uint64_t slot_bytes = (last[GPT_LAYOUT_SYSTEM_A] - first[GPT_LAYOUT_SYSTEM_A] + 1) * 512;
    if (system_len > slot_bytes || (why = sysimage_check(h, slot_bytes)) != 0) {
        kprintf("[update] installation refused: the system image doesn't fit the slot\n");
        return -E_INVAL;
    }
    uint64_t esp_bytes = (last[GPT_LAYOUT_ESP] - first[GPT_LAYOUT_ESP] + 1) * 512;
    if (!valid_boot_image(boot_image, boot_len, esp_bytes)) {
        kprintf("[update] installation refused: the boot image isn't a FAT boot sector\n");
        return -E_INVAL;
    }

    uint8_t disk_guid[16], guids[5][16];
    random_bytes(disk_guid, sizeof disk_guid);
    random_bytes(guids, sizeof guids);
    struct gpt_new_partition parts[5] = {
        {gpt_type_esp, guids[0], first[0], last[0], 0, "AETHER ESP"},
        {gpt_type_aether_boot, guids[1], first[1], last[1], 0, "AETHER BOOT"},
        {gpt_type_aether_system_a, guids[2], first[2], last[2], GPT_FLAG_LEGACY_BOOTABLE, "AETHER SYSTEM A"},
        {gpt_type_aether_system_b, guids[3], first[3], last[3], 0, "AETHER SYSTEM B"},
        {gpt_type_aether_data, guids[4], first[4], last[4], 0, "AETHER DATA"},
    };

    kprintf("[update] installing to %s: ESP %lu MiB, slots %lu MiB, data %lu MiB\n", id->dev->name,
            (last[0] - first[0] + 1) / 2048, (last[2] - first[2] + 1) / 2048, (last[4] - first[4] + 1) / 2048);

    /* Invalidate both slots before writing anything new. */
    uint8_t zero[4096];
    memset(zero, 0, sizeof zero);
    for (int s = 2; s <= 3; s++)
        if (blk_write(id->dev, first[s], sizeof zero / 512, zero)) return -E_IO;

    if (blk_write(id->dev, first[GPT_LAYOUT_ESP], (uint32_t)(boot_len / 512), boot_image)) {
        kprintf("[update] writing the boot image failed\n");
        return -E_IO;
    }
    if (blk_write(id->dev, first[GPT_LAYOUT_SYSTEM_A], (uint32_t)(system_len / 512), system_image)) {
        kprintf("[update] writing the system image failed\n");
        return -E_IO;
    }
    /* Boot control: slot A is active, but it is not marked good until the
     * installed system reaches its desktop. The loader then has a rollback
     * target (slot B) only after a successful update, which is correct: a
     * fresh install has nowhere else to go. */
    struct bootcontrol bc;
    memset(&bc, 0, sizeof bc);
    for (int i = 0; i < 8; i++) bc.magic[i] = "AETHBOOT"[i];
    bc.version = BOOTCONTROL_VERSION;
    bc.active = 0;
    bc.good = 0;
    bc.pending = BOOTCONTROL_NONE;
    bc.attempts = 0;
    bc.generation = 1;
    bc.image_size[0] = system_len;
    bc.min_version = h->version_code;
    memcpy(bc.image_sha256[0], image_digest, 32);
    bootcontrol_finish(&bc);
    uint8_t bcraw[BOOTCONTROL_COPIES * 512];
    for (int i = 0; i < BOOTCONTROL_COPIES; i++) memcpy(bcraw + i * 512, &bc, sizeof bc);
    if (blk_write(id->dev, first[GPT_LAYOUT_BOOT], BOOTCONTROL_COPIES, bcraw)) return -E_IO;

    struct gpt_partition data_part;
    memset(&data_part, 0, sizeof data_part);
    data_part.first_lba = first[GPT_LAYOUT_DATA];
    data_part.last_lba = last[GPT_LAYOUT_DATA];
    err = format_data(id->dev, &data_part, passphrase, passphrase_len);
    if (err) {
        kprintf("[update] formatting the data partition failed (%d)\n", err);
        return err;
    }

    struct gpt_io io = {id->dev, id->dev->sectors, dev_read, dev_write, {0}, {0}};
    memcpy(io.disk_guid, disk_guid, 16);
    if (gpt_create(&io, parts, 5)) {
        kprintf("[update] writing the GPT failed\n");
        return -E_IO;
    }
    blk_flush(id->dev);
    kprintf("[update] installation complete; reboot to start the installed system\n");
    return 0;
}

int update_install_system(const void *system_image, uint64_t system_len) {
    if (!(boot_flags & BOOT_FLAG_INSTALLED)) return -E_NOTSUP;
    struct install_disk *id = first_install();
    if (!id || !id->sys_present[0] || !id->sys_present[1]) return -E_NOTSUP;
    const struct sysimage_header *h = system_image;
    if (system_len < sizeof *h) return -E_INVAL;
    uint64_t bytes = (id->sys[0].last_lba - id->sys[0].first_lba + 1) * 512;
    const char *why = sysimage_check(h, bytes);
    if (why) {
        kprintf("[update] update refused: %s\n", why);
        return -E_INVAL;
    }
    if (h->image_size != system_len) return -E_INVAL;
    uint8_t image_digest[32];
    int err = hash_image(h, system_image, image_digest);
    if (err) return err;
    int running = (int)running_slot(), target = 1 - running;
    if (target < 0 || target > 1 || !id->sys_present[target] || !id->sys_present[running]) return -E_NOTSUP;
    struct bootcontrol bc;
    if (bootcontrol_read(id, &bc)) {
        memset(&bc, 0, sizeof bc);
        for (int i = 0; i < 8; i++) bc.magic[i] = "AETHBOOT"[i];
        bc.version = BOOTCONTROL_VERSION;
        bc.generation = 1;
        bc.good = (uint32_t)running;
        bc.min_version = 0;
        bc.attempts = 0;
    }
    if (h->version_code < bc.min_version) {
        kprintf("[update] update refused: version %s is below the rollback floor\n", h->version);
        return -E_DENIED;
    }
    err = write_part(id->dev, &id->sys[target], system_image, system_len);
    if (err) return err;
    bc.good = (uint32_t)running;
    bc.active = (uint32_t)target;
    bc.pending = (uint32_t)target;
    bc.attempts = 0;
    bc.image_size[target] = system_len;
    memcpy(bc.image_sha256[target], image_digest, 32);
    err = bootcontrol_write(id, &bc);
    if (err) return err;
    kprintf("[update] slot %d now holds %s; it will be tried on the next boot\n", target, h->version);
    return 0;
}

int update_list_disks(struct disk_info *out, int max) {
    int n = 0;
    for (int i = 0; i < ndisks && n < max; i++) {
        struct install_disk *id = &disks[i];
        memset(&out[n], 0, sizeof out[n]);
        strlcpy(out[n].name, id->dev->name, sizeof out[n].name);
        strlcpy(out[n].model, id->dev->model[0] ? id->dev->model : "disk", sizeof out[n].model);
        out[n].sectors = id->dev->sectors;
        out[n].installed = (uint32_t)id->has_install;
        out[n].has_gpt = (uint32_t)id->has_gpt;
        out[n].target_ok = id->dev->sectors >= 2ull * 1024 * 1024;
        n++;
    }
    return n;
}

void system_reboot(void) {
    kprintf("[update] reboot requested\n");
    /* Keyboard controller reset first; ACPI's reset register as a fallback. */
    for (int i = 0; i < 100; i++) io_wait();
    outb(0x64, 0xFE);
    for (volatile int i = 0; i < 1000000; i++) {}
    outb(0xCF9, 0x02);
    outb(0xCF9, 0x06);
    /* If the firmware ignores both, a triple fault is the last resort. */
    __asm__ volatile("cli");
    for (;;) __asm__ volatile("hlt");
}