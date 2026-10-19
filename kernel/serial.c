/* serial.c - COM1 output, for logging in QEMU (-serial) and VMware (serial port to file). */
#include "cpu.h"
#include "kernel.h"

#define COM1 0x3F8

static int present;

void serial_init(void) {
    outb(COM1 + 1, 0x00); /* disable interrupts */
    outb(COM1 + 3, 0x80); /* DLAB on to set the baud divisor */
    outb(COM1 + 0, 0x01); /* divisor 1 = 115200 baud */
    outb(COM1 + 1, 0x00);
    outb(COM1 + 3, 0x03); /* 8 data bits, no parity, 1 stop bit */
    outb(COM1 + 2, 0xC7); /* enable and clear FIFOs */

    /* Many laptops have no COM1. Probe with loopback mode so we don't spin forever. */
    outb(COM1 + 4, 0x1E);
    outb(COM1 + 0, 0xAE);
    present = inb(COM1 + 0) == 0xAE;
    outb(COM1 + 4, 0x0F);
}

void serial_putc(char c) {
    if (!present) return;
    if (c == '\n') serial_putc('\r');
    for (int spins = 0; spins < 100000 && !(inb(COM1 + 5) & 0x20); spins++)
        ;
    outb(COM1, (uint8_t)c);
}
