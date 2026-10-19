/*
 * gdt.c - our own GDT and TSS.
 *
 * Layout is fixed by the syscall/sysret MSR conventions: kernel code, kernel
 * data, then user data before user code. The TSS gives the CPU a kernel
 * stack to switch to on interrupts from user mode (rsp0) and dedicated
 * known-good stacks for double faults and NMIs (IST1/IST2), so even a
 * kernel stack overflow produces a readable crash screen.
 */
#include "gdt.h"
#include "kernel.h"

struct __attribute__((packed)) tss {
    uint32_t reserved0;
    uint64_t rsp0, rsp1, rsp2;
    uint64_t reserved1;
    uint64_t ist[7];
    uint64_t reserved2;
    uint16_t reserved3, iomap_base;
};

static struct tss tss;
static uint64_t gdt[7];
static uint8_t ist_stacks[2][16384] __attribute__((aligned(16)));

void gdt_init(void) {
    gdt[0] = 0;
    gdt[1] = 0x00AF9A000000FFFFull; /* kernel code: present, ring 0, executable, long mode */
    gdt[2] = 0x00CF92000000FFFFull; /* kernel data */
    gdt[3] = 0x00CFF2000000FFFFull; /* user data: ring 3 */
    gdt[4] = 0x00AFFA000000FFFFull; /* user code: ring 3, long mode */

    tss.iomap_base = sizeof tss; /* no I/O permission bitmap: user mode gets no port access */
    tss.ist[0] = (uint64_t)&ist_stacks[0][sizeof ist_stacks[0]];
    tss.ist[1] = (uint64_t)&ist_stacks[1][sizeof ist_stacks[1]];
    uint64_t base = (uint64_t)&tss, limit = sizeof tss - 1;
    gdt[5] = (limit & 0xFFFF) | ((base & 0xFFFFFF) << 16) | (0x89ull << 40) | (((limit >> 16) & 0xF) << 48) |
             (((base >> 24) & 0xFF) << 56);
    gdt[6] = base >> 32;

    struct __attribute__((packed)) {
        uint16_t limit;
        uint64_t base;
    } gdtr = {sizeof gdt - 1, (uint64_t)gdt};

    __asm__ volatile(
        "lgdt %0\n"
        "mov %1, %%ds\n mov %1, %%es\n mov %1, %%ss\n"
        "xor %%eax, %%eax\n mov %%ax, %%fs\n mov %%ax, %%gs\n"
        "pushq %2\n lea 1f(%%rip), %%rax\n pushq %%rax\n lretq\n1:\n"
        "ltr %w3\n"
        :
        : "m"(gdtr), "r"((uint64_t)SEL_KDATA), "i"(SEL_KCODE), "r"((uint64_t)SEL_TSS)
        : "rax", "memory");
}

void tss_set_kernel_stack(uint64_t rsp0) { tss.rsp0 = rsp0; }
