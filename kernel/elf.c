/*
 * elf.c - loading program images into a fresh address space.
 *
 * Every segment is checked before anything is mapped: it must stay inside
 * user space, inside the file, and never be writable and executable at once.
 * Built-in æther programs are fixed-address executables (ET_EXEC); programs
 * for the Linux layer are static position-independent executables (ET_DYN
 * without an interpreter), which load at a random base address.
 */
#include "elf.h"
#include "kernel.h"
#include "mm.h"
#include "proc.h"

typedef struct {
    uint8_t e_ident[16];
    uint16_t e_type, e_machine;
    uint32_t e_version;
    uint64_t e_entry, e_phoff, e_shoff;
    uint32_t e_flags;
    uint16_t e_ehsize, e_phentsize, e_phnum, e_shentsize, e_shnum, e_shstrndx;
} Elf64_Ehdr;

typedef struct {
    uint32_t p_type, p_flags;
    uint64_t p_offset, p_vaddr, p_paddr, p_filesz, p_memsz, p_align;
} Elf64_Phdr;

#define ET_EXEC 2
#define ET_DYN 3
#define PT_LOAD 1
#define PT_INTERP 3
#define PT_PHDR 6
#define PF_X 1
#define PF_W 2

/* Where position-independent programs go: somewhere in [4 GiB, 132 GiB). */
#define DYN_BASE_MIN 0x0000000100000000ull

static const Elf64_Phdr *phdr(const uint8_t *img, const Elf64_Ehdr *eh, int i) {
    return (const void *)(img + eh->e_phoff + (uint64_t)i * sizeof(Elf64_Phdr));
}

/*
 * Validate an image the way elf_load will. This is deliberately the same
 * rules, checked before an app package is written to the app store: a
 * package that can never run is refused at install time, not at start time.
 */
int elf_check(const uint8_t *img, uint64_t size, int allow_dyn) {
    const Elf64_Ehdr *eh = (const void *)img;
    if (size < sizeof *eh || memcmp(eh->e_ident, "\x7F" "ELF\x02\x01", 6) || eh->e_machine != 62 ||
        eh->e_phentsize != sizeof(Elf64_Phdr) || eh->e_phnum == 0 ||
        eh->e_phoff > size || (uint64_t)eh->e_phnum * sizeof(Elf64_Phdr) > size - eh->e_phoff)
        return -E_INVAL;
    if (eh->e_type != ET_EXEC && !(allow_dyn && eh->e_type == ET_DYN)) return -E_INVAL;
    uint64_t lo = ~0ull, hi = 0;
    for (int i = 0; i < eh->e_phnum; i++) {
        const Elf64_Phdr *ph = phdr(img, eh, i);
        if (ph->p_type == PT_INTERP) return -E_INVAL;
        if (ph->p_type != PT_LOAD || ph->p_memsz == 0) continue;
        if (ph->p_memsz < ph->p_filesz || ph->p_offset + ph->p_filesz > size ||
            ph->p_offset + ph->p_filesz < ph->p_offset || ph->p_vaddr + ph->p_memsz < ph->p_vaddr ||
            ph->p_memsz > (1ull << 32))
            return -E_INVAL;
        if ((ph->p_flags & PF_W) && (ph->p_flags & PF_X)) return -E_INVAL;
        lo = MIN(lo, ph->p_vaddr & ~0xFFFull);
        hi = MAX(hi, ph->p_vaddr + ph->p_memsz);
    }
    if (hi == 0) return -E_INVAL;
    if (eh->e_type == ET_DYN && (lo != 0 || hi > (1ull << 33))) return -E_INVAL;
    if (eh->e_type == ET_EXEC && (eh->e_entry < 0x10000 || eh->e_entry >= USER_MMAP_BASE)) return -E_INVAL;
    if (eh->e_type == ET_DYN && eh->e_entry >= hi) return -E_INVAL;
    int entry_executable = 0;
    for (int i = 0; i < eh->e_phnum; i++) {
        const Elf64_Phdr *ph = phdr(img, eh, i);
        if (ph->p_type != PT_LOAD || !(ph->p_flags & PF_X) || ph->p_memsz == 0) continue;
        if (eh->e_entry >= ph->p_vaddr && eh->e_entry < ph->p_vaddr + ph->p_memsz) {
            entry_executable = 1;
            break;
        }
    }
    return entry_executable ? 0 : -E_INVAL;
}

