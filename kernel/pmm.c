/*
 * pmm.c - physical page allocator: one bit per 4 KiB page.
 *
 * Usable memory is whatever UEFI called conventional or boot-services
 * memory (boot services are gone by now). Loader data (the boot info, the
 * memory map, the loader's page tables) and the kernel image stay reserved.
 * The first 1 MiB is never handed out, so a null pointer is never a valid
 * page.
 */
#include "kernel.h"
#include "mm.h"

static uint8_t *bitmap; /* 1 = used */
static uint64_t npages, nfree, total_usable, hint;

static inline void set_used(uint64_t p) { bitmap[p / 8] |= (uint8_t)(1 << (p % 8)); }
static inline void set_free(uint64_t p) { bitmap[p / 8] &= (uint8_t)~(1 << (p % 8)); }
static inline int is_used(uint64_t p) { return bitmap[p / 8] & (1 << (p % 8)); }

static int usable(uint32_t type) {
    return type == MEM_CONVENTIONAL || type == MEM_BOOT_SERVICES_CODE || type == MEM_BOOT_SERVICES_DATA ||
           type == MEM_LOADER_CODE;
}

#define FOR_EACH_DESC(bi, d)                                                         \
    for (uint64_t _o = 0; _o < (bi)->mmap_size; _o += (bi)->mmap_desc_size)          \
        for (const struct mem_desc *d = P2V((bi)->mmap + _o); d; d = 0)

void pmm_init(const struct bootinfo *bi) {
    uint64_t top = 0;
    FOR_EACH_DESC(bi, d) {
        if (usable(d->type)) top = MAX(top, d->phys_start + d->num_pages * PAGE_SIZE);
    }
    npages = top / PAGE_SIZE;
    uint64_t bitmap_bytes = ALIGN_UP((npages + 7) / 8, PAGE_SIZE);

    /* Put the bitmap in the first usable region above 1 MiB that fits it. */
    uint64_t bitmap_phys = 0;
    FOR_EACH_DESC(bi, d) {
        uint64_t start = MAX(d->phys_start, 0x100000ull);
        uint64_t end = d->phys_start + d->num_pages * PAGE_SIZE;
        if (!bitmap_phys && usable(d->type) && end > start && end - start >= bitmap_bytes) bitmap_phys = start;
    }
    if (!bitmap_phys) panic("no room for the physical memory bitmap");
    bitmap = P2V(bitmap_phys);
    memset(bitmap, 0xFF, bitmap_bytes);

    FOR_EACH_DESC(bi, d) {
        if (!usable(d->type)) continue;
        for (uint64_t i = 0; i < d->num_pages; i++) {
            uint64_t p = d->phys_start / PAGE_SIZE + i;
            if (p < 256) continue; /* first 1 MiB */
            set_free(p);
            nfree++;
        }
    }
    for (uint64_t p = bitmap_phys / PAGE_SIZE; p < (bitmap_phys + bitmap_bytes) / PAGE_SIZE; p++) {
        if (!is_used(p)) nfree--;
        set_used(p);
    }
    /* The kernel image was allocated as loader data, so it's already excluded,
     * but be explicit in case a future loader changes that. */
    for (uint64_t p = bi->kernel_start / PAGE_SIZE; p < bi->kernel_end / PAGE_SIZE && p < npages; p++) {
        if (!is_used(p)) nfree--;
        set_used(p);
    }
    total_usable = nfree;
    hint = 256;
}

uint64_t pmm_alloc(void) { return pmm_alloc_contig(1); }

uint64_t pmm_alloc_contig(size_t n) {
    if (n == 0 || n > nfree) return 0;
    for (int pass = 0; pass < 2; pass++) {
        uint64_t run = 0;
        for (uint64_t p = pass ? 256 : hint; p < npages; p++) {
            run = is_used(p) ? 0 : run + 1;
            if (run == n) {
                uint64_t first = p - n + 1;
                for (uint64_t q = first; q <= p; q++) set_used(q);
                nfree -= n;
                hint = p + 1;
                memset(P2V(first * PAGE_SIZE), 0, n * PAGE_SIZE);
                return first * PAGE_SIZE;
            }
        }
    }
    return 0;
}

void pmm_free_contig(uint64_t phys, size_t n) {
    for (size_t i = 0; i < n; i++) {
        uint64_t p = phys / PAGE_SIZE + i;
        if (p < 256 || p >= npages || !is_used(p)) panic("pmm: bad free of page %lx", p * PAGE_SIZE);
        set_free(p);
        nfree++;
    }
    if (phys / PAGE_SIZE < hint) hint = phys / PAGE_SIZE;
}

void pmm_free(uint64_t phys) { pmm_free_contig(phys, 1); }
uint64_t pmm_total_pages(void) { return total_usable; }
uint64_t pmm_free_pages(void) { return nfree; }
