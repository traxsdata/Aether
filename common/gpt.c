/*
 * gpt.c - GUID Partition Table reader and writer (see gpt.h).
 */
#include "gpt.h"

const uint8_t gpt_type_esp[16] = {0xc1, 0x2a, 0x73, 0x28, 0xf8, 0x1f, 0x11, 0xd2,
                                  0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b};
const uint8_t gpt_type_aether_boot[16] = {0xa0, 0xe7, 0xb0, 0x01, 0x5e, 0x4d, 0x4c, 0x8f,
                                          0x9b, 0x21, 0x6a, 0x7d, 0x3c, 0x2f, 0x1e, 0x01};
const uint8_t gpt_type_aether_system_a[16] = {0xa0, 0xe7, 0xb0, 0x02, 0x5e, 0x4d, 0x4c, 0x8f,
                                              0x9b, 0x21, 0x6a, 0x7d, 0x3c, 0x2f, 0x1e, 0x01};
const uint8_t gpt_type_aether_system_b[16] = {0xa0, 0xe7, 0xb0, 0x03, 0x5e, 0x4d, 0x4c, 0x8f,
                                              0x9b, 0x21, 0x6a, 0x7d, 0x3c, 0x2f, 0x1e, 0x01};
const uint8_t gpt_type_aether_data[16] = {0xa0, 0xe7, 0xb0, 0x04, 0x5e, 0x4d, 0x4c, 0x8f,
                                          0x9b, 0x21, 0x6a, 0x7d, 0x3c, 0x2f, 0x1e, 0x01};

#define GPT_HEADER_SIZE 92u
#define GPT_ENTRY_ARRAY_SECTORS ((GPT_ENTRIES * GPT_ENTRY_SIZE + GPT_SECTOR - 1) / GPT_SECTOR)
#define SIG "EFI PART"

static uint8_t sector[GPT_SECTOR] __attribute__((aligned(8)));
static uint8_t entry_bytes[GPT_ENTRIES * GPT_ENTRY_SIZE] __attribute__((aligned(8)));
static struct gpt_io *active_io;

static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}
static uint64_t le64(const uint8_t *p) { return le32(p) | (uint64_t)le32(p + 4) << 32; }
static void put32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v, p[1] = (uint8_t)(v >> 8), p[2] = (uint8_t)(v >> 16), p[3] = (uint8_t)(v >> 24);
}
static void put64(uint8_t *p, uint64_t v) {
    put32(p, (uint32_t)v);
    put32(p + 4, (uint32_t)(v >> 32));
}

uint32_t gpt_crc32(const void *data, size_t len) {
    uint32_t crc = 0xFFFFFFFFu;
    const uint8_t *p = data;
    for (size_t i = 0; i < len; i++) {
        crc ^= p[i];
        for (int k = 0; k < 8; k++) crc = (crc >> 1) ^ (0xEDB88320u & (0u - (crc & 1)));
    }
    return ~crc;
}

static int sig_ok(const uint8_t *p) {
    for (int i = 0; i < 8; i++)
        if (p[i] != (uint8_t)SIG[i]) return 0;
    return 1;
}

static void copy_name(char out[37], const uint8_t *utf16) {
    int n = 0;
    for (; n < 36; n++) {
        uint16_t c = (uint16_t)(utf16[n * 2] | utf16[n * 2 + 1] << 8);
        if (!c) break;
        out[n] = (c >= 32 && c < 127) ? (char)c : '?';
    }
    out[n] = 0;
}

static int parse_entries(const uint8_t *raw, uint32_t count, struct gpt_partition *out, int max) {
    int n = 0;
    for (uint32_t i = 0; i < count; i++) {
        const uint8_t *e = raw + (size_t)i * GPT_ENTRY_SIZE;
        int used = 0;
        for (int k = 0; k < 16; k++)
            if (e[k]) used = 1;
        if (!used) continue;
        if (n >= max) break;
        for (int k = 0; k < 16; k++) out[n].type[k] = e[k], out[n].guid[k] = e[16 + k];
        out[n].first_lba = le64(e + 32);
        out[n].last_lba = le64(e + 40);
        out[n].attributes = le64(e + 48);
        copy_name(out[n].name, e + 56);
        n++;
    }
    return n;
}

static int read_at(uint64_t lba, uint32_t count, void *buf) {
    return active_io->read(active_io->ctx, lba, count, buf);
}

static int write_at(uint64_t lba, uint32_t count, const void *buf) {
    return active_io->write(active_io->ctx, lba, count, buf);
}

