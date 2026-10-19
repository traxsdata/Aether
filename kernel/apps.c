/*
 * apps.c - the program table. Each app's capability set is its manifest:
 * the terminal is a power tool and gets inspection and process control,
 * the calculator only gets a window, Notes is untrusted (a window, the file
 * picker and a private folder) and the security probe gets only a folder.
 *
 * Calculator, Notes, Files and Probe stay native C programs built into the
 * kernel image. Settings and App Store are Rust packages on the ISO; a
 * package whose name is in `system_apps` is a system app, and the manifest
 * here (not the package header) decides its caps and trust. Everything else,
 * including anything downloaded by App Store, is untrusted and gets at most
 * a window, the picker, a private folder and the network.
 */
#include "apps.h"
#include "../common/release_key.h"
#include "../common/rsa_verify.h"
#include "../include/abi.h"
#include "../include/bootinfo.h"
#include "crypto.h"
#include "elf.h"
#include "kernel.h"
#include "mm.h"
#include "proc.h"

#define BLOB(n) extern const uint8_t n##_start[], n##_end[]
BLOB(app_terminal);
BLOB(app_calculator);
BLOB(app_notes);
BLOB(app_probe);
BLOB(app_explorer);

static struct program programs[] = {
    {.name = "terminal", .title = "Terminal", .image = app_terminal_start,
     .caps = CAP_WINDOW | CAP_LOG | CAP_SYSINFO | CAP_SPAWN | CAP_KILL | CAP_FILES | CAP_PICK | CAP_STORAGE |
             CAP_NET | CAP_FIREWALL | CAP_APPS | CAP_CONFIG | CAP_CLIPBOARD,
     .trust = TRUST_SYSTEM, .mem_quota_mib = 64, .abi = ABI_AETHER},
    {.name = "calculator", .title = "Calculator", .image = app_calculator_start,
     .caps = CAP_WINDOW, .trust = TRUST_TRUSTED, .mem_quota_mib = 16, .abi = ABI_AETHER},
    {.name = "notes", .title = "Notes", .image = app_notes_start,
     .caps = CAP_WINDOW | CAP_PICK | CAP_STORAGE | CAP_CLIPBOARD, .trust = TRUST_UNTRUSTED, .mem_quota_mib = 32,
     .abi = ABI_AETHER},
    {.name = "probe", .image = app_probe_start,
     .caps = CAP_STORAGE, .trust = TRUST_UNTRUSTED, .mem_quota_mib = 8, .abi = ABI_AETHER},
    {.name = "explorer", .title = "Files", .image = app_explorer_start,
     .caps = CAP_WINDOW | CAP_FILES | CAP_CLIPBOARD, .trust = TRUST_TRUSTED, .mem_quota_mib = 32, .abi = ABI_AETHER},
};

/*
 * The Rust apps the OS ships as packages on the ISO. The names and
 * capabilities here are authoritative. Settings and App Store are trusted
 * (not system): they get exactly the capabilities they need, and no more.
 */
const struct system_app system_apps[] = {
    {"settings", "Settings",
     CAP_WINDOW | CAP_SYSINFO | CAP_NET | CAP_FIREWALL | CAP_CONFIG, TRUST_TRUSTED, 256,
     0x8C96B8},
    {"installer", "App Store",
     CAP_WINDOW | CAP_NET | CAP_APPS | CAP_SPAWN | CAP_PICK | CAP_STORAGE | CAP_UPDATE, TRUST_TRUSTED, 768, 0x5CF2B0},
};
const int nsystem_apps = (int)ARRAY_LEN(system_apps);

const struct system_app *system_app_find(const char *name) {
    for (int i = 0; i < nsystem_apps; i++)
        if (!strcmp(system_apps[i].name, name)) return &system_apps[i];
    return 0;
}

/* Installed packages, after the built-in programs. */
#define MAX_INSTALLED 32
static struct program installed[MAX_INSTALLED];
static int ninstalled;

static void fill_sizes(void) {
    programs[0].size = (uint64_t)(app_terminal_end - app_terminal_start);
    programs[1].size = (uint64_t)(app_calculator_end - app_calculator_start);
    programs[2].size = (uint64_t)(app_notes_end - app_notes_start);
    programs[3].size = (uint64_t)(app_probe_end - app_probe_start);
    programs[4].size = (uint64_t)(app_explorer_end - app_explorer_start);
}

