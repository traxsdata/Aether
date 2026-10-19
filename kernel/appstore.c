/*
 * appstore.c - app packages that outlive the boot medium.
 *
 * The ISO carries the OS's own apps. Apps installed by the App Store are
 * kept in /system/apps on the encrypted volume, a folder no app can name
 * (vfs.c refuses /system). The kernel reads a package into physical pages,
 * checks its header, hash and ELF image, then registers it in the same
 * program table as the apps from the ISO. Installing never restarts a
 * running app: the old code keeps running and its title bar says an update
 * is pending until it is next started.
 */
#include "appstore.h"
#include "afs.h"
#include "apps.h"
#include "elf.h"
#include "kernel.h"
#include "mm.h"
#include "proc.h"
#include "storage.h"

#define APP_MAX_PAGES (APP_MAX_BYTES / PAGE_SIZE)
#define APP_NAME_MAX_FILE (APP_NAME_MAX + 4)

static uint32_t apps_ino;

static int ends_with_app(const char *name) {
    size_t n = strlen(name);
    return n > 4 && !strcmp(name + n - 4, ".app");
}

static void copy_field(char *dst, size_t n, const char *src) { strlcpy(dst, src ? src : "", n); }

static int file_name(char *out, size_t n, const char *name) {
    int len = snprintf(out, n, "%s.app", name);
    return len > 0 && (size_t)len < n;
}

static int make_apps_dir(void) {
    uint32_t sys = vfs_system();
    if (!sys) return -E_LOCKED;
    if (!afs_lookup(sys, "apps", &apps_ino)) return 0;
    int d = afs_ialloc(AFS_DIR, LABEL_PUBLIC, sys);
    if (d < 0) return d;
    int err = afs_dir_add(sys, "apps", (uint32_t)d, AFS_DIR);
    if (err) {
        afs_ifree((uint32_t)d);
        return err;
    }
    apps_ino = (uint32_t)d;
    return 0;
}

void appstore_init(void) {
    apps_ino = 0;
    int err = make_apps_dir();
    if (err) {
        kprintf("[apps] can't open /system/apps (%d); installed apps won't be available\n", err);
        return;
    }
    char name[NAME_MAX + 1];
    uint32_t ino;
    uint8_t type;
    int loaded = 0;
    for (uint32_t i = 0; afs_dir_entry(apps_ino, i, name, &ino, &type) == 1; i++) {
        if (type != AFS_FILE || !ends_with_app(name)) continue;
        struct afs_inode n;
        if (afs_iget(ino, &n) || n.size < sizeof(struct app_header) || n.size > APP_MAX_BYTES) {
            kprintf("[apps] skipping %s: bad size\n", name);
            continue;
        }
        uint64_t pages = ALIGN_UP(n.size, PAGE_SIZE) / PAGE_SIZE;
        uint64_t phys = pmm_alloc_contig(pages);
        if (!phys) {
            kprintf("[apps] no memory to load %s\n", name);
            break;
        }
        if (afs_read(ino, 0, P2V(phys), n.size) != (int64_t)n.size) {
            kprintf("[apps] can't read %s\n", name);
            pmm_free_contig(phys, pages);
            continue;
        }
        struct app_header *h = P2V(phys);
        const char *why = app_check_header(h, n.size, 0);
        if (!why) why = app_check_icon(h, n.size);
        if (why) {
            kprintf("[apps] skipping %s: %s\n", name, why);
            pmm_free_contig(phys, pages);
            continue;
        }
        if (system_app_find(h->name) || program_find(h->name)) {
            kprintf("[apps] skipping %s: %s is already installed\n", name, h->name);
            pmm_free_contig(phys, pages);
            continue;
        }
        if (program_add(h, (const uint8_t *)h + sizeof *h, h->elf_size, phys, pages, 0)) loaded++;
        else {
            pmm_free_contig(phys, pages);
            break;
        }
    }
    if (loaded) kprintf("[apps] %d installed app%s loaded from /system/apps\n", loaded, loaded == 1 ? "" : "s");
}

static int write_package(const struct app_header *h, uint64_t phys, uint64_t len) {
    char fname[APP_NAME_MAX_FILE];
    if (!file_name(fname, sizeof fname, h->name)) return -E_INVAL;
    uint32_t ino;
    int err = afs_lookup(apps_ino, fname, &ino);
    if (err == -E_NOENT) {
        int f = afs_ialloc(AFS_FILE, LABEL_PUBLIC, apps_ino);
        if (f < 0) return f;
        if ((err = afs_dir_add(apps_ino, fname, (uint32_t)f, AFS_FILE))) {
            afs_ifree((uint32_t)f);
            return err;
        }
        ino = (uint32_t)f;
    } else if (err) {
        return err;
    }
    if ((err = afs_truncate(ino, 0))) return err;
    int64_t wrote = afs_write(ino, 0, P2V(phys), len);
    if (wrote != (int64_t)len) {
        afs_truncate(ino, 0);
        return wrote < 0 ? (int)wrote : -E_NOSPACE;
    }
    afs_sync();
    return 0;
}

