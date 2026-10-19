/*
 * rng.c - a ChaCha20-based generator keyed from hashed entropy: RDSEED and
 * RDRAND when the CPU has them, plus timestamp-counter jitter measured
 * against the 1 kHz timer, which works on any CPU. After every request the
 * key is replaced with fresh output, so a later memory leak can't reveal
 * earlier random numbers (keys we already generated).
 */
#include "rng.h"
#include "../include/abi.h"
#include "cpu.h"
#include "crypto.h"
#include "kernel.h"

static uint8_t key[32];
static uint64_t counter;
static int seeded;

static int rdseed64(uint64_t *v) {
    unsigned char ok;
    __asm__ volatile("rdseed %0; setc %1" : "=r"(*v), "=qm"(ok));
    return ok;
}

void rng_init(void) {
    struct sha256_ctx c;
    sha256_init(&c);
    uint32_t a, b, cx, d;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(cx), "=d"(d) : "a"(7), "c"(0));
    int have_rdseed = (b >> 18) & 1;
    for (int i = 0; i < 64; i++) {
        uint64_t v;
        if (have_rdseed && rdseed64(&v)) sha256_update(&c, &v, sizeof v);
        if (cpu_features & CPU_FEAT_RDRAND) {
            v = random_u64();
            sha256_update(&c, &v, sizeof v);
        }
    }
    /* Jitter: how many TSC cycles each of 64 timer ticks takes varies with
     * memory, caches and the hardware clock; hash all of them (64 ms). */
    for (int i = 0; i < 64; i++) {
        uint64_t t0 = ticks;
        uint64_t spins = 0;
        while (ticks == t0) spins++;
        uint64_t sample[2] = {rdtsc(), spins};
        sha256_update(&c, sample, sizeof sample);
    }
    sha256_final(&c, key);
    seeded = 1;
    kprintf("[rng] seeded (rdseed %s, rdrand %s, timer jitter)\n", have_rdseed ? "yes" : "no",
            (cpu_features & CPU_FEAT_RDRAND) ? "yes" : "no");
}

void rng_mix(const void *data, size_t len) {
    struct sha256_ctx c;
    sha256_init(&c);
    sha256_update(&c, key, sizeof key);
    uint64_t t = rdtsc();
    sha256_update(&c, &t, sizeof t);
    sha256_update(&c, data, len);
    sha256_final(&c, key);
    crypto_wipe(&c, sizeof c);
}

void random_bytes(void *out, size_t len) {
    if (!seeded) panic("rng: random bytes requested before the generator was seeded");
    uint8_t nonce[12] = {0};
    memcpy(nonce, &counter, sizeof counter);
    counter++;
    memset(out, 0, len);
    chacha20_xor(key, nonce, 1, out, out, len);
    uint8_t next[32] = {0};
    chacha20_xor(key, nonce, 0, next, next, sizeof next); /* block 0: the next key */
    memcpy(key, next, sizeof key);
    crypto_wipe(next, sizeof next);
}
