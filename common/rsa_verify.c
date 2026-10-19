/*
 * rsa_verify.c - compact RSA-2048 verification (see rsa_verify.h).
 *
 * Verification only needs s^65537 mod n. The implementation uses 32-bit
 * limbs, a 128-limb product and a bit-at-a-time modular reduction. It is not
 * fast enough for a general RSA library, but it verifies an update in a few
 * million simple operations, which is well within boot time.
 */
#include "rsa_verify.h"

#define LIMBS 64
#define PLIMBS (LIMBS * 2)
#define MAX_BITS (PLIMBS * 32)

static void from_be(uint32_t out[LIMBS], const uint8_t in[256]) {
    for (int i = 0; i < LIMBS; i++) out[i] = 0;
    for (int i = 0; i < 256; i++) {
        int j = 255 - i;
        out[j / 4] |= (uint32_t)in[i] << (8 * (j % 4));
    }
}

static void to_be(uint8_t out[256], const uint32_t in[LIMBS]) {
    for (int i = 0; i < 256; i++) {
        int j = 255 - i;
        out[i] = (uint8_t)(in[j / 4] >> (8 * (j % 4)));
    }
}

static int cmp(const uint32_t a[LIMBS], const uint32_t b[LIMBS], int n) {
    for (int i = n - 1; i >= 0; i--) {
        if (a[i] > b[i]) return 1;
        if (a[i] < b[i]) return -1;
    }
    return 0;
}

static int is_zero(const uint32_t a[LIMBS], int n) {
    for (int i = 0; i < n; i++)
        if (a[i]) return 0;
    return 1;
}

static void sub(uint32_t a[LIMBS + 1], const uint32_t b[LIMBS], int n) {
    uint64_t borrow = 0;
    for (int i = 0; i <= n; i++) {
        uint64_t bv = i < n ? b[i] : 0;
        uint64_t v = (uint64_t)a[i] - bv - borrow;
        a[i] = (uint32_t)v;
        borrow = (v >> 63) & 1;
    }
}

/* out = a * b mod n */
static void mulmod(uint32_t out[LIMBS], const uint32_t a[LIMBS], const uint32_t b[LIMBS],
                   const uint32_t n[LIMBS], int nl) {
    uint32_t p[PLIMBS];
    for (int i = 0; i < PLIMBS; i++) p[i] = 0;
    for (int i = 0; i < nl; i++) {
        uint64_t carry = 0;
        for (int j = 0; j < nl; j++) {
            uint64_t cur = (uint64_t)a[i] * b[j] + p[i + j] + carry;
            p[i + j] = (uint32_t)cur;
            carry = cur >> 32;
        }
        int k = i + nl;
        while (carry) {
            uint64_t cur = (uint64_t)p[k] + carry;
            p[k] = (uint32_t)cur;
            carry = cur >> 32;
            k++;
        }
    }

    uint32_t rem[LIMBS + 1];
    for (int i = 0; i <= LIMBS; i++) rem[i] = 0;
    for (int bit = MAX_BITS - 1; bit >= 0; bit--) {
        uint32_t carry = 0;
        for (int i = 0; i <= LIMBS; i++) {
            uint32_t next = rem[i] >> 31;
            rem[i] = (rem[i] << 1) | carry;
            carry = next;
        }
        rem[0] |= (p[bit / 32] >> (bit % 32)) & 1u;
        int ge = rem[LIMBS] != 0;
        if (!ge) {
            int c = cmp(rem, n, nl);
            ge = c >= 0;
        }
        if (ge) sub(rem, n, nl);
    }
    for (int i = 0; i < LIMBS; i++) out[i] = i < nl ? rem[i] : 0;
}

int rsa_verify_sha256(const uint8_t digest[32], const uint8_t signature[256],
                      const uint8_t modulus[256]) {
    uint32_t n[LIMBS], s[LIMBS], r[LIMBS], one[LIMBS];
    from_be(n, modulus);
    from_be(s, signature);
    int nl = LIMBS;
    while (nl > 1 && n[nl - 1] == 0) nl--;
    /* The reduction below assumes a full 2048-bit modulus. Reject anything
     * smaller rather than silently returning wrong results. */
    if (nl != LIMBS || (n[LIMBS - 1] >> 31) == 0 || is_zero(n, nl) || cmp(s, n, nl) >= 0) return 0;
    for (int i = 0; i < LIMBS; i++) one[i] = 0;
    one[0] = 1;
    for (int i = 0; i < LIMBS; i++) r[i] = one[i];
    /* 65537 = 2^16 + 1. Start at the base, square 16 times, multiply once. */
    for (int i = 0; i < LIMBS; i++) r[i] = s[i];
    for (int i = 0; i < 16; i++) mulmod(r, r, r, n, nl);
    mulmod(r, r, s, n, nl);

    uint8_t em[256];
    to_be(em, r);
    static const uint8_t prefix[19] = {0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01,
                                        0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20};
    if (em[0] != 0 || em[1] != 1) return 0;
    int ps = 2;
    while (ps < 256 && em[ps] == 0xFF) ps++;
    if (ps - 2 < 8 || ps >= 256 || em[ps] != 0) return 0;
    ps++;
    if (256 - ps != 19 + 32) return 0;
    for (int i = 0; i < 19; i++)
        if (em[ps + i] != prefix[i]) return 0;
    uint32_t d = 0;
    for (int i = 0; i < 32; i++) d |= em[ps + 19 + i] ^ digest[i];
    return d == 0;
}