const struct program *program_at(int i) {
    fill_sizes();
    if (i >= 0 && i < (int)ARRAY_LEN(programs)) return &programs[i];
    i -= (int)ARRAY_LEN(programs);
    return i >= 0 && i < ninstalled ? &installed[i] : 0;
}

const struct program *program_find(const char *name) {
    for (int i = 0; program_at(i); i++)
        if (!strcmp(program_at(i)->name, name)) return program_at(i);
    return 0;
}

static int valid_name(const char *s, size_t max) {
    size_t n = strnlen(s, max);
    if (n == 0 || n >= 16) return 0;
    for (size_t i = 0; i < n; i++)
        if (!((s[i] >= 'a' && s[i] <= 'z') || (s[i] >= '0' && s[i] <= '9') || s[i] == '-')) return 0;
    return 1;
}

static char *dup_field(const char *s, size_t max) {
    size_t n = strnlen(s, max);
    char *d = kmalloc(n + 1);
    if (!d) panic("apps: out of memory");
    for (size_t i = 0; i < n; i++) d[i] = (s[i] >= 32 && s[i] < 127) ? s[i] : '?';
    d[n] = 0;
    return d;
}

/* The optional icon is data, not code, but it lives in the same package and
 * is drawn by the kernel: validate it (and, when asked, its hash) before
 * anyone sees it. */
static const char *icon_check(const struct app_header *h, uint64_t size, int hash) {
    if (h->icon_format == APP_ICON_NONE) {
        if (h->icon_width || h->icon_height || h->icon_offset || h->icon_size) return "bad app icon";
    } else if (h->icon_format == APP_ICON_RGBA) {
        if (!h->icon_width || !h->icon_height || h->icon_width > APP_ICON_MAX || h->icon_height > APP_ICON_MAX)
            return "bad app icon size";
        uint64_t want = (uint64_t)h->icon_width * h->icon_height * 4;
        if (h->icon_size != want) return "bad app icon size";
        uint64_t start = sizeof *h + h->elf_size;
        if (h->icon_offset < start || h->icon_offset > size || h->icon_size > size - h->icon_offset)
            return "bad app icon offset";
        if (hash) {
            uint8_t digest[32];
            sha256((const uint8_t *)h + h->icon_offset, h->icon_size, digest);
            if (!crypto_memeq(digest, h->icon_sha256, 32)) return "app icon checksum mismatch (damaged?)";
        }
    } else {
        return "unknown app icon format";
    }
    return 0;
}

const char *app_check_icon(const struct app_header *h, uint64_t size) {
    return icon_check(h, size, 1);
}

/* The signature trailer is optional before 1.4 and required for packages the
 * App Store downloads (APP_INSTALL_SIGNED). Its shape is always checked, so
 * a package cannot hide data past the part that is hashed. */
const char *app_check_signature_shape(const struct app_header *h, uint64_t size) {
    if (h->sig_algorithm == APP_SIG_NONE) {
        if (h->sig_size || h->sig_offset || h->sig_publisher) return "bad app signature fields";
        return 0;
    }
    if (h->sig_algorithm != APP_SIG_RSA2048_SHA256 || h->sig_size != APP_SIG_SIZE) return "unknown app signature";
    if (h->sig_publisher != APP_PUBLISHER_AETHER) return "unknown app publisher";
    /* The trailer is the last thing in the package, after the icon. */
    uint64_t body = (uint64_t)sizeof *h + h->elf_size;
    if (h->icon_format == APP_ICON_RGBA) body += h->icon_size;
    if (h->sig_offset != body || h->sig_offset > size || h->sig_size > size - h->sig_offset)
        return "bad app signature offset";
    if (h->sig_offset + h->sig_size != size) return "data after app signature";
    return 0;
}

int app_is_signed(const struct app_header *h, uint64_t size) {
    return h->sig_algorithm != APP_SIG_NONE && !app_check_signature_shape(h, size);
}

const char *app_verify_signature(const struct app_header *h, uint64_t size) {
    const char *why = app_check_signature_shape(h, size);
    if (why) return why;
    if (h->sig_algorithm == APP_SIG_NONE) return "app package is not signed";
    uint8_t digest[32];
    sha256((const uint8_t *)h, h->sig_offset, digest);
    if (!rsa_verify_sha256(digest, (const uint8_t *)h + h->sig_offset, aether_release_modulus))
        return "app signature is not from the release key";
    return 0;
}

