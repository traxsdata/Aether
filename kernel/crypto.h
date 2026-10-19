/* crypto.h - SHA-256, HMAC, PBKDF2, HKDF and ChaCha20-Poly1305 (crypto.c). */
#pragma once
#include <stddef.h>
#include <stdint.h>

struct sha256_ctx { uint32_t h[8]; uint64_t len; uint8_t buf[64]; size_t n; };
void sha256_init(struct sha256_ctx *c);
void sha256_update(struct sha256_ctx *c, const void *data, size_t len);
void sha256_final(struct sha256_ctx *c, uint8_t out[32]);
void sha256(const void *data, size_t len, uint8_t out[32]);
void hmac_sha256(const void *key, size_t klen, const void *msg, size_t mlen, uint8_t out[32]);
void pbkdf2_sha256(const void *pw, size_t pwlen, const void *salt, size_t saltlen, uint32_t iters, uint8_t *out, size_t outlen);
/* HKDF-SHA256 (RFC 5869), extract+expand */
void hkdf_sha256(const void *salt, size_t saltlen, const void *ikm, size_t ikmlen, const void *info, size_t infolen, uint8_t *out, size_t outlen);
void chacha20_xor(const uint8_t key[32], const uint8_t nonce[12], uint32_t counter, const void *in, void *out, size_t len);
void poly1305(const uint8_t key[32], const void *msg, size_t len, uint8_t tag[16]);
/* RFC 8439 AEAD. open returns 0 if the tag verifies (constant-time compare), -1 otherwise; on failure out is zeroed. in and out may alias. */
void aead_seal(const uint8_t key[32], const uint8_t nonce[12], const void *aad, size_t aadlen, const void *in, size_t len, void *out, uint8_t tag[16]);
int aead_open(const uint8_t key[32], const uint8_t nonce[12], const void *aad, size_t aadlen, const void *in, size_t len, void *out, const uint8_t tag[16]);
int crypto_memeq(const void *a, const void *b, size_t n); /* constant time, 1 if equal */
void crypto_wipe(void *p, size_t n); /* not optimized away */
int crypto_selftest(void); /* 0 = all pass; kprintf a line per failing test */
