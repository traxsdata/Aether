/* mm.h - physical pages, page tables, kernel heap. */
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "../include/bootinfo.h"

/* ---- Physical memory (pmm.c) ------------------------------------------- */
void pmm_init(const struct bootinfo *bi);
uint64_t pmm_alloc(void);              /* one zeroed page, or 0 */
uint64_t pmm_alloc_contig(size_t n);   /* n zeroed contiguous pages, or 0 */
void pmm_free(uint64_t phys);
void pmm_free_contig(uint64_t phys, size_t n);
uint64_t pmm_total_pages(void);
uint64_t pmm_free_pages(void);

/* ---- Virtual memory (vmm.c) -------------------------------------------- */
#define PTE_P (1ull << 0)
#define PTE_RW (1ull << 1)
#define PTE_US (1ull << 2)
#define PTE_PWT (1ull << 3) /* with our PAT setup: write-combining */
#define PTE_PCD (1ull << 4) /* PCD|PWT: uncached (MMIO) */
#define PTE_PS (1ull << 7)
#define PTE_G (1ull << 8)
#define PTE_SHARED (1ull << 9) /* software bit: page isn't owned by this address space */
/* Software bits. In a not-present user entry, LAZY marks address space that is
 * reserved: the low flag bits say what a page there may do, and the page
 * itself is made on first touch (umem.c). NOACCESS: PROT_NONE; such an entry
 * may still hold a page, kept but unreachable. */
#define PTE_LAZY (1ull << 10)
#define PTE_NOACCESS (1ull << 11)
#define PTE_QUOTA (1ull << 52) /* counted against the process's memory quota */
#define PTE_NX (1ull << 63)
#define PTE_ADDR 0x000FFFFFFFFFF000ull

#define USER_TOP 0x0000800000000000ull /* user addresses are below this */

void vmm_init(const struct bootinfo *bi);
uint64_t vmm_kernel_pml4(void);
uint64_t vmm_new_address_space(void);          /* PML4 sharing the kernel half */
void vmm_destroy_address_space(uint64_t pml4); /* frees owned user pages + tables */
int vmm_map(uint64_t pml4, uint64_t virt, uint64_t phys, uint64_t flags); /* 0 on success */
void vmm_unmap(uint64_t pml4, uint64_t virt); /* doesn't free the page */
/* Physical address + flags of a 4K mapping, or 0 if unmapped. */
uint64_t vmm_translate(uint64_t pml4, uint64_t virt, uint64_t *flags);
uint64_t vmm_user_pages(uint64_t pml4);
/* The 4K entry for virt (creating the tables on the way if `create`), or 0. */
uint64_t *vmm_pte(uint64_t pml4, uint64_t virt, int create);
/* Free page tables left empty in [start, end); returns 1 if any were. */
int vmm_prune(uint64_t pml4, uint64_t start, uint64_t end); /* -> how many tables it freed */
/* User page tables made so far, all address spaces together: umem.c charges
 * the ones a reservation creates to the process's quota. */
extern uint64_t vmm_user_tables;

/* ---- Kernel heap (heap.c) ---------------------------------------------- */
void heap_init(void);
void *kmalloc(size_t n);  /* zeroed, 16-byte aligned */
void kfree(void *p);
