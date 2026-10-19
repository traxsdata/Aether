/*
 * bootcontrol.h - the tiny A/B boot record shared by the loader and kernel.
 *
 * It lives in its own GPT partition and is written to three consecutive
 * sectors. Every copy carries a generation and a CRC32; the newest valid
 * copy wins. The loader bumps `attempts` before it boots a pending slot, and
 * falls back to `good` when that reaches BOOTCONTROL_MAX_ATTEMPTS. The
 * kernel marks the running slot good once its storage path is up, so an
 * image that cannot boot is never kept in the boot path.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

#define BOOTCONTROL_MAGIC "AETHBOOT"
#define BOOTCONTROL_VERSION 1u
#define BOOTCONTROL_COPIES 3
#define BOOTCONTROL_NONE 0xFFFFFFFFu
#define BOOTCONTROL_MAX_ATTEMPTS 2u

struct bootcontrol {
    char magic[8];
    uint32_t version;
    uint32_t active;   /* slot to boot: 0 or 1 */
    uint32_t good;     /* last slot known to reach the desktop */
    uint32_t pending;  /* slot being tried, or BOOTCONTROL_NONE */
    uint32_t attempts;      /* boots of `active` while it is pending */
    uint32_t min_version;   /* never accept a system image below this */
    uint64_t generation;
    uint64_t image_size[2];
    uint8_t image_sha256[2][32];
    uint8_t reserved[388];
    uint32_t crc32; /* over the first 508 bytes */
};
_Static_assert(sizeof(struct bootcontrol) == 512, "bootcontrol size");

static inline uint32_t bootcontrol_crc(const struct bootcontrol *bc) {
    /* Duplicated tiny CRC32 so this header has no link-time dependency. */
    uint32_t crc = 0xFFFFFFFFu;
    const uint8_t *p = (const uint8_t *)bc;
    for (size_t i = 0; i < offsetof(struct bootcontrol, crc32); i++) {
        crc ^= p[i];
        for (int k = 0; k < 8; k++) crc = (crc >> 1) ^ (0xEDB88320u & (0u - (crc & 1)));
    }
    return ~crc;
}

static inline int bootcontrol_valid(const struct bootcontrol *bc) {
    for (int i = 0; i < 8; i++)
        if (bc->magic[i] != BOOTCONTROL_MAGIC[i]) return 0;
    if (bc->version != BOOTCONTROL_VERSION) return 0;
    if (bc->active > 1 || bc->good > 1 || bc->attempts > BOOTCONTROL_MAX_ATTEMPTS + 1) return 0;
    if (bc->pending != BOOTCONTROL_NONE && bc->pending > 1) return 0;
    return bc->crc32 == bootcontrol_crc(bc);
}

static inline void bootcontrol_finish(struct bootcontrol *bc) { bc->crc32 = bootcontrol_crc(bc); }
