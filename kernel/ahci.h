/* ahci.h - polling AHCI (SATA) driver. Drives register as sata0, sata1, ... */
#pragma once

/* Find every AHCI controller (PCI class 01/06/01), bring up its SATA
 * drives and register them as block devices. Also logs (but doesn't drive)
 * NVMe controllers. Needs pci_init() and the PIT running first. */
void ahci_init(void);

/* Non-destructive read/write/compare on the last 8 sectors of each SATA
 * drive whose model string contains "QEMU" (other disks are skipped).
 * Returns 0 if every tested drive passed or none was tested. */
int ahci_selftest(void);
