/*
 * crypto.c - SHA-256 (FIPS 180-4), HMAC (RFC 2104), PBKDF2 (RFC 8018),
 * HKDF (RFC 5869) and ChaCha20-Poly1305 (RFC 8439).
 *
 * Plain portable C: no SSE, no tables beyond the SHA-256 constants.
 * Nothing here branches on or indexes by secret data.
 */
#include "kernel.h"
#include "crypto.h"

static inline uint32_t rotr32(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }
static inline uint32_t rotl32(uint32_t x, int n) { return (x << n) | (x >> (32 - n)); }

static inline uint32_t load_be32(const uint8_t *p) {
    return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
}
static inline void store_be32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)(v >> 24), p[1] = (uint8_t)(v >> 16), p[2] = (uint8_t)(v >> 8), p[3] = (uint8_t)v;
}
static inline uint32_t load_le32(const uint8_t *p) {
    return p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}
static inline void store_le32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v, p[1] = (uint8_t)(v >> 8), p[2] = (uint8_t)(v >> 16), p[3] = (uint8_t)(v >> 24);
}
static inline uint64_t load_le64(const uint8_t *p) { return load_le32(p) | (uint64_t)load_le32(p + 4) << 32; }
static inline void store_le64(uint8_t *p, uint64_t v) {
    store_le32(p, (uint32_t)v);
    store_le32(p + 4, (uint32_t)(v >> 32));
}

int crypto_memeq(const void *a, const void *b, size_t n) {
    const volatile uint8_t *x = a, *y = b;
    uint32_t d = 0;
    for (size_t i = 0; i < n; i++) d |= x[i] ^ y[i];
    return (int)(((d - 1) >> 8) & 1);
}

void crypto_wipe(void *p, size_t n) {
    volatile uint8_t *q = p;
    while (n--) *q++ = 0;
    __asm__ volatile("" : : "r"(p) : "memory");
}

/* ---- SHA-256 ---- */

static const uint32_t K256[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
};

