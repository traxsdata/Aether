/* sha256.h - SHA-256 for the bootloader (no libc, no kernel dependencies). */
#pragma once
#include <stddef.h>
#include <stdint.h>

struct boot_sha256 {
    uint32_t h[8];
    uint64_t len;
    uint8_t buf[64];
    size_t n;
};

void boot_sha256_init(struct boot_sha256 *c);
void boot_sha256_update(struct boot_sha256 *c, const void *data, size_t len);
void boot_sha256_final(struct boot_sha256 *c, uint8_t out[32]);
