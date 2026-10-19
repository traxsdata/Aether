/* elf.h - loading program images. */
#pragma once
#include <stdint.h>

struct elf_info {
    uint64_t base;  /* where a position-independent image was put, else 0 */
    uint64_t entry;
    uint64_t phdr;  /* user address of the program headers, 0 if not loaded */
    uint64_t phnum;
    uint64_t end;   /* first page after the image */
};

/* Map and copy every loadable segment into pml4. allow_dyn: accept static
 * position-independent executables (loaded at a random base). */
int elf_load(uint64_t pml4, const uint8_t *img, uint64_t size, int allow_dyn, struct elf_info *out);

/* Check an image without mapping it: the same header, segment, W^X and entry
 * rules as elf_load. Used before an app package is installed. */
int elf_check(const uint8_t *img, uint64_t size, int allow_dyn);