static void sha256_block(uint32_t h[8], const uint8_t *p) {
    uint32_t w[64];
    for (int i = 0; i < 16; i++) w[i] = load_be32(p + 4 * i);
    for (int i = 16; i < 64; i++) {
        uint32_t s0 = rotr32(w[i - 15], 7) ^ rotr32(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = rotr32(w[i - 2], 17) ^ rotr32(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    uint32_t a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], k = h[7];
    for (int i = 0; i < 64; i++) {
        uint32_t t1 = k + (rotr32(e, 6) ^ rotr32(e, 11) ^ rotr32(e, 25)) + ((e & f) ^ (~e & g)) + K256[i] + w[i];
        uint32_t t2 = (rotr32(a, 2) ^ rotr32(a, 13) ^ rotr32(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
        k = g, g = f, f = e, e = d + t1;
        d = c, c = b, b = a, a = t1 + t2;
    }
    h[0] += a, h[1] += b, h[2] += c, h[3] += d;
    h[4] += e, h[5] += f, h[6] += g, h[7] += k;
}

void sha256_init(struct sha256_ctx *c) {
    static const uint32_t iv[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                                   0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    for (int i = 0; i < 8; i++) c->h[i] = iv[i];
    c->len = 0;
    c->n = 0;
}

void sha256_update(struct sha256_ctx *c, const void *data, size_t len) {
    const uint8_t *p = data;
    c->len += len;
    if (c->n) {
        size_t k = MIN(len, 64 - c->n);
        memcpy(c->buf + c->n, p, k);
        c->n += k, p += k, len -= k;
        if (c->n < 64) return;
        sha256_block(c->h, c->buf);
        c->n = 0;
    }
    for (; len >= 64; p += 64, len -= 64) sha256_block(c->h, p);
    if (len) memcpy(c->buf, p, len);
    c->n = len;
}

void sha256_final(struct sha256_ctx *c, uint8_t out[32]) {
    uint64_t bits = c->len * 8;
    c->buf[c->n++] = 0x80;
    if (c->n > 56) {
        memset(c->buf + c->n, 0, 64 - c->n);
        sha256_block(c->h, c->buf);
        c->n = 0;
    }
    memset(c->buf + c->n, 0, 56 - c->n);
    for (int i = 0; i < 8; i++) c->buf[56 + i] = (uint8_t)(bits >> (56 - 8 * i));
    sha256_block(c->h, c->buf);
    for (int i = 0; i < 8; i++) store_be32(out + 4 * i, c->h[i]);
    crypto_wipe(c, sizeof(*c));
}

void sha256(const void *data, size_t len, uint8_t out[32]) {
    struct sha256_ctx c;
    sha256_init(&c);
    sha256_update(&c, data, len);
    sha256_final(&c, out);
}

/* ---- HMAC, PBKDF2, HKDF ---- */

struct hmac_ctx { struct sha256_ctx in, out; };

static void hmac_init(struct hmac_ctx *h, const void *key, size_t klen) {
    uint8_t k[64] = {0}, pad[64];
    if (klen > 64) sha256(key, klen, k);
    else memcpy(k, key, klen);
    for (int i = 0; i < 64; i++) pad[i] = k[i] ^ 0x36;
    sha256_init(&h->in);
    sha256_update(&h->in, pad, 64);
    for (int i = 0; i < 64; i++) pad[i] = k[i] ^ 0x5c;
    sha256_init(&h->out);
    sha256_update(&h->out, pad, 64);
    crypto_wipe(k, sizeof(k));
    crypto_wipe(pad, sizeof(pad));
}

static void hmac_final(struct hmac_ctx *h, uint8_t out[32]) {
    uint8_t d[32];
    sha256_final(&h->in, d);
    sha256_update(&h->out, d, 32);
    sha256_final(&h->out, out);
    crypto_wipe(d, sizeof(d));
}

void hmac_sha256(const void *key, size_t klen, const void *msg, size_t mlen, uint8_t out[32]) {
    struct hmac_ctx h;
    hmac_init(&h, key, klen);
    sha256_update(&h.in, msg, mlen);
    hmac_final(&h, out);
}

void pbkdf2_sha256(const void *pw, size_t pwlen, const void *salt, size_t saltlen, uint32_t iters, uint8_t *out,
                   size_t outlen) {
    struct hmac_ctx base, h;
    uint8_t u[32], t[32], ctr[4];
    hmac_init(&base, pw, pwlen);
    for (uint32_t blk = 1; outlen; blk++) {
        store_be32(ctr, blk);
        h = base;
        sha256_update(&h.in, salt, saltlen);
        sha256_update(&h.in, ctr, 4);
        hmac_final(&h, u);
        memcpy(t, u, 32);
        for (uint32_t i = 1; i < iters; i++) {
            h = base;
            sha256_update(&h.in, u, 32);
            hmac_final(&h, u);
            for (int j = 0; j < 32; j++) t[j] ^= u[j];
        }
        size_t k = MIN(outlen, 32);
        memcpy(out, t, k);
        out += k, outlen -= k;
    }
    crypto_wipe(&base, sizeof(base));
    crypto_wipe(u, sizeof(u));
    crypto_wipe(t, sizeof(t));
}

void hkdf_sha256(const void *salt, size_t saltlen, const void *ikm, size_t ikmlen, const void *info, size_t infolen,
                 uint8_t *out, size_t outlen) {
    uint8_t prk[32], t[32], i = 0;
    struct hmac_ctx h;
    /* An absent salt means HashLen zero bytes, which HMAC pads to the same key as an empty one. */
    hmac_sha256(salt, saltlen, ikm, ikmlen, prk);
    outlen = MIN(outlen, 255 * 32);
    while (outlen) {
        hmac_init(&h, prk, 32);
        if (i) sha256_update(&h.in, t, 32);
        sha256_update(&h.in, info, infolen);
        i++;
        sha256_update(&h.in, &i, 1);
        hmac_final(&h, t);
        size_t k = MIN(outlen, 32);
        memcpy(out, t, k);
        out += k, outlen -= k;
    }
    crypto_wipe(prk, sizeof(prk));
    crypto_wipe(t, sizeof(t));
}

/* ---- ChaCha20 ---- */

#define QR(a, b, c, d)                                                 \
    (a += b, d ^= a, d = rotl32(d, 16), c += d, b ^= c, b = rotl32(b, 12), \
     a += b, d ^= a, d = rotl32(d, 8), c += d, b ^= c, b = rotl32(b, 7))

static void chacha20_block(const uint32_t in[16], uint8_t out[64]) {
    uint32_t x[16];
    for (int i = 0; i < 16; i++) x[i] = in[i];
    for (int i = 0; i < 10; i++) {
        QR(x[0], x[4], x[8], x[12]);
        QR(x[1], x[5], x[9], x[13]);
        QR(x[2], x[6], x[10], x[14]);
        QR(x[3], x[7], x[11], x[15]);
        QR(x[0], x[5], x[10], x[15]);
        QR(x[1], x[6], x[11], x[12]);
        QR(x[2], x[7], x[8], x[13]);
        QR(x[3], x[4], x[9], x[14]);
    }
    for (int i = 0; i < 16; i++) store_le32(out + 4 * i, x[i] + in[i]);
    crypto_wipe(x, sizeof(x));
}

static void chacha20_setup(uint32_t s[16], const uint8_t key[32], const uint8_t nonce[12], uint32_t counter) {
    s[0] = 0x61707865, s[1] = 0x3320646e, s[2] = 0x79622d32, s[3] = 0x6b206574;
    for (int i = 0; i < 8; i++) s[4 + i] = load_le32(key + 4 * i);
    s[12] = counter;
    for (int i = 0; i < 3; i++) s[13 + i] = load_le32(nonce + 4 * i);
}

void chacha20_xor(const uint8_t key[32], const uint8_t nonce[12], uint32_t counter, const void *in, void *out,
                  size_t len) {
    uint32_t s[16];
    uint8_t ks[64];
    const uint8_t *p = in;
    uint8_t *q = out;
    chacha20_setup(s, key, nonce, counter);
    while (len) {
        chacha20_block(s, ks);
        s[12]++;
        size_t k = MIN(len, 64);
        for (size_t i = 0; i < k; i++) q[i] = p[i] ^ ks[i];
        p += k, q += k, len -= k;
    }
    crypto_wipe(s, sizeof(s));
    crypto_wipe(ks, sizeof(ks));
}

/* ---- Poly1305: h and r in three limbs of 44, 44 and 42 bits ---- */

#define M44 0xfffffffffffull
#define M42 0x3ffffffffffull

struct poly_ctx {
    uint64_t r0, r1, r2, s1, s2, h0, h1, h2, pad0, pad1;
    uint8_t buf[16];
    size_t n;
};

static void poly_init(struct poly_ctx *c, const uint8_t key[32]) {
    uint64_t t0 = load_le64(key) & 0x0ffffffc0fffffffull;
    uint64_t t1 = load_le64(key + 8) & 0x0ffffffc0ffffffcull;
    c->r0 = t0 & M44;
    c->r1 = ((t0 >> 44) | (t1 << 20)) & M44;
    c->r2 = t1 >> 24;
    /* 2^132 = 4 * 2^130 = 20 (mod 2^130 - 5) */
    c->s1 = c->r1 * 20;
    c->s2 = c->r2 * 20;
    c->h0 = c->h1 = c->h2 = 0;
    c->pad0 = load_le64(key + 16);
    c->pad1 = load_le64(key + 24);
    c->n = 0;
}

static void poly_blocks(struct poly_ctx *c, const uint8_t *p, size_t nblocks, uint64_t hibit) {
    typedef unsigned __int128 u128;
    uint64_t r0 = c->r0, r1 = c->r1, r2 = c->r2, s1 = c->s1, s2 = c->s2;
    uint64_t h0 = c->h0, h1 = c->h1, h2 = c->h2;
    for (; nblocks; nblocks--, p += 16) {
        uint64_t t0 = load_le64(p), t1 = load_le64(p + 8);
        h0 += t0 & M44;
        h1 += ((t0 >> 44) | (t1 << 20)) & M44;
        h2 += (t1 >> 24) | hibit;

        u128 d0 = (u128)h0 * r0 + (u128)h1 * s2 + (u128)h2 * s1;
        u128 d1 = (u128)h0 * r1 + (u128)h1 * r0 + (u128)h2 * s2;
        u128 d2 = (u128)h0 * r2 + (u128)h1 * r1 + (u128)h2 * r0;

        h0 = (uint64_t)d0 & M44;
        d1 += (uint64_t)(d0 >> 44);
        h1 = (uint64_t)d1 & M44;
        d2 += (uint64_t)(d1 >> 44);
        h2 = (uint64_t)d2 & M42;
        h0 += (uint64_t)(d2 >> 42) * 5;
        h1 += h0 >> 44;
        h0 &= M44;
    }
    c->h0 = h0, c->h1 = h1, c->h2 = h2;
}

static void poly_update(struct poly_ctx *c, const void *data, size_t len) {
    const uint8_t *p = data;
    if (c->n) {
        size_t k = MIN(len, 16 - c->n);
        for (size_t i = 0; i < k; i++) c->buf[c->n + i] = p[i];
        c->n += k, p += k, len -= k;
        if (c->n < 16) return;
        poly_blocks(c, c->buf, 1, 1ull << 40);
        c->n = 0;
    }
    poly_blocks(c, p, len / 16, 1ull << 40);
    p += len & ~(size_t)15;
    len &= 15;
    for (size_t i = 0; i < len; i++) c->buf[i] = p[i];
    c->n = len;
}

/* Zero-pad what has been fed so far to a 16-byte boundary (RFC 8439 2.8). */
static void poly_pad16(struct poly_ctx *c) {
    static const uint8_t zero[16];
    if (c->n) poly_update(c, zero, 16 - c->n);
}

static void poly_final(struct poly_ctx *c, uint8_t tag[16]) {
    if (c->n) {
        c->buf[c->n] = 1;
        for (size_t i = c->n + 1; i < 16; i++) c->buf[i] = 0;
        poly_blocks(c, c->buf, 1, 0);
    }
    uint64_t h0 = c->h0, h1 = c->h1, h2 = c->h2, k;
    for (int i = 0; i < 2; i++) {
        k = h1 >> 44, h1 &= M44, h2 += k;
        k = h2 >> 42, h2 &= M42, h0 += k * 5;
        k = h0 >> 44, h0 &= M44, h1 += k;
    }
    /* h is now below 2^130 + small; take h - p if that doesn't go negative. */
    uint64_t g0 = h0 + 5;
    uint64_t g1 = h1 + (g0 >> 44);
    g0 &= M44;
    uint64_t g2 = h2 + (g1 >> 44) - (1ull << 42);
    g1 &= M44;
    uint64_t keep = (g2 >> 63) - 1; /* all ones if g >= 0 */
    h0 = (h0 & ~keep) | (g0 & keep);
    h1 = (h1 & ~keep) | (g1 & keep);
    h2 = (h2 & ~keep) | (g2 & keep);

    unsigned __int128 acc = (unsigned __int128)(h0 | (h1 << 44)) + c->pad0;
    store_le64(tag, (uint64_t)acc);
    acc = (acc >> 64) + ((h1 >> 20) | (h2 << 24)) + c->pad1;
    store_le64(tag + 8, (uint64_t)acc);
    crypto_wipe(c, sizeof(*c));
}

void poly1305(const uint8_t key[32], const void *msg, size_t len, uint8_t tag[16]) {
    struct poly_ctx c;
    poly_init(&c, key);
    poly_update(&c, msg, len);
    poly_final(&c, tag);
}

/* ---- ChaCha20-Poly1305 AEAD ---- */

static void aead_mac(const uint8_t key[32], const uint8_t nonce[12], const void *aad, size_t aadlen, const void *ct,
                     size_t len, uint8_t tag[16]) {
    uint8_t otk[64] = {0}, lens[16];
    struct poly_ctx c;
    chacha20_xor(key, nonce, 0, otk, otk, 64);
    poly_init(&c, otk);
    poly_update(&c, aad, aadlen);
    poly_pad16(&c);
    poly_update(&c, ct, len);
    poly_pad16(&c);
    store_le64(lens, aadlen);
    store_le64(lens + 8, len);
    poly_update(&c, lens, 16);
    poly_final(&c, tag);
    crypto_wipe(otk, sizeof(otk));
}

void aead_seal(const uint8_t key[32], const uint8_t nonce[12], const void *aad, size_t aadlen, const void *in,
               size_t len, void *out, uint8_t tag[16]) {
    chacha20_xor(key, nonce, 1, in, out, len);
    aead_mac(key, nonce, aad, aadlen, out, len, tag);
}

int aead_open(const uint8_t key[32], const uint8_t nonce[12], const void *aad, size_t aadlen, const void *in,
              size_t len, void *out, const uint8_t tag[16]) {
    uint8_t want[16];
    aead_mac(key, nonce, aad, aadlen, in, len, want);
    int ok = crypto_memeq(want, tag, 16);
    crypto_wipe(want, sizeof(want));
    if (!ok) {
        memset(out, 0, len);
        return -1;
    }
    chacha20_xor(key, nonce, 1, in, out, len);
    return 0;
}

/* ---- self-test against the published vectors ---- */

static size_t unhex(uint8_t *dst, const char *s) {
    size_t n = 0;
    for (; s[0] && s[1]; s += 2) {
        int hi = s[0] <= '9' ? s[0] - '0' : (s[0] | 0x20) - 'a' + 10;
        int lo = s[1] <= '9' ? s[1] - '0' : (s[1] | 0x20) - 'a' + 10;
        dst[n++] = (uint8_t)(hi << 4 | lo);
    }
    return n;
}

static int check(const char *name, const uint8_t *got, const char *hex) {
    uint8_t want[128];
    size_t n = unhex(want, hex);
    if (memcmp(got, want, n) == 0) return 0;
    kprintf("crypto: selftest %s FAILED\n", name);
    return 1;
}

static const char sunscreen[] = "Ladies and Gentlemen of the class of '99: If I could offer you only one tip for "
                                "the future, sunscreen would be it.";

int crypto_selftest(void) {
    uint8_t key[32], nonce[12], aad[16], buf[128], out[128], tag[16];
    int fail = 0;

    sha256("abc", 3, out);
    fail += check("sha256 abc", out, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    sha256("", 0, out);
    fail += check("sha256 empty", out, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    sha256("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", 56, out);
    fail += check("sha256 448-bit", out, "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");

    memset(key, 0x0b, 20);
    hmac_sha256(key, 20, "Hi There", 8, out);
    fail += check("hmac rfc4231 #1", out, "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
    hmac_sha256("Jefe", 4, "what do ya want for nothing?", 28, out);
    fail += check("hmac rfc4231 #2", out, "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");

    pbkdf2_sha256("passwd", 6, "salt", 4, 1, out, 64);
    fail += check("pbkdf2 rfc7914", out,
                  "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
                  "49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783");

    memset(key, 0x0b, 22);
    unhex(buf, "000102030405060708090a0b0c");
    unhex(aad, "f0f1f2f3f4f5f6f7f8f9");
    hkdf_sha256(buf, 13, key, 22, aad, 10, out, 42);
    fail += check("hkdf rfc5869 #1", out,
                  "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865");

    for (int i = 0; i < 32; i++) key[i] = (uint8_t)i;
    unhex(nonce, "000000000000004a00000000");
    chacha20_xor(key, nonce, 1, sunscreen, out, 114);
    fail += check("chacha20 rfc8439", out,
                  "6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0b"
                  "f91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d8"
                  "07ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab7793736"
                  "5af90bbf74a35be6b40b8eedf2785e42874d");

    unhex(key, "85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b");
    poly1305(key, "Cryptographic Forum Research Group", 34, tag);
    fail += check("poly1305 rfc8439", tag, "a8061dc1305136c6c22b8baf0c0127a9");

    for (int i = 0; i < 32; i++) key[i] = (uint8_t)(0x80 + i);
    unhex(nonce, "070000004041424344454647");
    unhex(aad, "50515253c0c1c2c3c4c5c6c7");
    memcpy(buf, sunscreen, 114);
    aead_seal(key, nonce, aad, 12, buf, 114, buf, tag); /* in place */
    fail += check("aead seal rfc8439", buf,
                  "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6"
                  "3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36"
                  "92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc"
                  "3ff4def08e4b7a9de576d26586cec64b6116");
    fail += check("aead tag rfc8439", tag, "1ae10b594f09e26a7e902ecbd0600691");
    if (aead_open(key, nonce, aad, 12, buf, 114, out, tag) != 0 || !crypto_memeq(out, sunscreen, 114)) {
        kprintf("crypto: selftest aead open FAILED\n");
        fail++;
    }
    static const uint8_t zero[128];
    buf[57] ^= 0x04;
    memset(out, 0xaa, 114);
    if (aead_open(key, nonce, aad, 12, buf, 114, out, tag) != -1 || !crypto_memeq(out, zero, 114)) {
        kprintf("crypto: selftest aead reject FAILED\n");
        fail++;
    }
    return fail;
}
