/*
 * log.c - kernel logging and the crash screen.
 *
 * kprintf output goes to three places: COM1, an in-memory ring that
 * privileged apps can read (the terminal's `log` command), and, until the
 * window manager takes over the screen, a text console on the framebuffer.
 */
#include "../common/gfx.h"
#include "cpu.h"
#include "idt.h"
#include "kernel.h"

#define KLOG_SIZE 16384

static char klog[KLOG_SIZE];
static uint64_t klog_head; /* total bytes ever written */

static struct surface con;
static int con_on, con_x, con_y;

void console_init(const struct framebuffer *fb) {
    gfx_init_surface(&con, P2V(fb->base), (int)fb->width, (int)fb->height, (int)fb->pitch);
    con.swap = fb->format == FB_FORMAT_RGBX;
    gfx_fill(&con, 0, 0, con.w, con.h, 0x0B1020);
    con_x = 16;
    con_y = 16;
    con_on = 1;
}

void console_disable(void) { con_on = 0; }

static void con_putc(char c) {
    int fw = gfx_font_w(FONT_SMALL), fh = gfx_font_h(FONT_SMALL);
    if (c == '\n' || con_x + fw > con.w - 16) {
        con_x = 16;
        con_y += fh;
        if (con_y + fh > con.h - 16) {
            gfx_fill(&con, 0, 0, con.w, con.h, 0x0B1020);
            con_y = 16;
        }
        if (c == '\n') return;
    }
    gfx_char(&con, con_x, con_y, c, 0xC8D3F5, FONT_SMALL);
    con_x += fw;
}

static void log_putc(char c) {
    serial_putc(c);
    klog[klog_head++ % KLOG_SIZE] = c;
    if (con_on) con_putc(c);
}

void kprintf(const char *fmt, ...) {
    char buf[512];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    uint64_t flags = read_rflags();
    cli();
    for (char *p = buf; *p; p++) log_putc(*p);
    if (flags & 0x200) sti();
}

size_t klog_read(char *dst, size_t len) {
    uint64_t avail = klog_head < KLOG_SIZE ? klog_head : KLOG_SIZE;
    if (len > avail) len = avail;
    uint64_t start = klog_head - len;
    for (size_t i = 0; i < len; i++) dst[i] = klog[(start + i) % KLOG_SIZE];
    return len;
}

/* ---- Crash screen ------------------------------------------------------ */

static void crash_screen(const char *title, const char *msg, struct isr_frame *f) {
    struct surface s;
    gfx_init_surface(&s, P2V(boot_fb.base), (int)boot_fb.width, (int)boot_fb.height, (int)boot_fb.pitch);
    s.swap = boot_fb.format == FB_FORMAT_RGBX;
    gfx_gradient_v(&s, 0, 0, s.w, s.h, 0x3A0A1E, 0x12040A);
    int x = 48, y = 48;
    gfx_text(&s, x, y, ":(", 0xFF6B8B, FONT_LARGE);
    y += 56;
    gfx_text(&s, x, y, title, 0xFFFFFF, FONT_MEDIUM);
    y += 36;
    gfx_text(&s, x, y, msg, 0xFFC2D1, FONT_SMALL);
    y += 32;
    if (f) {
        char line[128];
        const char *names[] = {"rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp",
                               "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"};
        uint64_t vals[] = {f->rax, f->rbx, f->rcx, f->rdx, f->rsi, f->rdi, f->rbp, f->rsp,
                           f->r8, f->r9, f->r10, f->r11, f->r12, f->r13, f->r14, f->r15};
        snprintf(line, sizeof line, "vector %lu  error %lx  rip %016lx  cs %lx  rflags %lx  cr2 %016lx",
                 f->vector, f->error, f->rip, f->cs, f->rflags, read_cr2());
        gfx_text(&s, x, y, line, 0xFFFFFF, FONT_SMALL);
        y += 24;
        for (int i = 0; i < 16; i += 4) {
            snprintf(line, sizeof line, "%-3s %016lx   %-3s %016lx   %-3s %016lx   %-3s %016lx", names[i],
                     vals[i], names[i + 1], vals[i + 1], names[i + 2], vals[i + 2], names[i + 3], vals[i + 3]);
            gfx_text(&s, x, y, line, 0xE8B4C4, FONT_SMALL);
            y += 18;
        }
    }
    y += 24;
    gfx_text(&s, x, y, "The system has been halted to protect your data. Restart the machine to continue.",
             0xFFFFFF, FONT_SMALL);
}

__attribute__((noreturn)) static void do_panic(const char *msg, struct isr_frame *f) {
    cli();
    console_disable();
    serial_putc('\n');
    kprintf("*** KERNEL PANIC: %s\n", msg);
    if (f) kprintf("    rip=%016lx rsp=%016lx cr2=%016lx err=%lx\n", f->rip, f->rsp, read_cr2(), f->error);
    crash_screen("NewOS stopped: kernel panic", msg, f);
    halt_forever();
}

__attribute__((noreturn)) void panic(const char *fmt, ...) {
    char buf[256];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    do_panic(buf, 0);
}

__attribute__((noreturn)) void panic_frame(const char *what, struct isr_frame *f) {
    char buf[160];
    snprintf(buf, sizeof buf, "%s in kernel mode at %016lx", what, f->rip);
    do_panic(buf, f);
}