static int remove_file(const char *name) {
    char fname[APP_NAME_MAX_FILE];
    if (!file_name(fname, sizeof fname, name)) return -E_INVAL;
    uint32_t ino;
    int err = afs_lookup(apps_ino, fname, &ino);
    if (err == -E_NOENT) return 0;
    if (err) return err;
    if ((err = afs_dir_remove(apps_ino, fname))) return err;
    afs_sync();
    return afs_ifree(ino);
}

int appstore_install(uint64_t package_phys, uint64_t len, uint32_t flags) {
    if (!package_phys || len < sizeof(struct app_header) || len > APP_MAX_BYTES) return -E_INVAL;
    if (!afs_mounted() || !apps_ino) return -E_LOCKED;
    struct app_header *h = P2V(package_phys);
    const char *why = app_check_header(h, len, 1);
    if (why) {
        kprintf("[apps] refusing package: %s\n", why);
        return -E_INVAL;
    }
    if (system_app_find(h->name)) return -E_PERM;
    struct program *old = (struct program *)program_find(h->name);
    if (old && (!old->installed || old->system)) return -E_PERM;
    if (old && !(flags & APP_INSTALL_REPLACE)) return -E_EXIST;
    uint64_t pages = ALIGN_UP(len, PAGE_SIZE) / PAGE_SIZE;
    int err = write_package(h, package_phys, len);
    if (err) return err;
    if (old) {
        /* An update keeps the permissions the user already approved. A new
         * package cannot silently add CAP_NET or CAP_PICK; the app has to be
         * removed and installed deliberately to widen them. */
        uint32_t old_caps = old->caps;
        program_replace(old, h, (const uint8_t *)h + sizeof *h, h->elf_size, package_phys, pages, 0);
        old->caps &= old_caps;
        program_mark_update_pending(old);
        kprintf("[apps] updated %s to %s\n", h->name, h->version);
        return 0;
    }
    if (!program_add(h, (const uint8_t *)h + sizeof *h, h->elf_size, package_phys, pages, 0)) {
        remove_file(h->name);
        return -E_LIMIT;
    }
    kprintf("[apps] installed %s %s from the App Store\n", h->name, h->version);
    return 0;
}

int appstore_remove(const char *name) {
    if (!afs_mounted() || !apps_ino) return -E_LOCKED;
    struct program *p = (struct program *)program_find(name);
    if (!p) return -E_NOENT;
    if (!p->installed || p->system) return -E_PERM;
    if (program_any_running(name)) return -E_BUSY;
    int err = remove_file(name);
    if (err) return err;
    program_remove(p);
    kprintf("[apps] removed %s\n", name);
    return 0;
}

static void fill_info(const struct program *p, struct app_info *out) {
    memset(out, 0, sizeof *out);
    copy_field(out->name, sizeof out->name, p->name);
    copy_field(out->title, sizeof out->title, p->title ? p->title : p->name);
    copy_field(out->version, sizeof out->version, p->version);
    copy_field(out->source, sizeof out->source, p->source);
    out->caps = p->caps;
    out->trust = p->trust;
    out->abi = p->abi;
    out->system = (uint32_t)p->system;
    out->installed = (uint32_t)p->installed;
    out->running = (uint32_t)proc_named_running(p->name);
    out->update_pending = (uint32_t)proc_update_pending(p->name);
    out->icon_color = p->icon_color;
    out->mem_quota_mib = p->mem_quota_mib;
    out->size = p->size;
}

int appstore_list(struct app_info *out, int max) {
    int n = 0;
    for (int i = 0; program_at(i) && n < max; i++) fill_info(program_at(i), &out[n++]);
    return n;
}

int appstore_info(const char *name, struct app_info *out) {
    const struct program *p = program_find(name);
    if (!p) return -E_NOENT;
    fill_info(p, out);
    return 0;
}

int appstore_icon(const char *name, const uint8_t **data, uint32_t *w, uint32_t *h) {
    const struct program *p = program_find(name);
    if (!p || !p->icon || !p->icon_w || !p->icon_h) return 0;
    *data = p->icon;
    *w = p->icon_w;
    *h = p->icon_h;
    return 1;
}
