/* rsa_verify.h - RSA-2048 PKCS#1 v1.5 SHA-256 signature verification.
 *
 * Shared by the UEFI loader and the kernel. It has no allocation, no libc and
 * no secret-dependent branches or table lookups; only public data is used.
 */
#pragma once
#include <stdint.h>

/* Returns 1 when `signature` is a valid RSASSA-PKCS1-v1_5/SHA-256 signature
 * of `digest` under `modulus` (big-endian, 256 bytes). Returns 0 otherwise. */
int rsa_verify_sha256(const uint8_t digest[32], const uint8_t signature[256],
                      const uint8_t modulus[256]);