/* Try one GPT header. Returns entry count, or -2 bad header, -3 bad entries. */
static int header_try(uint64_t header_lba, uint64_t alt_lba, uint64_t want_entries_lba, struct gpt_partition *out,
                      int max) {
    if (read_at(header_lba, 1, sector)) return -2;
    if (!sig_ok(sector) || le32(sector + 8) != 0x00010000u) return -2;
    uint32_t hs = le32(sector + 12);
    if (hs < GPT_HEADER_SIZE || hs > GPT_SECTOR) return -2;
    uint32_t want_crc = le32(sector + 16);
    put32(sector + 16, 0);
    uint32_t got_crc = gpt_crc32(sector, hs);
    put32(sector + 16, want_crc);
    if (got_crc != want_crc) return -2;
    if (le64(sector + 24) != header_lba || le64(sector + 32) != alt_lba) return -2;
    if (le64(sector + 40) < GPT_FIRST_USABLE || le64(sector + 40) >= le64(sector + 48)) return -2;
    /* Usable space must not include either GPT copy at the end of the disk. */
    if (active_io->sectors < GPT_ENTRY_ARRAY_SECTORS + 2 ||
        le64(sector + 48) > active_io->sectors - GPT_ENTRY_ARRAY_SECTORS - 1)
        return -2;
    uint64_t elba = le64(sector + 72);
    uint32_t nent = le32(sector + 80), esz = le32(sector + 84);
    uint32_t ecrc = le32(sector + 88);
    if (elba != want_entries_lba || nent > GPT_ENTRIES || esz != GPT_ENTRY_SIZE) return -3;
    uint32_t bytes = nent * esz;
    uint32_t sectors = (bytes + GPT_SECTOR - 1) / GPT_SECTOR;
    if (sectors > GPT_ENTRY_ARRAY_SECTORS || read_at(elba, sectors, entry_bytes)) return -3;
    if (gpt_crc32(entry_bytes, bytes) != ecrc) return -3;
    for (int i = 0; i < 16; i++) active_io->loaded_disk_guid[i] = sector[56 + i];
    uint64_t first_usable = le64(sector + 40), last_usable = le64(sector + 48);
    int n = parse_entries(entry_bytes, nent, out, max);
    for (int i = 0; i < n; i++) {
        if (!out[i].first_lba || out[i].last_lba < out[i].first_lba || out[i].first_lba < first_usable ||
            out[i].last_lba > last_usable)
            return -3;
        for (int j = 0; j < i; j++)
            if (out[i].first_lba <= out[j].last_lba && out[j].first_lba <= out[i].last_lba) return -3;
    }
    return n;
}

int gpt_load(struct gpt_io *io, struct gpt_partition *out, int max) {
    if (!io || !io->read || io->sectors < GPT_FIRST_USABLE + GPT_ENTRY_ARRAY_SECTORS + 2) return -1;
    active_io = io;
    for (int i = 0; i < 16; i++) io->loaded_disk_guid[i] = 0;
    uint64_t last = io->sectors - 1;
    int n = header_try(GPT_HEADER_LBA, last, GPT_ENTRIES_LBA, out, max);
    if (n >= 0) return n;
    uint64_t backup_entries = last - GPT_ENTRY_ARRAY_SECTORS;
    int b = header_try(last, GPT_HEADER_LBA, backup_entries, out, max);
    return b >= 0 ? b : n;
}

int gpt_find_type(const struct gpt_partition *parts, int count, const uint8_t type[16], int skip,
                  struct gpt_partition *out) {
    for (int i = 0; i < count; i++) {
        int same = 1;
        for (int k = 0; k < 16; k++)
            if (parts[i].type[k] != type[k]) same = 0;
        if (!same) continue;
        if (skip-- > 0) continue;
        if (out) *out = parts[i];
        return 1;
    }
    return 0;
}

static void make_header(uint64_t my_lba, uint64_t alt_lba, uint64_t entries_lba, uint64_t first_usable,
                        uint64_t last_usable, uint32_t entries_crc, const uint8_t disk_guid[16]) {
    for (int i = 0; i < GPT_SECTOR; i++) sector[i] = 0;
    for (int i = 0; i < 8; i++) sector[i] = (uint8_t)SIG[i];
    put32(sector + 8, 0x00010000u);
    put32(sector + 12, GPT_HEADER_SIZE);
    put64(sector + 24, my_lba);
    put64(sector + 32, alt_lba);
    put64(sector + 40, first_usable);
    put64(sector + 48, last_usable);
    for (int i = 0; i < 16; i++) sector[56 + i] = disk_guid[i];
    put64(sector + 72, entries_lba);
    put32(sector + 80, GPT_ENTRIES);
    put32(sector + 84, GPT_ENTRY_SIZE);
    put32(sector + 88, entries_crc);
    put32(sector + 16, gpt_crc32(sector, GPT_HEADER_SIZE));
}

