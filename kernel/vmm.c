/*
 * vmm.c - page tables.
 *
 * The kernel builds its own tables instead of trusting the loader's:
 *   - higher-half direct map of RAM: read/write, never executable
 *   - kernel .text read+execute, .rodata read-only, .data/.bss read/write, no-exec
 *     (so no kernel page is ever both writable and executable: W^X)
 *   - framebuffer write-combining (much faster on real hardware and VMware)
 *   - no identity map: address 0 and the whole lower half belong to user processes
 * Kernel pages are supervisor-only, so user code can't touch them; with
 * SMEP/SMAP the kernel also can't run user code or touch user data by accident.
 */
#include "cpu.h"
#include "kernel.h"
#include "mm.h"

extern char __text_start[], __text_end[], __rodata_start[], __rodata_end[], __data_start[], __kernel_end[];

static uint64_t kernel_pml4;
uint64_t vmm_user_tables;

static uint64_t *table(uint64_t phys) { return P2V(phys); }

/* Return the next-level table for entry idx, creating it if needed. */
static uint64_t *next_level(uint64_t *t, int idx, int user) {
    if (!(t[idx] & PTE_P)) {
        uint64_t page = pmm_alloc();
        if (!page) return 0;
        t[idx] = page | PTE_P | PTE_RW | (user ? PTE_US : 0);
        if (user) vmm_user_tables++;
    }
    if (t[idx] & PTE_PS) panic("vmm: 4K mapping inside a large page");
    return table(t[idx] & PTE_ADDR);
}

int vmm_map(uint64_t pml4, uint64_t virt, uint64_t phys, uint64_t flags) {
    int user = virt < USER_TOP;
    uint64_t *l4 = table(pml4), *l3, *l2, *l1;
    if (!(l3 = next_level(l4, (virt >> 39) & 511, user))) return -1;
    if (!(l2 = next_level(l3, (virt >> 30) & 511, user))) return -1;
    if (!(l1 = next_level(l2, (virt >> 21) & 511, user))) return -1;
    l1[(virt >> 12) & 511] = (phys & PTE_ADDR) | flags | PTE_P;
    return 0;
}

void vmm_unmap(uint64_t pml4, uint64_t virt) {
    uint64_t *t = table(pml4);
    for (int shift = 39; shift > 12; shift -= 9) {
        uint64_t e = t[(virt >> shift) & 511];
        if (!(e & PTE_P) || (e & PTE_PS)) return;
        t = table(e & PTE_ADDR);
    }
    t[(virt >> 12) & 511] = 0;
    invlpg(virt);
}

static void map_2m(uint64_t pml4, uint64_t virt, uint64_t phys, uint64_t flags) {
    uint64_t *l3 = next_level(table(pml4), (virt >> 39) & 511, 0);
    uint64_t *l2 = l3 ? next_level(l3, (virt >> 30) & 511, 0) : 0;
    if (!l2) panic("vmm: out of memory building the direct map");
    uint64_t *e = &l2[(virt >> 21) & 511];
    if ((*e & PTE_P) && !(*e & PTE_PS)) return; /* already split into 4K pages: keep them */
    *e = (phys & ~0x1FFFFFull) | flags | PTE_P | PTE_PS;
}

static void map_hhdm_range(uint64_t start, uint64_t end, uint64_t flags) {
    for (uint64_t pa = start & ~0x1FFFFFull; pa < end; pa += 0x200000) map_2m(kernel_pml4, HHDM_BASE + pa, pa, flags);
}

/* The loader put the kernel image wherever there was room. */
static uint64_t kernel_phys, kernel_virt;

static void map_kernel_range(char *start, char *end, uint64_t flags) {
    for (uint64_t va = (uint64_t)start & ~0xFFFull; va < (uint64_t)end; va += PAGE_SIZE)
        if (vmm_map(kernel_pml4, va, va - kernel_virt + kernel_phys, flags))
            panic("vmm: out of memory mapping the kernel");
}

uint64_t vmm_translate(uint64_t pml4, uint64_t virt, uint64_t *flags) {
    uint64_t *t = table(pml4);
    for (int shift = 39; shift >= 12; shift -= 9) {
        uint64_t e = t[(virt >> shift) & 511];
        if (!(e & PTE_P)) return 0;
        if (shift == 12 || (shift == 21 && (e & PTE_PS))) {
            if (flags) *flags = e;
            uint64_t mask = shift == 12 ? 0xFFF : 0x1FFFFF;
            return (e & PTE_ADDR & ~mask) | (virt & mask);
        }
        t = table(e & PTE_ADDR);
    }
    return 0;
}

