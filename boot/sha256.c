/* sha256.c - FIPS 180-4 SHA-256, self-contained for the UEFI loader. */
#include "sha256.h"

static uint32_t rotr(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }

static const uint32_t K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
};

static uint32_t get32(const uint8_t *p) {
    return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
}
static void put32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)(v >> 24), p[1] = (uint8_t)(v >> 16), p[2] = (uint8_t)(v >> 8), p[3] = (uint8_t)v;
}
static void copy(uint8_t *d, const uint8_t *s, size_t n) {
    while (n--) *d++ = *s++;
}

static void block(uint32_t h[8], const uint8_t *p) {
    uint32_t w[64];
    for (int i = 0; i < 16; i++) w[i] = get32(p + 4 * i);
    for (int i = 16; i < 64; i++) {
        uint32_t s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    uint32_t a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], k = h[7];
    for (int i = 0; i < 64; i++) {
        uint32_t t1 = k + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K[i] + w[i];
        uint32_t t2 = (rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
        k = g, g = f, f = e, e = d + t1;
        d = c, c = b, b = a, a = t1 + t2;
    }
    h[0] += a, h[1] += b, h[2] += c, h[3] += d, h[4] += e, h[5] += f, h[6] += g, h[7] += k;
}

void boot_sha256_init(struct boot_sha256 *c) {
    static const uint32_t iv[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                                   0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    for (int i = 0; i < 8; i++) c->h[i] = iv[i];
    c->len = 0;
    c->n = 0;
}

void boot_sha256_update(struct boot_sha256 *c, const void *data, size_t len) {
    const uint8_t *p = data;
    c->len += len;
    if (c->n) {
        size_t k = len < 64 - c->n ? len : 64 - c->n;
        copy(c->buf + c->n, p, k);
        c->n += k, p += k, len -= k;
        if (c->n < 64) return;
        block(c->h, c->buf);
        c->n = 0;
    }
    while (len >= 64) {
        block(c->h, p);
        p += 64, len -= 64;
    }
    if (len) copy(c->buf, p, len);
    c->n = len;
}

void boot_sha256_final(struct boot_sha256 *c, uint8_t out[32]) {
    uint64_t bits = c->len * 8;
    c->buf[c->n++] = 0x80;
    if (c->n > 56) {
        while (c->n < 64) c->buf[c->n++] = 0;
        block(c->h, c->buf);
        c->n = 0;
    }
    while (c->n < 56) c->buf[c->n++] = 0;
    for (int i = 0; i < 8; i++) c->buf[56 + i] = (uint8_t)(bits >> (56 - 8 * i));
    block(c->h, c->buf);
    for (int i = 0; i < 8; i++) put32(out + 4 * i, c->h[i]);
}
