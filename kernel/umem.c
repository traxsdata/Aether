/*
 * umem.c - memory for applications beyond their program image: anonymous,
 * zeroed pages at a randomized base, inside a per-process quota, and never
 * writable and executable at the same time. Untrusted apps can't create
 * executable memory at all, so they can only ever run the code they were
 * shipped with.
 */
#include "../include/abi.h"
#include "cpu.h"
#include "kernel.h"
#include "mm.h"
#include "proc.h"
#include "uaccess.h"

#define MAP_MAX (1ull << 30) /* one call maps at most 1 GiB */

static int prot_ok(uint64_t prot) {
    if (!(prot & PROT_READ) || (prot & ~(uint64_t)(PROT_READ | PROT_WRITE | PROT_EXEC))) return 0;
    if ((prot & PROT_WRITE) && (prot & PROT_EXEC)) return 0;
    if ((prot & PROT_EXEC) && current->trust == TRUST_UNTRUSTED) return 0;
    return 1;
}

static uint64_t pte_flags(uint64_t prot) {
    return PTE_US | ((prot & PROT_WRITE) ? PTE_RW : 0) | ((prot & PROT_EXEC) ? 0 : PTE_NX);
}

/* Is [addr, addr+len) page aligned and inside the mapping area? */
static int range_ok(uint64_t addr, uint64_t len) {
    return !(addr & 0xFFF) && len && len <= MAP_MAX && addr >= USER_MMAP_BASE && addr < USER_MMAP_TOP &&
           len <= USER_MMAP_TOP - addr; /* no wrap past the top of the mapping area */
}

int64_t sys_mem_unmap(uint64_t addr, uint64_t len);

int64_t sys_mem_map(uint64_t len, uint64_t prot) {
    if (!len || len > MAP_MAX || !prot_ok(prot)) return -E_INVAL;
    uint64_t pages = ALIGN_UP(len, PAGE_SIZE) / PAGE_SIZE;
    if (current->mem_pages + current->pt_pages + pages > current->mem_quota_pages) return -E_NOMEM;
    uint64_t base = current->mmap_next;
    if (base + (pages + 1) * PAGE_SIZE > USER_MMAP_TOP) return -E_NOMEM;
    /* A crafted ELF can put its brk dangerously close to the bump area; never
     * overwrite an existing mapping (that would leak its physical page). */
    if (!umem_range_free(base, (pages + 1) * PAGE_SIZE)) return -E_NOMEM;
    for (uint64_t i = 0; i < pages; i++) {
        uint64_t page = pmm_alloc();
        if (!page || vmm_map(current->pml4, base + i * PAGE_SIZE, page, pte_flags(prot) | PTE_QUOTA)) {
            if (page) pmm_free(page);
            current->mem_pages += i;
            if (i) sys_mem_unmap(base, i * PAGE_SIZE);
            return -E_NOMEM;
        }
    }
    current->mem_pages += pages;
    current->mmap_next = base + (pages + 1) * PAGE_SIZE; /* one unmapped guard page between mappings */
    return (int64_t)base;
}

int64_t sys_mem_unmap(uint64_t addr, uint64_t len) {
    len = ALIGN_UP(len, PAGE_SIZE);
    if (!range_ok(addr, len)) return -E_INVAL;
    umem_release(addr, len);
    return 0;
}

int64_t sys_mem_protect(uint64_t addr, uint64_t len, uint64_t prot) {
    len = ALIGN_UP(len, PAGE_SIZE);
    if (!range_ok(addr, len) || !prot_ok(prot)) return -E_INVAL;
    for (uint64_t va = addr; va < addr + len; va += PAGE_SIZE) {
        uint64_t flags;
        if (!vmm_translate(current->pml4, va, &flags) || (flags & PTE_SHARED)) return -E_INVAL;
    }
    for (uint64_t va = addr; va < addr + len; va += PAGE_SIZE) {
        uint64_t *pte = vmm_pte(current->pml4, va, 0);
        *pte = (*pte & (PTE_ADDR | PTE_QUOTA)) | pte_flags(prot) | PTE_P;
        invlpg(va);
    }
    return 0;
}

