/*
 * drivers.c - the small C side of the Rust kernel drivers.
 *
 * Rust cannot call a C macro or take a C function's address for a variadic
 * function. This file exposes a few narrow, checked helpers instead. The
 * drivers themselves (NVMe in 0.8) live in drivers/ and are compiled as
 * no_std static libraries, so no part of the Rust runtime or allocator is
 * linked into the kernel.
 */
#include "blk.h"
#include "cpu.h"
#include "kernel.h"
#include "mm.h"
#include "pci.h"

void aether_log(const char *msg) { kprintf("[rust] %s\n", msg); }
void aether_log_hex(const char *label, uint64_t v) { kprintf("[rust] %s %lx\n", label, v); }

static uint16_t pit_read(void) {
    outb(0x43, 0x00);
    uint8_t lo = inb(0x40), hi = inb(0x40);
    return (uint16_t)(lo | hi << 8);
}

/* Monotonic milliseconds from the PIT, usable before interrupts are enabled
 * and while they are off (the drivers poll during boot and inside syscalls).
 * Channel 0 counts down at 1.193182 MHz, so accumulate the actual counter
 * delta rather than assuming one call means one millisecond. The driver
 * loops call this often enough to see every 1 ms wrap. */
uint64_t aether_pit_ms(void) {
    const uint32_t pit_hz = 1193182, divisor = pit_hz / TICK_HZ;
    static uint64_t ms;
    static uint32_t rem;
    static uint16_t last;
    static int started;
    uint16_t now = pit_read();
    if (!started) {
        started = 1;
        last = now;
        return 0;
    }
    uint32_t delta = now <= last ? (uint32_t)last - now : (uint32_t)last + divisor - now;
    last = now;
    rem += delta * 1000u;
    ms += rem / pit_hz;
    rem %= pit_hz;
    return ms;
}

int aether_map_mmio(uint64_t virt, uint64_t phys, uint64_t len) {
    if (!len || (phys & (PAGE_SIZE - 1)) || (virt & (PAGE_SIZE - 1))) return -1;
    for (uint64_t off = 0; off < len; off += PAGE_SIZE) {
        if (vmm_map(vmm_kernel_pml4(), virt + off, phys + off, PTE_RW | PTE_NX | PTE_G | PTE_PCD | PTE_PWT))
            return -1;
        invlpg(virt + off);
    }
    return 0;
}

void aether_invlpg(uint64_t virt) { invlpg(virt); }

uint64_t aether_pci_bar_mem(struct pci_dev *d, int n) {
    int is_io = 0;
    uint64_t bar = pci_bar_ex(d, n, &is_io);
    return is_io ? 0 : bar;
}

void aether_pci_disable(struct pci_dev *d) {
    uint16_t cmd = pci_read16(d, PCI_CMD);
    pci_write16(d, PCI_CMD, cmd & ~(PCI_CMD_MEM | PCI_CMD_BUS_MASTER));
}

__attribute__((noreturn)) void aether_kernel_panic(void) { panic("Rust kernel driver panic"); }

/* The NVMe driver is a Rust staticlib. */
extern int nvme_init(void);

void drivers_init(void) {
    int r = nvme_init();
    if (r) kprintf("[nvme] no NVMe namespace registered\n");
}
