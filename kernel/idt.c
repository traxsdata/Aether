/* idt.c - exception and interrupt routing. */
#include "idt.h"
#include "cpu.h"
#include "gdt.h"
#include "kernel.h"
#include "mm.h"
#include "pic.h"
#include "proc.h"
#include "uaccess.h"

struct __attribute__((packed)) idt_entry {
    uint16_t off_lo, sel;
    uint8_t ist, type;
    uint16_t off_mid;
    uint32_t off_hi, zero;
};

static struct idt_entry idt[256];
extern const uint64_t isr_stub_table[256];

static const char *exception_names[32] = {
    "divide error", "debug", "NMI", "breakpoint", "overflow", "bound range", "invalid opcode",
    "device not available", "double fault", "coprocessor overrun", "invalid TSS", "segment not present",
    "stack fault", "general protection fault", "page fault", "reserved", "x87 FP error", "alignment check",
    "machine check", "SIMD FP error", "virtualization", "control protection", "reserved", "reserved",
    "reserved", "reserved", "reserved", "reserved", "hypervisor injection", "VMM communication",
    "security exception", "reserved",
};

void idt_init(void) {
    for (int v = 0; v < 256; v++) {
        uint64_t off = isr_stub_table[v];
        idt[v] = (struct idt_entry){
            .off_lo = off & 0xFFFF,
            .sel = SEL_KCODE,
            .ist = 0,
            .type = 0x8E, /* present, ring 0 only, 64-bit interrupt gate (IF cleared on entry) */
            .off_mid = (off >> 16) & 0xFFFF,
            .off_hi = off >> 32,
        };
    }
    idt[8].ist = 1; /* double fault: always on a fresh stack */
    idt[2].ist = 2; /* NMI */
    idt[18].ist = 2; /* machine check */

    struct __attribute__((packed)) {
        uint16_t limit;
        uint64_t base;
    } idtr = {sizeof idt - 1, (uint64_t)idt};
    __asm__ volatile("lidt %0" : : "m"(idtr));
}

static void user_fault(struct isr_frame *f) {
    /* iretq would fault in ring 0 on a non-canonical RSP. Exceptions have no
     * PIC EOI to send, so this can kill immediately. */
    if (f->rsp >= USER_TOP)
        proc_kill_current(EXIT_KILLED_BASE + 13, "blocked: invalid user stack pointer");
    char why[96];
    const char *name = exception_names[f->vector];
    if (f->vector == 14) {
        uint64_t addr = read_cr2();
        if (!(f->error & 1)) { /* not present: maybe reserved memory touched for the first time */
            int r = umem_fault(addr, (f->error & 2) != 0, (f->error & 0x10) != 0);
            if (r == 0) return; /* mapped: retry the instruction */
            if (r == -2) proc_kill_current(EXIT_KILLED_BASE + 14, "ran out of memory (its memory limit was reached)");
        }
        const char *what = (f->error & 0x10) ? "execute" : (f->error & 2) ? "write" : "read";
        if (addr >= 0x800000000000ull)
            snprintf(why, sizeof why, "blocked: tried to %s kernel memory", what);
        else if (f->error & 1)
            snprintf(why, sizeof why, "blocked: tried to %s protected memory at %lx", what, addr);
        else
            snprintf(why, sizeof why, "crashed: %s of unmapped address %lx", what, addr);
    } else if (f->vector == 13) {
        snprintf(why, sizeof why, "blocked: privileged instruction or bad segment (rip %lx)", f->rip);
    } else {
        snprintf(why, sizeof why, "crashed: %s at %lx", name, f->rip);
    }
    proc_kill_current(EXIT_KILLED_BASE + (int)f->vector, why);
}

void isr_dispatch(struct isr_frame *f) {
    uint64_t v = f->vector;
    if (v < 32) {
        if ((f->cs & 3) == 3) {
            user_fault(f); /* returns only if it fixed the fault (memory made on first touch) */
            return;
        }
        panic_frame(exception_names[v], f);
    }
    if (v >= VEC_IRQ0 && v < VEC_IRQ0 + 16) {
        /* Send the EOI and service the device before any check that can kill
         * the interrupted process. Killing is noreturn (it schedules), and
         * doing it first would leave the 8259 in-service bit set: a timer IRQ
         * would then mask the timer, keyboard, mouse and NIC forever. */
        irq_handle((int)(v - VEC_IRQ0));
        /* Every path back to ring 3 goes through iretq. A non-canonical user
         * RSP faults in ring 0, so reject it after the IRQ is acknowledged. */
        if ((f->cs & 3) == 3 && f->rsp >= USER_TOP)
            proc_kill_current(EXIT_KILLED_BASE + 13, "blocked: invalid user stack pointer");
        /* Preempt user code, or the kernel's idle loop, if someone else should run. */
        if ((f->cs & 3) == 3 || cur_thread->idle) proc_maybe_preempt();
        return;
    }
    /* Anything else is a stray interrupt (e.g. left armed by firmware): ignore it. */
    if ((f->cs & 3) == 3 && f->rsp >= USER_TOP)
        proc_kill_current(EXIT_KILLED_BASE + 13, "blocked: invalid user stack pointer");
}