/* ---- Reserved memory, made on first touch -------------------------------
 * Used by the Linux layer (mmap, brk, thread stacks): reserving address
 * space only writes page-table entries marked PTE_LAZY; the page fault
 * handler and user_range_ok() turn them into zeroed pages when touched,
 * charging the memory quota then. */

#define RESERVE_MAX_PAGES (8ull << 18) /* 8 GiB of reserved address space per process */

static uint64_t lazy_flags(uint64_t prot) {
    return PTE_LAZY | PTE_US | ((prot & PROT_WRITE) ? PTE_RW : 0) | ((prot & PROT_EXEC) ? 0 : PTE_NX) |
           (prot ? 0 : PTE_NOACCESS);
}

/* Next free stretch of the mapping area, with an unmapped guard page after it. */
void umem_claim_va(uint64_t addr, uint64_t len) {
    if (addr >= USER_MMAP_BASE && addr < USER_MMAP_TOP)
        current->mmap_next = MAX(current->mmap_next, addr + len + PAGE_SIZE);
}

uint64_t umem_alloc_va(uint64_t len) {
    uint64_t base = current->mmap_next;
    if (len > MAP_MAX || base + len + PAGE_SIZE > USER_MMAP_TOP) return 0;
    current->mmap_next = base + len + PAGE_SIZE;
    return base;
}

int umem_has_shared(uint64_t addr, uint64_t len) {
    for (uint64_t va = addr; va < addr + len; va += PAGE_SIZE) {
        uint64_t *pte = vmm_pte(current->pml4, va, 0);
        if (pte && (*pte & PTE_SHARED)) return 1;
    }
    return 0;
}

int umem_range_free(uint64_t addr, uint64_t len) {
    for (uint64_t va = addr; va < addr + len; va += PAGE_SIZE) {
        uint64_t *pte = vmm_pte(current->pml4, va, 0);
        if (pte && *pte) return 0;
    }
    return 1;
}

int umem_reserve(uint64_t addr, uint64_t len, uint64_t prot) {
    uint64_t pages = len / PAGE_SIZE;
    if (current->reserved_pages + pages > RESERVE_MAX_PAGES) return -E_NOMEM;
    /* Reserving costs no pages, but it does cost page tables: a sparse
     * reservation (one page per 2 MiB) is almost all table. Those come out
     * of the quota, or reserving could drain the machine's memory. */
    uint64_t tables0 = vmm_user_tables;
    for (uint64_t va = addr; va < addr + len; va += PAGE_SIZE) {
        uint64_t *pte = vmm_pte(current->pml4, va, 1);
        if (!pte || *pte) {
            current->reserved_pages += (va - addr) / PAGE_SIZE;
            current->pt_pages += vmm_user_tables - tables0;
            umem_release(addr, va - addr);
            return pte ? -E_EXIST : -E_NOMEM;
        }
        *pte = lazy_flags(prot);
    }
    current->reserved_pages += pages;
    current->pt_pages += vmm_user_tables - tables0;
    if (current->mem_pages + current->pt_pages > current->mem_quota_pages) {
        umem_release(addr, len);
        return -E_NOMEM;
    }
    return 0;
}

void umem_release(uint64_t addr, uint64_t len) {
    uint64_t end = addr + len;
    for (uint64_t va = addr; va < end; va += PAGE_SIZE) {
        uint64_t *pte = vmm_pte(current->pml4, va, 0);
        if (!pte) { /* no table here: skip to the next 2 MiB */
            va = (va | 0x1FFFFF) + 1 - PAGE_SIZE;
            continue;
        }
        uint64_t e = *pte;
        if (!e || (e & PTE_SHARED)) continue;
        if ((e & PTE_P) || (e & PTE_ADDR)) {
            pmm_free(e & PTE_ADDR);
            if (e & PTE_QUOTA) current->mem_pages--;
        } else if (current->reserved_pages) {
            current->reserved_pages--;
        }
        *pte = 0;
        invlpg(va);
    }
    uint64_t freed = (uint64_t)vmm_prune(current->pml4, addr, end);
    current->pt_pages -= MIN(freed, current->pt_pages);
}