int elf_load(uint64_t pml4, const uint8_t *img, uint64_t size, int allow_dyn, struct elf_info *out) {
    const Elf64_Ehdr *eh = (const void *)img;
    if (size < sizeof *eh || memcmp(eh->e_ident, "\x7F" "ELF\x02\x01", 6) || eh->e_machine != 62 ||
        eh->e_phentsize != sizeof(Elf64_Phdr) || eh->e_phnum == 0 ||
        eh->e_phoff > size || (uint64_t)eh->e_phnum * sizeof(Elf64_Phdr) > size - eh->e_phoff)
        return -E_INVAL;
    if (eh->e_type != ET_EXEC && !(allow_dyn && eh->e_type == ET_DYN)) return -E_INVAL;

    /* Validate every segment first, and find the image's extent. */
    uint64_t lo = ~0ull, hi = 0, phdr_vaddr = 0;
    for (int i = 0; i < eh->e_phnum; i++) {
        const Elf64_Phdr *ph = phdr(img, eh, i);
        if (ph->p_type == PT_INTERP) return -E_INVAL; /* needs a dynamic loader: not supported */
        if (ph->p_type == PT_PHDR) phdr_vaddr = ph->p_vaddr;
        if (ph->p_type != PT_LOAD || ph->p_memsz == 0) continue;
        if (ph->p_memsz < ph->p_filesz || ph->p_offset + ph->p_filesz > size || ph->p_offset + ph->p_filesz < ph->p_offset ||
            ph->p_vaddr + ph->p_memsz < ph->p_vaddr || ph->p_memsz > (1ull << 32))
            return -E_INVAL;
        if ((ph->p_flags & PF_W) && (ph->p_flags & PF_X)) return -E_INVAL; /* W^X */
        if (!phdr_vaddr && eh->e_phoff >= ph->p_offset && eh->e_phoff < ph->p_offset + ph->p_filesz)
            phdr_vaddr = ph->p_vaddr + (eh->e_phoff - ph->p_offset);
        lo = MIN(lo, ph->p_vaddr & ~0xFFFull);
        hi = MAX(hi, ph->p_vaddr + ph->p_memsz);
    }
    if (hi == 0) return -E_INVAL;

    uint64_t base = 0;
    if (eh->e_type == ET_DYN) {
        if (lo != 0 || hi > (1ull << 33)) return -E_INVAL;
        base = DYN_BASE_MIN + ((random_u64() & 0xFFFF) << 21); /* 16 bits of randomness, 2 MiB aligned */
    }
    if (base + lo < 0x10000 || base + hi >= USER_MMAP_BASE) return -E_INVAL;

    /* iretq loads this value into RIP. It must be a canonical user address
     * inside an executable PT_LOAD segment; otherwise a crafted package can
     * fault in ring 0 or jump somewhere the ELF never mapped. */
    if (eh->e_entry >= USER_TOP) return -E_INVAL;
    uint64_t entry = base + eh->e_entry;
    if (entry < base || entry >= USER_TOP) return -E_INVAL;
    int entry_executable = 0;
    for (int i = 0; i < eh->e_phnum; i++) {
        const Elf64_Phdr *ph = phdr(img, eh, i);
        if (ph->p_type != PT_LOAD || !(ph->p_flags & PF_X) || ph->p_memsz == 0) continue;
        uint64_t start = base + ph->p_vaddr;
        if (entry >= start && entry < start + ph->p_memsz) {
            entry_executable = 1;
            break;
        }
    }
    if (!entry_executable) return -E_INVAL;

    for (int i = 0; i < eh->e_phnum; i++) {
        const Elf64_Phdr *ph = phdr(img, eh, i);
        if (ph->p_type != PT_LOAD || ph->p_memsz == 0) continue;
        uint64_t vaddr = base + ph->p_vaddr;
        uint64_t flags = PTE_US | ((ph->p_flags & PF_W) ? PTE_RW : 0) | ((ph->p_flags & PF_X) ? 0 : PTE_NX);
        for (uint64_t va = vaddr & ~0xFFFull; va < vaddr + ph->p_memsz; va += PAGE_SIZE) {
            uint64_t old, page = vmm_translate(pml4, va, &old) & PTE_ADDR;
            if (page) {
                /* Two segments share this page: it gets the union of their
                 * rights, but never writable + executable. */
                uint64_t merged = (old & ~PTE_ADDR & ~PTE_NX) | (flags & PTE_RW);
                if ((old & PTE_NX) && (flags & PTE_NX)) merged |= PTE_NX;
                if ((merged & PTE_RW) && !(merged & PTE_NX)) return -E_INVAL;
                vmm_map(pml4, va, page, merged & ~PTE_P);
            } else if (!(page = pmm_alloc()) || vmm_map(pml4, va, page, flags)) {
                return -E_NOMEM;
            }
            /* Copy the part of the file that lands in this page. */
            uint64_t seg_lo = MAX(va, vaddr), seg_hi = MIN(va + PAGE_SIZE, vaddr + ph->p_filesz);
            if (seg_hi > seg_lo)
                memcpy((uint8_t *)P2V(page) + (seg_lo - va), img + ph->p_offset + (seg_lo - vaddr), seg_hi - seg_lo);
        }
    }
    out->base = base;
    out->entry = entry;
    out->phdr = phdr_vaddr ? base + phdr_vaddr : 0;
    out->phnum = eh->e_phnum;
    out->end = ALIGN_UP(base + hi, PAGE_SIZE);
    return 0;
}
