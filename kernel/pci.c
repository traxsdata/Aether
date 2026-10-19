/*
 * pci.c - PCI enumeration through configuration mechanism #1: write an
 * address (enable bit, bus, device, function, dword offset) to port 0xCF8,
 * then read or write the selected dword at port 0xCFC.
 *
 * The scan is brute force: every bus and device, function 0, and functions
 * 1-7 only when function 0's header type has the multi-function bit. It
 * runs once at boot, before interrupts are on, so there's no locking around
 * the 0xCF8/0xCFC pair.
 */
#include "pci.h"
#include "kernel.h"

#define PCI_ADDR 0xCF8
#define PCI_DATA 0xCFC
#define MAX_PCI 64

static struct pci_dev devs[MAX_PCI];
static int ndevs;

static inline void outl(uint16_t port, uint32_t v) { __asm__ volatile("outl %0, %1" : : "a"(v), "Nd"(port)); }
static inline uint32_t inl(uint16_t port) {
    uint32_t v;
    __asm__ volatile("inl %1, %0" : "=a"(v) : "Nd"(port));
    return v;
}

static uint32_t cfg_addr(uint8_t bus, uint8_t dev, uint8_t fn, uint8_t off) {
    return 0x80000000u | ((uint32_t)bus << 16) | ((uint32_t)(dev & 31) << 11) | ((uint32_t)(fn & 7) << 8) |
           (off & 0xFCu);
}

static uint32_t cfg_read(uint8_t bus, uint8_t dev, uint8_t fn, uint8_t off) {
    outl(PCI_ADDR, cfg_addr(bus, dev, fn, off));
    return inl(PCI_DATA);
}

uint32_t pci_read32(const struct pci_dev *d, uint8_t off) { return cfg_read(d->bus, d->dev, d->fn, off); }

uint16_t pci_read16(const struct pci_dev *d, uint8_t off) {
    return (uint16_t)(pci_read32(d, off) >> ((off & 2) * 8));
}

void pci_write32(const struct pci_dev *d, uint8_t off, uint32_t v) {
    outl(PCI_ADDR, cfg_addr(d->bus, d->dev, d->fn, off));
    outl(PCI_DATA, v);
}

/* Read-modify-write of the containing dword. The Status register (next to
 * Command) has write-1-to-clear bits, so when touching Command we write
 * zeros into Status, which changes nothing. */
void pci_write16(const struct pci_dev *d, uint8_t off, uint16_t v) {
    uint32_t shift = (off & 2) * 8;
    uint32_t old = pci_read32(d, off);
    if ((off & 0xFC) == PCI_CMD) old &= 0x0000FFFFu;
    pci_write32(d, off, (old & ~(0xFFFFu << shift)) | ((uint32_t)v << shift));
}

uint64_t pci_bar_ex(const struct pci_dev *d, int n, int *is_io) {
    if (is_io) *is_io = 0;
    if (n < 0 || n > 5) return 0;
    uint8_t off = (uint8_t)(0x10 + n * 4);
    uint32_t lo = pci_read32(d, off);
    if (lo & 1) { /* I/O space */
        if (is_io) *is_io = 1;
        return lo & ~3u;
    }
    uint64_t base = lo & ~0xFull;
    if (((lo >> 1) & 3) == 2 && n < 5) base |= (uint64_t)pci_read32(d, (uint8_t)(off + 4)) << 32; /* 64-bit */
    return base;
}

uint64_t pci_bar(const struct pci_dev *d, int n) { return pci_bar_ex(d, n, 0); }

void pci_enable_bus_master_mem(const struct pci_dev *d) {
    uint16_t cmd = pci_read16(d, PCI_CMD);
    pci_write16(d, PCI_CMD, cmd | PCI_CMD_MEM | PCI_CMD_BUS_MASTER | PCI_CMD_INTX_DISABLE);
}

int pci_count(void) { return ndevs; }
struct pci_dev *pci_get(int idx) { return idx >= 0 && idx < ndevs ? &devs[idx] : 0; }

struct pci_dev *pci_find_class(uint8_t class, uint8_t subclass, uint8_t progif, int idx) {
    for (int i = 0; i < ndevs; i++) {
        struct pci_dev *d = &devs[i];
        if (d->class != class) continue;
        if (subclass != PCI_ANY && d->subclass != subclass) continue;
        if (progif != PCI_ANY && d->progif != progif) continue;
        if (idx-- == 0) return d;
    }
    return 0;
}

static const char *class_name(uint8_t c, uint8_t s) {
    switch (c) {
    case 0x01:
        switch (s) {
        case 0x01: return "IDE";
        case 0x06: return "SATA";
        case 0x08: return "NVMe";
        default: return "storage";
        }
    case 0x02: return "network";
    case 0x03: return "display";
    case 0x04: return "multimedia";
    case 0x06:
        switch (s) {
        case 0x00: return "host bridge";
        case 0x01: return "ISA bridge";
        case 0x04: return "PCI bridge";
        default: return "bridge";
        }
    case 0x0C:
        switch (s) {
        case 0x03: return "USB";
        case 0x05: return "SMBus";
        default: return "serial bus";
        }
    default: return "other";
    }
}

static void probe(uint8_t bus, uint8_t dev, uint8_t fn) {
    uint32_t id = cfg_read(bus, dev, fn, 0x00);
    if ((id & 0xFFFF) == 0xFFFF || ndevs == MAX_PCI) return;
    uint32_t cls = cfg_read(bus, dev, fn, 0x08);
    struct pci_dev *d = &devs[ndevs++];
    d->bus = bus, d->dev = dev, d->fn = fn;
    d->vendor = (uint16_t)id, d->device = (uint16_t)(id >> 16);
    d->class = (uint8_t)(cls >> 24), d->subclass = (uint8_t)(cls >> 16), d->progif = (uint8_t)(cls >> 8);
    kprintf("[pci] %02x:%02x.%u %04x:%04x class %02x.%02x.%02x %s\n", bus, dev, fn, d->vendor, d->device, d->class,
            d->subclass, d->progif, class_name(d->class, d->subclass));
}

void pci_init(void) {
    ndevs = 0;
    for (unsigned bus = 0; bus < 256; bus++) {
        for (uint8_t dev = 0; dev < 32; dev++) {
            if ((cfg_read((uint8_t)bus, dev, 0, 0x00) & 0xFFFF) == 0xFFFF) continue;
            uint8_t htype = (uint8_t)(cfg_read((uint8_t)bus, dev, 0, 0x0C) >> 16);
            int nfn = (htype & 0x80) ? 8 : 1;
            for (uint8_t fn = 0; fn < nfn; fn++) probe((uint8_t)bus, dev, fn);
        }
    }
    kprintf("[pci] %d device%s\n", ndevs, ndevs == 1 ? "" : "s");
}
