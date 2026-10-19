/*
 * sysimage.h - the on-disk OS system image.
 *
 * A system image is what the loader boots from an AETHER_SYSTEM partition.
 * It contains the kernel once and every system app package once, with fixed
 * offsets so the loader can read each piece without a filesystem:
 *
 *   0                      header (4096 bytes)
 *   kernel_offset          kernel.elf
 *   apps[i].offset         package bytes, as made by tools/mkapp.py
 *   ...                    padding
 *
 * `payload_sha256` covers everything from `header_size` to `image_size`.
 * The header itself is covered indirectly: all offsets and sizes it contains
 * are range-checked, and the payload hash is what the loader actually trusts.
 * The loader refuses an image whose hash does not match before it maps any
 * code.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

#define SYSIMAGE_MAGIC "AETHERS1"
#define SYSIMAGE_FORMAT 1u
#define SYSIMAGE_HEADER_SIZE 4096u
#define SYSIMAGE_APPS_MAX 32
#define SYSIMAGE_NAME_MAX 32
#define SYSIMAGE_VERSION_MAX 32
#define SYSIMAGE_BUILD_MAX 32
#define SYSIMAGE_SIG_RSA2048_SHA256 1u
#define SYSIMAGE_SIGNATURE_SIZE 256

struct sysimage_app {
    char name[SYSIMAGE_NAME_MAX];
    uint64_t offset; /* from the start of the image; 4096-aligned */
    uint64_t size;
};

struct sysimage_header {
    char magic[8];
    uint32_t format;
    uint32_t header_size;
    uint64_t image_size; /* total bytes, including the header */
    uint64_t kernel_offset;
    uint64_t kernel_size;
    uint32_t app_count;
    uint32_t flags;
    char version[SYSIMAGE_VERSION_MAX];
    char build[SYSIMAGE_BUILD_MAX];
    struct sysimage_app apps[SYSIMAGE_APPS_MAX];
    uint8_t payload_sha256[32];
    uint8_t image_sha256[32]; /* canonical header+payload digest (see below) */
    uint32_t signature_algorithm; /* SYSIMAGE_SIG_RSA2048_SHA256 */
    uint32_t signature_size;
    uint8_t signature[SYSIMAGE_SIGNATURE_SIZE];
    uint32_t version_code; /* monotonic numeric version (major<<16 | minor<<8 | patch) */
    /* SHA-256 of the ESP boot image this release installs. It is covered by
     * the signature, so a fresh install can't be paired with a bootloader
     * from a different (or compromised) download. Zero in pre-1.0 images. */
    uint8_t boot_sha256[32];
    uint8_t reserved[2084];
};
_Static_assert(offsetof(struct sysimage_header, payload_sha256) == 1648, "sysimage layout");
_Static_assert(offsetof(struct sysimage_header, image_sha256) == 1680, "sysimage layout");
_Static_assert(offsetof(struct sysimage_header, signature_algorithm) == 1712, "sysimage layout");
_Static_assert(offsetof(struct sysimage_header, signature) == 1720, "sysimage layout");
_Static_assert(offsetof(struct sysimage_header, version_code) == 1976, "sysimage layout");
_Static_assert(offsetof(struct sysimage_header, boot_sha256) == 1980, "sysimage layout");
_Static_assert(offsetof(struct sysimage_header, reserved) == 2012, "sysimage layout");
_Static_assert(sizeof(struct sysimage_header) == SYSIMAGE_HEADER_SIZE, "sysimage header size");

/* Range and signature-shape checks only. The caller computes the canonical
 * digest (header with image_sha256/signature zeroed, then the payload),
 * compares it to image_sha256, verifies the signature, and, at boot, compares
 * it to the hash recorded in boot control. Returns 0 or an error string. */
static inline const char *sysimage_check(const struct sysimage_header *h, uint64_t partition_bytes) {
    if (h->magic[0] != 'A' || h->magic[1] != 'E' || h->magic[2] != 'T' || h->magic[3] != 'H' ||
        h->magic[4] != 'E' || h->magic[5] != 'R' || h->magic[6] != 'S' || h->magic[7] != '1')
        return "not an Aether system image";
    if (h->format != SYSIMAGE_FORMAT || h->header_size != SYSIMAGE_HEADER_SIZE) return "unknown system image format";
    if (h->version[SYSIMAGE_VERSION_MAX - 1] || h->build[SYSIMAGE_BUILD_MAX - 1])
        return "system image has unterminated version strings";
    if (h->signature_algorithm != SYSIMAGE_SIG_RSA2048_SHA256 ||
        h->signature_size != SYSIMAGE_SIGNATURE_SIZE)
        return "system image is not signed";
    if (!h->version_code) return "system image has no version";
    if (h->image_size < SYSIMAGE_HEADER_SIZE + 4096 || h->image_size > partition_bytes ||
        (h->image_size % 512))
        return "system image doesn't fit its partition";
    if (h->kernel_offset < SYSIMAGE_HEADER_SIZE || h->kernel_offset > h->image_size ||
        h->kernel_size < 4096 || h->kernel_size > h->image_size - h->kernel_offset)
        return "system image has a bad kernel range";
    if (h->app_count > SYSIMAGE_APPS_MAX) return "system image has too many apps";
    for (uint32_t i = 0; i < h->app_count; i++) {
        const struct sysimage_app *a = &h->apps[i];
        if (!a->name[0] || a->name[SYSIMAGE_NAME_MAX - 1]) return "system image app has a bad name";
        if (!a->size || a->offset < SYSIMAGE_HEADER_SIZE || a->offset > h->image_size ||
            a->size > h->image_size - a->offset)
            return "system image app is out of range";
    }
    return 0;
}
