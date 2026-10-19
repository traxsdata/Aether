/* lib.c - kernel randomness and stack-smashing protection. */
#include "../include/abi.h"
#include "cpu.h"
#include "kernel.h"

/* ---- Randomness and stack protection ----------------------------------- */

uint64_t random_u64(void) {
    if (cpu_features & CPU_FEAT_RDRAND) {
        uint64_t v;
        unsigned char ok;
        for (int i = 0; i < 10; i++) {
            __asm__ volatile("rdrand %0; setc %1" : "=r"(v), "=qm"(ok));
            if (ok) return v;
        }
    }
    /* Fallback: mix the timestamp counter. Weak, but better than a constant. */
    static uint64_t state = 0x9E3779B97F4A7C15ull;
    state ^= rdtsc();
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    return state;
}

/* -fstack-protector-strong puts this value below the return address of
 * functions with arrays on the stack and checks it before returning. */
uint64_t __stack_chk_guard = 0x595E9FBD94FDA766ull;

__attribute__((noreturn)) void __stack_chk_fail(void) {
    panic("kernel stack buffer overflow detected (stack canary corrupted)");
}
