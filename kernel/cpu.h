/* cpu.h - thin wrappers around x86_64 instructions the kernel needs. */
#pragma once
#include <stdint.h>

static inline void outb(uint16_t port, uint8_t val) {
    __asm__ volatile("outb %0, %1" : : "a"(val), "Nd"(port));
}

static inline uint8_t inb(uint16_t port) {
    uint8_t val;
    __asm__ volatile("inb %1, %0" : "=a"(val) : "Nd"(port));
    return val;
}

static inline void io_wait(void) { outb(0x80, 0); }

static inline uint64_t rdmsr(uint32_t msr) {
    uint32_t lo, hi;
    __asm__ volatile("rdmsr" : "=a"(lo), "=d"(hi) : "c"(msr));
    return ((uint64_t)hi << 32) | lo;
}

static inline void wrmsr(uint32_t msr, uint64_t v) {
    __asm__ volatile("wrmsr" : : "c"(msr), "a"((uint32_t)v), "d"((uint32_t)(v >> 32)));
}

static inline void cpuid(uint32_t leaf, uint32_t sub, uint32_t *a, uint32_t *b, uint32_t *c, uint32_t *d) {
    __asm__ volatile("cpuid" : "=a"(*a), "=b"(*b), "=c"(*c), "=d"(*d) : "a"(leaf), "c"(sub));
}

#define DEFINE_CR(n)                                                                  \
    static inline uint64_t read_cr##n(void) {                                         \
        uint64_t v;                                                                   \
        __asm__ volatile("mov %%cr" #n ", %0" : "=r"(v));                             \
        return v;                                                                     \
    }                                                                                 \
    static inline void write_cr##n(uint64_t v) {                                      \
        __asm__ volatile("mov %0, %%cr" #n : : "r"(v) : "memory");                    \
    }
DEFINE_CR(0)
DEFINE_CR(2)
DEFINE_CR(3)
DEFINE_CR(4)

static inline void invlpg(uint64_t va) { __asm__ volatile("invlpg (%0)" : : "r"(va) : "memory"); }
static inline void cli(void) { __asm__ volatile("cli" ::: "memory"); }
static inline void sti(void) { __asm__ volatile("sti" ::: "memory"); }
static inline void hlt(void) { __asm__ volatile("hlt" ::: "memory"); }
/* SMAP: temporarily allow / forbid kernel access to user pages. */
static inline void stac(void) { __asm__ volatile("stac" ::: "memory", "cc"); }
static inline void clac(void) { __asm__ volatile("clac" ::: "memory", "cc"); }

static inline uint64_t rdtsc(void) {
    uint32_t lo, hi;
    __asm__ volatile("rdtsc" : "=a"(lo), "=d"(hi));
    return ((uint64_t)hi << 32) | lo;
}

static inline uint64_t read_rflags(void) {
    uint64_t f;
    __asm__ volatile("pushfq; pop %0" : "=r"(f));
    return f;
}

__attribute__((noreturn)) static inline void halt_forever(void) {
    for (;;) __asm__ volatile("cli; hlt");
}

#define MSR_EFER 0xC0000080
#define MSR_STAR 0xC0000081
#define MSR_LSTAR 0xC0000082
#define MSR_SFMASK 0xC0000084
#define MSR_PAT 0x277
#define MSR_APIC_BASE 0x1B

#define EFER_SCE (1ull << 0)
#define EFER_NXE (1ull << 11)
#define CR0_MP (1ull << 1)
#define CR0_EM (1ull << 2)
#define CR0_WP (1ull << 16)
#define CR4_PGE (1ull << 7)
#define CR4_OSFXSR (1ull << 9)
#define CR4_OSXMMEXCPT (1ull << 10)
#define CR4_UMIP (1ull << 11)
#define CR4_SMEP (1ull << 20)
#define CR4_SMAP (1ull << 21)