int gpt_create(struct gpt_io *io, const struct gpt_new_partition *parts, int count) {
    if (!io || !io->read || !io->write || !parts || count < 0 || count > GPT_ENTRIES) return -1;
    uint64_t last = io->sectors - 1;
    if (io->sectors < GPT_FIRST_USABLE + 2 * GPT_ENTRY_ARRAY_SECTORS + 2) return -1;
    uint64_t first_usable = GPT_FIRST_USABLE;
    uint64_t last_usable = last - GPT_ENTRY_ARRAY_SECTORS - 1;
    for (int i = 0; i < count; i++) {
        const struct gpt_new_partition *p = &parts[i];
        if (!p->type || !p->first_lba || p->first_lba < first_usable || p->last_lba < p->first_lba ||
            p->last_lba > last_usable)
            return -1;
        for (int j = 0; j < i; j++)
            if (p->first_lba <= parts[j].last_lba && parts[j].first_lba <= p->last_lba) return -1;
    }
    active_io = io;

    uint8_t disk_guid[16];
    int any = 0;
    for (int i = 0; i < 16; i++) any |= io->disk_guid[i];
    if (any) {
        for (int i = 0; i < 16; i++) disk_guid[i] = io->disk_guid[i];
    } else {
        for (int i = 0; i < 16; i++) disk_guid[i] = 0;
        for (int i = 0; i < count; i++)
            for (int k = 0; k < 16; k++) disk_guid[k] ^= parts[i].guid ? parts[i].guid[k] : (uint8_t)(i * 31 + k);
        disk_guid[7] ^= 0x4F, disk_guid[15] ^= 0xA1;
    }

    /* Protective MBR: one type-0xEE partition from LBA 1 to the end. */
    for (int i = 0; i < GPT_SECTOR; i++) sector[i] = 0;
    sector[446] = 0x00;
    sector[447] = 0x00, sector[448] = 0x02, sector[449] = 0x00;
    sector[450] = 0xEE;
    sector[451] = 0xFE, sector[452] = 0xFF, sector[453] = 0xFF;
    put32(sector + 454, 1);
    uint64_t psize = io->sectors > 0xFFFFFFFFull ? 0xFFFFFFFFull : io->sectors - 1;
    put32(sector + 458, (uint32_t)psize);
    sector[510] = 0x55, sector[511] = 0xAA;
    if (write_at(0, 1, sector)) return -1;

    /* Partition entries. */
    for (size_t i = 0; i < sizeof entry_bytes; i++) entry_bytes[i] = 0;
    for (int i = 0; i < count; i++) {
        const struct gpt_new_partition *p = &parts[i];
        uint8_t *e = entry_bytes + (size_t)i * GPT_ENTRY_SIZE;
        for (int k = 0; k < 16; k++) e[k] = p->type[k];
        if (p->guid) {
            for (int k = 0; k < 16; k++) e[16 + k] = p->guid[k];
        } else {
            for (int k = 0; k < 16; k++) e[16 + k] = (uint8_t)(i * 67 + k * 13 + 1);
        }
        put64(e + 32, p->first_lba);
        put64(e + 40, p->last_lba);
        put64(e + 48, p->attributes);
        const char *name = p->name ? p->name : "aether";
        for (int k = 0; k < 36 && name[k]; k++) e[56 + k * 2] = (uint8_t)name[k];
    }
    uint32_t ecrc = gpt_crc32(entry_bytes, sizeof entry_bytes);
    /* Entries first: a header that points at them is written afterwards. */
    if (write_at(GPT_ENTRIES_LBA, GPT_ENTRY_ARRAY_SECTORS, entry_bytes)) return -1;
    if (write_at(last - GPT_ENTRY_ARRAY_SECTORS, GPT_ENTRY_ARRAY_SECTORS, entry_bytes)) return -1;

    make_header(GPT_HEADER_LBA, last, GPT_ENTRIES_LBA, first_usable, last_usable, ecrc, disk_guid);
    if (write_at(GPT_HEADER_LBA, 1, sector)) return -1;
    make_header(last, GPT_HEADER_LBA, last - GPT_ENTRY_ARRAY_SECTORS, first_usable, last_usable, ecrc, disk_guid);
    if (write_at(last, 1, sector)) return -1;
    return 0;
}

int gpt_layout(uint64_t disk_sectors, uint64_t out_first[5], uint64_t out_last[5]) {
    uint64_t esp = 64ull * 1024 * 1024 / GPT_SECTOR;
    uint64_t boot = 1ull * 1024 * 1024 / GPT_SECTOR;
    uint64_t system = 256ull * 1024 * 1024 / GPT_SECTOR;
    uint64_t align = 2048; /* 1 MiB */
    uint64_t sizes[5] = {esp, boot, system, system, 0};
    uint64_t at = 2048;
    for (int i = 0; i < 5; i++) {
        uint64_t start = (at + align - 1) / align * align;
        if (i == 4) {
            uint64_t last_usable = disk_sectors - GPT_ENTRY_ARRAY_SECTORS - 2;
            if (start >= last_usable || last_usable - start < 64 * 1024 * 1024 / GPT_SECTOR) return -1;
            out_first[i] = start;
            out_last[i] = last_usable;
            return 0;
        }
        out_first[i] = start;
        out_last[i] = start + sizes[i] - 1;
        at = out_last[i] + 1;
    }
    return -1;
}