void vmm_init(const struct bootinfo *bi) {
    /* PAT entry 1 (selected by PWT alone) becomes write-combining. */
    wrmsr(MSR_PAT, 0x0007040600070106ull);
    wrmsr(MSR_EFER, rdmsr(MSR_EFER) | EFER_NXE);

    kernel_phys = bi->kernel_start;
    kernel_virt = bi->kernel_virt;
    kernel_pml4 = pmm_alloc();
    if (!kernel_pml4) panic("vmm: no memory for the kernel page table");

    for (uint64_t o = 0; o < bi->mmap_size; o += bi->mmap_desc_size) {
        const struct mem_desc *d = P2V(bi->mmap + o);
        if (mem_type_is_mapped(d->type))
            map_hhdm_range(d->phys_start, d->phys_start + d->num_pages * PAGE_SIZE, PTE_RW | PTE_NX | PTE_G);
    }
    map_hhdm_range(bi->fb.base, bi->fb.base + bi->fb.size, PTE_RW | PTE_NX | PTE_G | PTE_PWT);

    /* Local APIC registers: uncached MMIO. */
    uint64_t apic = rdmsr(MSR_APIC_BASE) & 0xFFFFF000ull;
    if (!vmm_translate(kernel_pml4, HHDM_BASE + apic, 0))
        vmm_map(kernel_pml4, HHDM_BASE + apic, apic, PTE_RW | PTE_NX | PTE_G | PTE_PCD | PTE_PWT);

    map_kernel_range(__text_start, __text_end, PTE_G);                     /* r-x */
    map_kernel_range(__rodata_start, __rodata_end, PTE_G | PTE_NX);        /* r-- */
    map_kernel_range(__data_start, __kernel_end, PTE_G | PTE_NX | PTE_RW); /* rw- */

    write_cr0(read_cr0() | CR0_WP); /* the kernel honours read-only pages too */
    write_cr4(read_cr4() | CR4_PGE);
    write_cr3(kernel_pml4);
}

uint64_t vmm_kernel_pml4(void) { return kernel_pml4; }

uint64_t vmm_new_address_space(void) {
    uint64_t pml4 = pmm_alloc();
    if (!pml4) return 0;
    memcpy(table(pml4) + 256, table(kernel_pml4) + 256, 256 * sizeof(uint64_t));
    return pml4;
}

static uint64_t walk_user(uint64_t pml4, int free_pages) {
    uint64_t count = 0, *l4 = table(pml4);
    for (int i = 0; i < 256; i++) {
        if (!(l4[i] & PTE_P)) continue;
        uint64_t *l3 = table(l4[i] & PTE_ADDR);
        for (int j = 0; j < 512; j++) {
            if (!(l3[j] & PTE_P)) continue;
            uint64_t *l2 = table(l3[j] & PTE_ADDR);
            for (int k = 0; k < 512; k++) {
                if (!(l2[k] & PTE_P)) continue;
                uint64_t *l1 = table(l2[k] & PTE_ADDR);
                for (int m = 0; m < 512; m++) {
                    /* A page parked by PROT_NONE is still owned. */
                    int owned = (l1[m] & PTE_P) || ((l1[m] & PTE_LAZY) && (l1[m] & PTE_ADDR));
                    if (!owned || (l1[m] & PTE_SHARED)) continue;
                    count++;
                    if (free_pages) pmm_free(l1[m] & PTE_ADDR);
                }
                if (free_pages) pmm_free(l2[k] & PTE_ADDR);
            }
            if (free_pages) pmm_free(l3[j] & PTE_ADDR);
        }
        if (free_pages) pmm_free(l4[i] & PTE_ADDR);
    }
    return count;
}

void vmm_destroy_address_space(uint64_t pml4) {
    walk_user(pml4, 1);
    pmm_free(pml4);
}

uint64_t vmm_user_pages(uint64_t pml4) { return walk_user(pml4, 0); }

uint64_t *vmm_pte(uint64_t pml4, uint64_t virt, int create) {
    uint64_t *t = table(pml4);
    for (int shift = 39; shift > 12; shift -= 9) {
        int idx = (virt >> shift) & 511;
        if (!(t[idx] & PTE_P)) {
            if (!create) return 0;
            t = next_level(t, idx, virt < USER_TOP);
            if (!t) return 0;
            continue;
        }
        if (t[idx] & PTE_PS) return 0;
        t = table(t[idx] & PTE_ADDR);
    }
    return &t[(virt >> 12) & 511];
}

int vmm_prune(uint64_t pml4, uint64_t start, uint64_t end) {
    int freed = 0;
    for (uint64_t va = start & ~0x1FFFFFull; va < end; va += 0x200000) {
        uint64_t *l3, *l2, *l1, *l4 = table(pml4);
        uint64_t e4 = l4[(va >> 39) & 511];
        if (!(e4 & PTE_P)) continue;
        l3 = table(e4 & PTE_ADDR);
        uint64_t e3 = l3[(va >> 30) & 511];
        if (!(e3 & PTE_P) || (e3 & PTE_PS)) continue;
        l2 = table(e3 & PTE_ADDR);
        uint64_t *e2 = &l2[(va >> 21) & 511];
        if (!(*e2 & PTE_P) || (*e2 & PTE_PS)) continue;
        l1 = table(*e2 & PTE_ADDR);
        int empty = 1;
        for (int i = 0; i < 512 && empty; i++) empty = l1[i] == 0;
        if (!empty) continue;
        pmm_free(*e2 & PTE_ADDR);
        *e2 = 0;
        freed++;
    }
    if (freed) write_cr3(read_cr3()); /* drop cached paging structures */
    return freed;
}
