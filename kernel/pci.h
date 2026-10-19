/* pci.h - PCI configuration space (mechanism #1) and device enumeration. */
#pragma once
#include <stdint.h>

struct pci_dev {
    uint8_t bus, dev, fn;
    uint16_t vendor, device;
    uint8_t class, subclass, progif;
};

#define PCI_CMD 0x04
#define PCI_CMD_IO (1u << 0)
#define PCI_CMD_MEM (1u << 1)
#define PCI_CMD_BUS_MASTER (1u << 2)
#define PCI_CMD_INTX_DISABLE (1u << 10)
#define PCI_ANY 0xFF /* wildcard for pci_find_class's subclass / progif */

void pci_init(void); /* scan every bus/device/function and log what's there */
int pci_count(void);
struct pci_dev *pci_get(int idx);
/* The idx-th device (0-based) with this class/subclass/progif, or 0. */
struct pci_dev *pci_find_class(uint8_t class, uint8_t subclass, uint8_t progif, int idx);

/* Offsets are in bytes; 32-bit accesses need a dword-aligned offset, 16-bit a word-aligned one. */
uint32_t pci_read32(const struct pci_dev *d, uint8_t off);
uint16_t pci_read16(const struct pci_dev *d, uint8_t off);
void pci_write32(const struct pci_dev *d, uint8_t off, uint32_t v);
void pci_write16(const struct pci_dev *d, uint8_t off, uint16_t v);

/* Base address of BAR n with the flag bits stripped (a 64-bit memory BAR
 * combines n and n+1). For an I/O BAR, the port base. 0 if unimplemented.
 * pci_bar_ex also reports whether the BAR is I/O space. */
uint64_t pci_bar(const struct pci_dev *d, int n);
uint64_t pci_bar_ex(const struct pci_dev *d, int n, int *is_io);
/* Turn on memory-space decoding and bus mastering (DMA); mask INTx (we poll). */
void pci_enable_bus_master_mem(const struct pci_dev *d);
