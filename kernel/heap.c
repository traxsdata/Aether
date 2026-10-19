/*
 * heap.c - kernel heap: first-fit free list over one contiguous region,
 * with neighbouring free blocks merged on kfree. Each block carries a
 * header with a magic value so double frees and stray pointers are caught.
 */
#include "kernel.h"
#include "mm.h"

#define HEAP_PAGES 4096 /* 16 MiB */
#define MAGIC_USED 0xA110CA7EDB10C000ull
#define MAGIC_FREE 0xF4EEB10C4F4EEB10ull

struct block {
    uint64_t magic;
    uint64_t size; /* bytes including this header, multiple of 16 */
    struct block *next; /* free list, sorted by address */
    uint64_t pad;
};

static struct block *free_list;

void heap_init(void) {
    uint64_t phys = pmm_alloc_contig(HEAP_PAGES);
    if (!phys) panic("heap: no memory");
    free_list = P2V(phys);
    free_list->magic = MAGIC_FREE;
    free_list->size = HEAP_PAGES * PAGE_SIZE;
    free_list->next = 0;
}

void *kmalloc(size_t n) {
    uint64_t need = ALIGN_UP(n + sizeof(struct block), 16);
    for (struct block **pp = &free_list; *pp; pp = &(*pp)->next) {
        struct block *b = *pp;
        if (b->size < need) continue;
        if (b->size - need >= 64) { /* split */
            struct block *rest = (struct block *)((uint8_t *)b + need);
            rest->magic = MAGIC_FREE;
            rest->size = b->size - need;
            rest->next = b->next;
            b->size = need;
            *pp = rest;
        } else {
            *pp = b->next;
        }
        b->magic = MAGIC_USED;
        b->next = 0;
        memset(b + 1, 0, b->size - sizeof *b);
        return b + 1;
    }
    return 0;
}

void kfree(void *p) {
    if (!p) return;
    struct block *b = (struct block *)p - 1;
    if (b->magic != MAGIC_USED) panic("heap: bad kfree(%p)", p);
    b->magic = MAGIC_FREE;
    struct block **pp = &free_list;
    while (*pp && *pp < b) pp = &(*pp)->next;
    b->next = *pp;
    *pp = b;
    /* Merge with the following block, then with the preceding one. */
    if (b->next && (uint8_t *)b + b->size == (uint8_t *)b->next) {
        b->size += b->next->size;
        b->next = b->next->next;
    }
    if (pp != &free_list) {
        struct block *prev = (struct block *)((uint8_t *)pp - offsetof(struct block, next));
        if ((uint8_t *)prev + prev->size == (uint8_t *)b) {
            prev->size += b->size;
            prev->next = b->next;
        }
    }
}
