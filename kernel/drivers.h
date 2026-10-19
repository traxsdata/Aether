/* drivers.h - Rust kernel drivers (NVMe in 0.8) and the shared C helpers. */
#pragma once
#include <stdint.h>

/* Initialise every Rust kernel driver. Call after pci_init() and the AHCI
 * driver, before storage_init(). */
void drivers_init(void);

/* Monotonic milliseconds from the PIT; safe with interrupts off. */
uint64_t aether_pit_ms(void);
