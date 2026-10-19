/*
 * power.c - minimal ACPI table walk for a clean shutdown.
 *
 * The loader already hands us the ACPI 2.0+ RSDP. We do not need an AML
 * interpreter for this: the FADT gives the PM1 control port, and S5 uses the
 * conventional SLP_TYP value 5. If firmware exposes no usable FADT (or the
 * write does not power off), fall back to QEMU's ACPI shutdown ports and
 * then halt.
 */
#include "power.h"
#include "../include/bootinfo.h"
#include "cpu.h"
#include "kernel.h"
#include "mm.h"

#define ACPI_RSDP_SIG "RSD PTR "
#define ACPI_XSDT_SIG "XSDT"
#define ACPI_RSDT_SIG "RSDT"
#define ACPI_FADT_SIG "FACP"

static uint64_t rsdp_phys;

typedef struct {
    char sig[4];
    uint32_t length;
    uint8_t revision, checksum;
    char oem[6];
    char oem_table[8];
    uint32_t oem_revision;
    uint32_t creator;
    uint32_t creator_revision;
} __attribute__((packed)) acpi_table;

static int sum_ok(const void *p, size_t n) {
    const uint8_t *b = p;
    uint8_t sum = 0;
    for (size_t i = 0; i < n; i++) sum = (uint8_t)(sum + b[i]);
    return sum == 0;
}

static const acpi_table *find_fadt(void) {
    if (!rsdp_phys) return 0;
    const uint8_t *rsdp = P2V(rsdp_phys);
    if (memcmp(rsdp, ACPI_RSDP_SIG, 8) || !sum_ok(rsdp, 20)) return 0;
    int xsdt = rsdp[15] >= 2;
    uint64_t table_phys = 0;
    if (xsdt) {
        table_phys = *(const uint64_t *)(rsdp + 24);
        if (!sum_ok(rsdp, 36)) return 0;
    } else {
        table_phys = *(const uint32_t *)(rsdp + 16);
    }
    if (!table_phys) return 0;
    const acpi_table *root = P2V(table_phys);
    if (memcmp(root->sig, xsdt ? ACPI_XSDT_SIG : ACPI_RSDT_SIG, 4) || root->length < 36 ||
        root->length > (1u << 20) || !sum_ok(root, root->length))
        return 0;
    size_t entry = xsdt ? 8 : 4;
    size_t count = (root->length - 36) / entry;
    for (size_t i = 0; i < count; i++) {
        uint64_t p = xsdt ? ((const uint64_t *)((const uint8_t *)root + 36))[i]
                          : ((const uint32_t *)((const uint8_t *)root + 36))[i];
        if (!p) continue;
        const acpi_table *t = P2V(p);
        if (memcmp(t->sig, ACPI_FADT_SIG, 4)) continue;
        if (t->length < 76 || t->length > (1u << 20) || !sum_ok(t, t->length)) return 0;
        return t;
    }
    return 0;
}

static inline void outw(uint16_t port, uint16_t value) {
    __asm__ volatile("outw %0, %1" : : "a"(value), "Nd"(port));
}

void power_init(uint64_t acpi_rsdp_phys) {
    rsdp_phys = acpi_rsdp_phys;
    kprintf("[power] ACPI %s\n", acpi_rsdp_phys ? "RSDP found" : "RSDP missing");
}

void system_shutdown(void) {
    kprintf("[power] shutting down\n");
    const acpi_table *fadt = find_fadt();
    if (fadt) {
        uint32_t pm1a = *(const uint32_t *)((const uint8_t *)fadt + 44); /* PM1a_CNT_BLK */
        uint32_t pm1b = *(const uint32_t *)((const uint8_t *)fadt + 48); /* PM1b_CNT_BLK */
        uint8_t len = ((const uint8_t *)fadt)[69];                       /* PM1_CNT_LEN */
        /* S5: SLP_TYP 5 in bits 10..12, SLP_EN set. */
        uint16_t val = (uint16_t)((5u << 10) | (1u << 13));
        if (pm1a && len >= 2) outw((uint16_t)pm1a, val);
        if (pm1b && len >= 2) outw((uint16_t)pm1b, val);
    }
    /* QEMU/Bochs ACPI shutdown ports, then an older QEMU port. */
    outw(0x604, 0x2000);
    outw(0xB004, 0x2000);
    cli();
    for (;;) hlt();
}
