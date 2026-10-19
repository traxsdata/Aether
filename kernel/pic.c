/*
 * pic.c - 8259 PIC + 8254 PIT. Every PC-compatible machine, including
 * VMware, QEMU and (through chipset emulation) most laptops, still has
 * these; the roadmap replaces them with the APIC.
 */
#include "pic.h"
#include "cpu.h"
#include "input.h"
#include "kernel.h"
#include "proc.h"

#define PIC1_CMD 0x20
#define PIC1_DATA 0x21
#define PIC2_CMD 0xA0
#define PIC2_DATA 0xA1

volatile uint64_t ticks;
static uint16_t irq_mask = 0xFFFF;

static void pic_write_mask(void) {
    outb(PIC1_DATA, irq_mask & 0xFF);
    outb(PIC2_DATA, irq_mask >> 8);
}

void pic_init(void) {
    outb(PIC1_CMD, 0x11); io_wait(); /* init, expect ICW4 */
    outb(PIC2_CMD, 0x11); io_wait();
    outb(PIC1_DATA, 32); io_wait();  /* master: vectors 32-39 */
    outb(PIC2_DATA, 40); io_wait();  /* slave: vectors 40-47 */
    outb(PIC1_DATA, 4); io_wait();   /* slave on IRQ2 */
    outb(PIC2_DATA, 2); io_wait();
    outb(PIC1_DATA, 1); io_wait();   /* 8086 mode */
    outb(PIC2_DATA, 1); io_wait();
    irq_mask = 0xFFFF & ~(1 << 2);   /* only the cascade line */
    pic_write_mask();
}

void pic_unmask(int irq) {
    irq_mask &= (uint16_t)~(1u << irq);
    pic_write_mask();
}

static void pic_eoi(int irq) {
    if (irq >= 8) outb(PIC2_CMD, 0x20);
    outb(PIC1_CMD, 0x20);
}

void pit_init(unsigned hz) {
    unsigned divisor = 1193182 / hz;
    outb(0x43, 0x36); /* channel 0, lo/hi byte, rate generator */
    outb(0x40, divisor & 0xFF);
    outb(0x40, divisor >> 8);
}

/* UEFI may leave the local APIC timer armed; mask it so it can't fire into us. */
void lapic_quiesce(void) {
    uint64_t base = rdmsr(MSR_APIC_BASE);
    if (!(base & (1 << 11))) return; /* APIC globally disabled */
    volatile uint32_t *lapic = P2V(base & 0xFFFFF000);
    lapic[0x320 / 4] |= 1u << 16; /* LVT timer: masked */
    lapic[0x340 / 4] |= 1u << 16; /* LVT perf counter */
    lapic[0x370 / 4] |= 1u << 16; /* LVT error */
}

void irq_handle(int irq) {
    /* Spurious interrupts on IRQ7/15: the in-service bit isn't set. */
    if (irq == 7 || irq == 15) {
        outb(irq == 7 ? PIC1_CMD : PIC2_CMD, 0x0B);
        if (!(inb(irq == 7 ? PIC1_CMD : PIC2_CMD) & 0x80)) {
            if (irq == 15) outb(PIC1_CMD, 0x20);
            return;
        }
    }
    switch (irq) {
    case 0:
        ticks++;
        pic_eoi(irq);
        proc_tick();
        return;
    case 1:
        ps2_keyboard_irq();
        break;
    case 12:
        ps2_mouse_irq();
        break;
    }
    pic_eoi(irq);
}
