/*
 * bootinfo.h - the contract between the UEFI bootloader and the kernel.
 *
 * The bootloader fills one of these in, exits UEFI boot services, switches
 * to its own page tables and jumps to the kernel entry point with the
 * *physical* address of this struct in RDI (System V ABI).
 *
 * Page tables the loader sets up (all writable, executable, supervisor-only;
 * the kernel replaces them with strict ones as its first act):
 *   - identity map of all RAM and the framebuffer (2 MiB pages)
 *   - the same ranges again at HHDM_BASE ("higher-half direct map")
 *   - the kernel image at its linked virtual address (4 KiB pages)
 */
#pragma once
#include <stdint.h>

#define BOOTINFO_MAGIC 0x4F4F42534F57454Eull /* ASCII "NEWOSBOO" in memory */

#define HHDM_BASE 0xFFFF800000000000ull       /* physical 0 is mapped here */
#define KERNEL_VIRT_BASE 0xFFFFFFFF80000000ull /* the kernel is linked above this */

enum {
    FB_FORMAT_RGBX = 0, /* byte order R,G,B,X  (PixelRedGreenBlueReserved8BitPerColor) */
    FB_FORMAT_BGRX = 1, /* byte order B,G,R,X  (PixelBlueGreenRedReserved8BitPerColor) */
};

struct framebuffer {
    uint64_t base;   /* physical address of pixel (0,0) */
    uint64_t size;   /* bytes */
    uint32_t width;  /* visible pixels per row */
    uint32_t height; /* rows */
    uint32_t pitch;  /* pixels per scanline (>= width) */
    uint32_t format; /* FB_FORMAT_* */
};

/* Memory map entries are copied verbatim from UEFI (EFI_MEMORY_DESCRIPTOR). */
struct mem_desc {
    uint32_t type;
    uint32_t pad;
    uint64_t phys_start;
    uint64_t virt_start;
    uint64_t num_pages; /* 4 KiB pages */
    uint64_t attribute;
};

/* UEFI memory types the kernel cares about. */
enum {
    MEM_RESERVED = 0,
    MEM_LOADER_CODE = 1,
    MEM_LOADER_DATA = 2,
    MEM_BOOT_SERVICES_CODE = 3,
    MEM_BOOT_SERVICES_DATA = 4,
    MEM_RUNTIME_CODE = 5,
    MEM_RUNTIME_DATA = 6,
    MEM_CONVENTIONAL = 7,
    MEM_UNUSABLE = 8,
    MEM_ACPI_RECLAIM = 9,
    MEM_ACPI_NVS = 10,
    MEM_MMIO = 11,
    MEM_MMIO_PORT = 12,
    MEM_PAL_CODE = 13,
    MEM_PERSISTENT = 14,
};

/* Which memory types the loader maps (identity + HHDM). */
static inline int mem_type_is_mapped(uint32_t t) {
    return (t >= MEM_LOADER_CODE && t <= MEM_ACPI_NVS) || t == MEM_PERSISTENT;
}

/* App packages the loader found in \apps on the boot partition, read into
 * loader-data memory (which the kernel never reuses). */
#define BOOT_MODULES_MAX 32
struct boot_module {
    char name[32]; /* file name without extension, ASCII */
    uint64_t phys; /* where its bytes are */
    uint64_t size;
};

/* boot_flags */
#define BOOT_FLAG_NETBOOT (1u << 0) /* the netboot ISO asked for the installer */
#define BOOT_FLAG_INSTALLED (1u << 1) /* booted from an A/B system partition */

#define BOOT_NO_SLOT 0xFFFFFFFFu

struct bootinfo {
    uint64_t magic; /* BOOTINFO_MAGIC */
    struct framebuffer fb;
    uint64_t mmap;           /* physical address of the first descriptor */
    uint64_t mmap_size;      /* total bytes */
    uint64_t mmap_desc_size; /* stride between descriptors (may exceed sizeof(struct mem_desc)) */
    uint64_t acpi_rsdp;      /* physical address of the ACPI 2.0+ RSDP, or 0 */
    uint64_t kernel_start;   /* physical range the kernel image occupies */
    uint64_t kernel_end;
    uint64_t kernel_virt;    /* virtual address that kernel_start is mapped at */
    uint32_t nmodules;
    uint32_t boot_flags;     /* BOOT_FLAG_* */
    uint32_t boot_slot;      /* A/B slot the loader booted, or BOOT_NO_SLOT */
    uint8_t boot_disk_guid[16]; /* GPT disk GUID that supplied the system */
    struct boot_module modules[BOOT_MODULES_MAX];
};
