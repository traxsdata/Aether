/* apps.h - the program table: built-in programs and installed packages. */
#pragma once
#include <stdint.h>

struct program {
    const char *name;
    const char *title; /* shown in the dock, or 0 if not launchable from there */
    const uint8_t *image;
    uint64_t size;
    uint32_t caps; /* the most this program is ever granted */
    uint32_t trust; /* TRUST_*: decides its system call filter and memory quota */
    uint32_t mem_quota_mib;
    uint32_t abi;        /* ABI_*: æther's system calls, or the Linux layer */
    uint32_t icon_color; /* dock icon colour, for apps without an icon image */
    const uint8_t *icon; /* RGBA8888 icon in the package, or 0 */
    uint32_t icon_w, icon_h;
    const char *version; /* installed apps: the app's own version, and where it came from */
    const char *source;
    uint8_t sha256[32]; /* installed apps: expected hash of the image, checked on first start */
    uint8_t icon_sha256[32]; /* installed apps: expected hash of the icon, when there is one */
    int has_sha;
    int has_icon_sha;
    int verified;   /* 1 checked and good, -1 checked and bad */
    int system;     /* part of the OS image: not removable or replaceable by apps */
    int installed;  /* a package (ISO or /system/apps), not a built-in program */
    int owns_image; /* the kernel allocated physical pages for this package */
    uint64_t image_phys, image_pages; /* the whole package (header + ELF), for AFS installs */
};

/* The apps the OS ships. A package on the ISO with one of these names is
 * treated as a system app and gets this manifest, never the header's caps. */
struct system_app {
    const char *name;
    const char *title;
    uint32_t caps;
    uint32_t trust;
    uint32_t mem_quota_mib;
    uint32_t icon_color;
};

extern const struct system_app system_apps[];
extern const int nsystem_apps;
const struct system_app *system_app_find(const char *name);

/*
 * An installed app package (name.app in \apps on the boot partition, or in
 * /system/apps on the encrypted volume), made by tools/mkapp.py: this header,
 * then the program's ELF image.
 */
#define APP_MAGIC "AETHAPP1"
struct app_header {
    char magic[8];
    uint32_t header_size; /* sizeof(struct app_header) */
    uint32_t format;      /* 1 */
    char name[32];        /* program name: a-z, 0-9 and '-', at most 15 characters */
    char title[32];
    char version[32];
    char source[96];      /* e.g. github.com/storytold/gridcraft@v1.2.0 */
    uint32_t caps, trust, mem_quota_mib, abi, icon_color, pad;
    uint64_t elf_size;
    uint8_t elf_sha256[32];
    /*
     * An optional icon, appended after the ELF (so the image stays where the
     * loader expects it). RGBA8888, row-major, at most 256x256. `icon_offset`
     * is from the start of the package. Both hashes are checked when the
     * package is installed, and again on first start.
     */
    uint32_t icon_format; /* APP_ICON_* */
    uint32_t icon_width, icon_height;
    uint32_t icon_offset, icon_size;
    uint8_t icon_sha256[32];
    uint8_t reserved[188];
};
_Static_assert(sizeof(struct app_header) == 512, "app header");

#define APP_ICON_NONE 0
#define APP_ICON_RGBA 1

/* 0 if the package header and image are valid, else why not. The image's hash
 * is only checked if `hash` is set. Neither checks for a name collision. */
const char *app_check_header(const struct app_header *h, uint64_t size, int hash);
/* Just the icon: bounds and SHA-256. Used at boot/registration so a bad icon
 * is never drawn, without hashing the (larger) ELF yet. */
const char *app_check_icon(const struct app_header *h, uint64_t size);
/* As above, and refuses a name already in the program table. */
const char *app_check(const struct app_header *h, uint64_t size, int hash);
/* An installed app's image matches its package hash (checked once, on first start). */
int program_verify(const struct program *p);

/* Register a package. `sys` is the system manifest, or 0 for an untrusted
 * user app. `allocation` is freed when the program is removed. */
struct program *program_add(const struct app_header *h, const uint8_t *image, uint64_t size, uint64_t image_phys,
                            uint64_t image_pages, const struct system_app *sys);
/* Replace a program in place (an app update); the old strings and image are freed. */
struct program *program_replace(struct program *p, const struct app_header *h, const uint8_t *image, uint64_t size,
                                uint64_t image_phys, uint64_t image_pages, const struct system_app *sys);
void program_remove(struct program *p);
/* Mark every live process of this program as running an older version. */
void program_mark_update_pending(const struct program *p);
int program_any_running(const char *name);

struct bootinfo;
void programs_install(const struct bootinfo *bi); /* registers the packages the loader found */

const struct program *program_find(const char *name);
const struct program *program_at(int i); /* 0 past the end */
