/* kernel.h - common kernel declarations. */
#pragma once
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include "../include/bootinfo.h"

#define AETHER_VERSION "1.9.0"
#define AETHER_BUILD __DATE__ " " __TIME__

#define PAGE_SIZE 4096ull
#define ALIGN_UP(x, a) (((x) + (a) - 1) & ~((uint64_t)(a) - 1))
#define ARRAY_LEN(a) (sizeof(a) / sizeof((a)[0]))
#define MIN(a, b) ((a) < (b) ? (a) : (b))
#define MAX(a, b) ((a) > (b) ? (a) : (b))

/* Physical -> kernel virtual through the higher-half direct map. */
static inline void *P2V(uint64_t phys) { return (void *)(phys + HHDM_BASE); }

/* common/string.c, common/fmt.c */
#include "../common/string.h"

/* lib.c */
uint64_t random_u64(void);

/* log.c: kprintf goes to serial, the kernel log ring, and the boot console. */
void kprintf(const char *fmt, ...);
size_t klog_read(char *dst, size_t len);
void console_init(const struct framebuffer *fb);
void console_disable(void); /* once the window manager owns the screen */
__attribute__((noreturn)) void panic(const char *fmt, ...);
struct isr_frame;
__attribute__((noreturn)) void panic_frame(const char *what, struct isr_frame *f);

/* serial.c */
void serial_init(void);
void serial_putc(char c);

/* cpu features (main.c) */
extern uint32_t cpu_features; /* CPU_FEAT_* from abi.h */
extern char cpu_vendor[16], cpu_brand[48];
extern struct framebuffer boot_fb;

/* timer (pic.c) */
extern volatile uint64_t ticks; /* 1 tick = 1 ms */
#define TICK_HZ 1000
