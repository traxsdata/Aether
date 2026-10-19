/* power.h - ACPI power management (shutdown today; suspend is future work). */
#pragma once
#include <stdint.h>

void power_init(uint64_t acpi_rsdp_phys);
void system_shutdown(void) __attribute__((noreturn));