const char *app_check_header(const struct app_header *h, uint64_t size, int hash) {
    if (size < sizeof *h || memcmp(h->magic, APP_MAGIC, 8) || h->header_size != sizeof *h || h->format != 1)
        return "not an æther app package";
    if (h->elf_size == 0 || h->elf_size > size - sizeof *h) return "truncated";
    if (!valid_name(h->name, sizeof h->name)) return "bad program name";
    if (h->abi != ABI_AETHER && h->abi != ABI_LINUX) return "unknown system call interface";
    if (h->mem_quota_mib > 512) return "memory quota is too large";
    const uint8_t *image = (const uint8_t *)h + sizeof *h;
    if (elf_check(image, h->elf_size, 1)) return "invalid or unsafe program image";
    const char *icon_why = icon_check(h, size, hash);
    if (icon_why) return icon_why;
    const char *sig_why = app_check_signature_shape(h, size);
    if (sig_why) return sig_why;
    /* Native (æther-ABI) programs are fixed-address executables; only the
     * Linux layer loads position-independent images. */
    if (h->abi == ABI_AETHER && *(const uint16_t *)(image + 16) != 2)
        return "native apps must be fixed-address executables";
    if (!hash) return 0;
    uint8_t digest[32];
    sha256(image, h->elf_size, digest);
    if (!crypto_memeq(digest, h->elf_sha256, 32)) return "checksum mismatch (damaged?)";
    return 0;
}

const char *app_check(const struct app_header *h, uint64_t size, int hash) {
    const char *why = app_check_header(h, size, hash);
    if (why) return why;
    if (program_find(h->name)) return "name already taken";
    return 0;
}

/*
 * Register a package. `sys` means it is one of the OS's own apps: its manifest
 * decides caps and trust. User packages are always untrusted, whatever their
 * header says, and may only ask for a window, the picker, a private folder
 * and the network (every connection still passes the firewall).
 */
static void program_clear(struct program *p) {
    kfree((void *)p->name);
    kfree((void *)p->title);
    kfree((void *)p->version);
    kfree((void *)p->source);
    if (p->owns_image) pmm_free_contig(p->image_phys, p->image_pages);
    memset(p, 0, sizeof *p);
}

static void program_fill(struct program *p, const struct app_header *h, const uint8_t *image, uint64_t size,
                         uint64_t image_phys, uint64_t image_pages, const struct system_app *sys) {
    memset(p, 0, sizeof *p);
    p->name = dup_field(h->name, sizeof h->name);
    if (sys) {
        p->title = sys->title ? dup_field(sys->title, sizeof h->title) : 0;
        p->caps = sys->caps;
        p->trust = sys->trust;
        p->mem_quota_mib = sys->mem_quota_mib;
        p->icon_color = sys->icon_color & 0xFFFFFF;
        p->system = 1;
    } else {
        p->title = dup_field(h->title[0] ? h->title : h->name, sizeof h->title);
        p->caps = h->caps & (CAP_WINDOW | CAP_PICK | CAP_STORAGE | CAP_NET);
        p->trust = TRUST_UNTRUSTED;
        p->mem_quota_mib = MIN(MAX(h->mem_quota_mib, 16u), 512u);
        p->icon_color = h->icon_color & 0xFFFFFF;
    }
    p->version = dup_field(h->version, sizeof h->version);
    p->source = dup_field(h->source, sizeof h->source);
    p->image = image;
    p->size = size;
    p->abi = h->abi;
    if (h->icon_format == APP_ICON_RGBA) {
        p->icon = (const uint8_t *)h + h->icon_offset;
        p->icon_w = h->icon_width;
        p->icon_h = h->icon_height;
        memcpy(p->icon_sha256, h->icon_sha256, 32);
        p->has_icon_sha = 1;
    }
    memcpy(p->sha256, h->elf_sha256, 32);
    p->has_sha = 1;
    p->verified = 0;
    p->installed = 1;
    p->owns_image = image_phys != 0;
    p->image_phys = image_phys;
    p->image_pages = image_pages;
}

static void log_program(const struct program *p) {
    kprintf("[apps] installed %s %s (%lu KiB, %s, %s, from %s)\n", p->name, p->version, p->size / 1024,
            p->abi == ABI_LINUX ? "Linux ABI" : "native", p->system ? "system" : "untrusted",
            p->source[0] ? p->source : "unknown source");
}