int umem_protect(uint64_t addr, uint64_t len, uint64_t prot) {
    for (uint64_t va = addr; va < addr + len; va += PAGE_SIZE) {
        uint64_t *pte = vmm_pte(current->pml4, va, 0);
        if (!pte || !*pte) return -E_NOMEM;
        if (*pte & PTE_SHARED) return -E_INVAL;
    }
    for (uint64_t va = addr; va < addr + len; va += PAGE_SIZE) {
        uint64_t *pte = vmm_pte(current->pml4, va, 0);
        uint64_t keep = *pte & (PTE_ADDR | PTE_QUOTA);
        if (!(*pte & PTE_ADDR)) *pte = lazy_flags(prot);     /* still untouched */
        else if (!prot) *pte = keep | lazy_flags(0);          /* park the page */
        else *pte = keep | pte_flags(prot) | PTE_P;
        invlpg(va);
    }
    return 0;
}

int umem_fault(uint64_t addr, int write, int exec) {
    if (addr >= USER_TOP) return -1;
    uint64_t *pte = vmm_pte(current->pml4, addr & ~0xFFFull, 0);
    if (!pte) return -1;
    uint64_t e = *pte;
    if (!(e & PTE_LAZY) || (e & (PTE_NOACCESS | PTE_ADDR | PTE_P))) return -1;
    if ((write && !(e & PTE_RW)) || (exec && (e & PTE_NX))) return -1;
    if (current->mem_pages + current->pt_pages >= current->mem_quota_pages) return -2;
    uint64_t page = pmm_alloc();
    if (!page) return -2;
    *pte = page | (e & (PTE_US | PTE_RW | PTE_NX)) | PTE_P | PTE_QUOTA;
    current->mem_pages++;
    if (current->reserved_pages) current->reserved_pages--;
    return 0;
}

int64_t umem_remap(uint64_t old, uint64_t oldlen, uint64_t newlen, int may_move) {
    if (newlen <= oldlen) {
        umem_release(old + newlen, oldlen - newlen);
        return (int64_t)old;
    }
    uint64_t *first = vmm_pte(current->pml4, old, 0);
    if (!first || !*first || umem_has_shared(old, oldlen)) return -E_FAULT;
    uint64_t e = *first;
    uint64_t prot = (e & PTE_NOACCESS) ? 0 : PROT_READ | ((e & PTE_RW) ? PROT_WRITE : 0) | ((e & PTE_NX) ? 0 : PROT_EXEC);
    uint64_t extra = newlen - oldlen;
    /* Grow in place when the space after it is free, keeping the bump
     * allocator's next address beyond it. */
    int in_area = old >= USER_MMAP_BASE && old + newlen + PAGE_SIZE <= USER_MMAP_TOP;
    int is_last = old + oldlen + PAGE_SIZE >= current->mmap_next;
    if (in_area && (is_last || old + newlen + PAGE_SIZE <= current->mmap_next) &&
        umem_range_free(old + oldlen, extra + PAGE_SIZE)) {
        int err = umem_reserve(old + oldlen, extra, prot);
        if (err) return err;
        current->mmap_next = MAX(current->mmap_next, old + newlen + PAGE_SIZE);
        return (int64_t)old;
    }
    if (!may_move) return -E_NOMEM;
    uint64_t dst = umem_alloc_va(newlen);
    if (!dst) return -E_NOMEM;
    if (!umem_range_free(dst, newlen + PAGE_SIZE)) return -E_NOMEM;
    uint64_t tables0 = vmm_user_tables;
    /* Move the page-table entries: the data itself stays where it is. */
    for (uint64_t off = 0; off < oldlen; off += PAGE_SIZE) {
        uint64_t *src = vmm_pte(current->pml4, old + off, 0);
        uint64_t *to = vmm_pte(current->pml4, dst + off, 1);
        if (!to) return -E_NOMEM; /* the part moved so far stays valid at dst */
        *to = src ? *src : 0;
        if (src) *src = 0;
        invlpg(old + off);
    }
    uint64_t made = vmm_user_tables - tables0;
    uint64_t freed = (uint64_t)vmm_prune(current->pml4, old, old + oldlen);
    current->pt_pages -= MIN(freed, current->pt_pages);
    current->pt_pages += made; /* the new destination range has its own tables */
    int err = umem_reserve(dst + oldlen, extra, prot);
    return err ? err : (int64_t)dst;
}