struct program *program_add(const struct app_header *h, const uint8_t *image, uint64_t size, uint64_t image_phys,
                            uint64_t image_pages, const struct system_app *sys) {
    if (ninstalled >= MAX_INSTALLED) return 0;
    struct program *p = &installed[ninstalled];
    program_fill(p, h, image, size, image_phys, image_pages, sys);
    ninstalled++;
    log_program(p);
    return p;
}

struct program *program_replace(struct program *p, const struct app_header *h, const uint8_t *image, uint64_t size,
                                uint64_t image_phys, uint64_t image_pages, const struct system_app *sys) {
    if (!p) return 0;
    program_clear(p);
    program_fill(p, h, image, size, image_phys, image_pages, sys);
    log_program(p);
    return p;
}

void program_remove(struct program *p) {
    int idx = -1;
    for (int i = 0; i < ninstalled; i++)
        if (&installed[i] == p) idx = i;
    if (idx < 0) return;
    program_clear(p);
    for (int i = idx; i < ninstalled - 1; i++) installed[i] = installed[i + 1];
    ninstalled--;
    memset(&installed[ninstalled], 0, sizeof installed[ninstalled]);
}

int program_verify(const struct program *p) {
    if (!p->has_sha) return 1; /* built into the kernel */
    struct program *w = 0;
    for (int i = 0; i < ninstalled; i++)
        if (&installed[i] == p) w = &installed[i];
    if (!w) return 0;
    if (!w->verified) {
        uint8_t digest[32];
        sha256(w->image, w->size, digest);
        int good = crypto_memeq(digest, w->sha256, 32);
        if (good && w->has_icon_sha && w->icon) {
            sha256(w->icon, (uint64_t)w->icon_w * w->icon_h * 4, digest);
            good = crypto_memeq(digest, w->icon_sha256, 32);
        }
        w->verified = good ? 1 : -1;
        if (w->verified < 0) kprintf("[apps] %s: checksum mismatch, refusing to start it\n", w->name);
    }
    return w->verified > 0;
}

void program_mark_update_pending(const struct program *p) {
    if (p) proc_mark_update_pending(p->name);
}

int program_any_running(const char *name) { return proc_named_running(name); }

/* Packages the loader found, in the order they were found. */
struct pending {
    const struct app_header *h;
    uint64_t size;
    const char *module_name;
    int used;
};

void programs_install(const struct bootinfo *bi) {
    struct pending pkgs[BOOT_MODULES_MAX];
    int npkgs = 0;
    for (uint32_t m = 0; m < bi->nmodules && m < BOOT_MODULES_MAX; m++) {
        const struct boot_module *mod = &bi->modules[m];
        const struct app_header *h = P2V(mod->phys);
        /* Hashing a big app takes a while, so that waits until it's first started. */
        const char *why = app_check_header(h, mod->size, 0);
        if (!why) why = app_check_icon(h, mod->size);
        if (why) {
            kprintf("[apps] skipping %s: %s\n", mod->name, why);
            continue;
        }
        pkgs[npkgs].h = h;
        pkgs[npkgs].size = mod->size;
        pkgs[npkgs].module_name = mod->name;
        pkgs[npkgs].used = 0;
        npkgs++;
    }

    /* System apps first, in the manifest's order, so the dock is stable. */
    for (int s = 0; s < nsystem_apps; s++) {
        int found = -1;
        for (int i = 0; i < npkgs; i++)
            if (!pkgs[i].used && !strcmp((const char *)pkgs[i].h->name, system_apps[s].name)) {
                found = i;
                break;
            }
        if (found < 0) {
            kprintf("[apps] system app %s is missing from the image\n", system_apps[s].name);
            continue;
        }
        pkgs[found].used = 1;
        const struct app_header *h = pkgs[found].h;
        program_add(h, (const uint8_t *)h + sizeof *h, h->elf_size, 0, 0, &system_apps[s]);
    }

    /* Everything else is a user app, even when it comes from the ISO. */
    for (int i = 0; i < npkgs; i++) {
        if (pkgs[i].used) continue;
        const struct app_header *h = pkgs[i].h;
        if (system_app_find(h->name) || program_find(h->name)) {
            kprintf("[apps] skipping %s: name is taken\n", pkgs[i].module_name);
            continue;
        }
        program_add(h, (const uint8_t *)h + sizeof *h, h->elf_size, 0, 0, 0);
    }
}
