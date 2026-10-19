/*
 * vfs.c - file system calls: path rules, handles and the DLP checks.
 *
 * What a process can name depends on its capabilities:
 *   CAP_FILES    absolute paths from the root ("/home/report.txt")
 *   CAP_STORAGE  relative paths inside its own folder, /apps/<name>
 *   CAP_PICK     no paths at all: only files the user chose for it in the
 *                system file picker, handed over as already-open handles
 * ".." is never accepted, so there is no way to climb out of a folder.
 */
#include "../include/abi.h"
#include "afs.h"
#include "dlp.h"
#include "net.h"
#include "kernel.h"
#include "proc.h"
#include "uaccess.h"
#include "wm.h"

#define CHUNK 4096
static uint8_t chunk[CHUNK];

static uint32_t home_ino, apps_ino, system_ino;

/* Creates /home, /apps and the kernel's /system on a fresh volume, and finds
 * them on an old one. /system is never visible to apps (see resolve()). */
void vfs_init_layout(void) {
    static const struct {
        const char *name;
        uint8_t label;
        uint32_t *ino;
    } dirs[] = {{"home", LABEL_PERSONAL, &home_ino},
                {"apps", LABEL_PUBLIC, &apps_ino},
                {"system", LABEL_PERSONAL, &system_ino}};
    for (unsigned i = 0; i < ARRAY_LEN(dirs); i++) {
        if (!afs_lookup(AFS_ROOT, dirs[i].name, dirs[i].ino)) continue;
        int ino = afs_ialloc(AFS_DIR, dirs[i].label, AFS_ROOT);
        if (ino < 0 || afs_dir_add(AFS_ROOT, dirs[i].name, (uint32_t)ino, AFS_DIR)) panic("vfs: can't create /%s", dirs[i].name);
        *dirs[i].ino = (uint32_t)ino;
    }
    fw_load();
}

uint32_t vfs_home(void) { return home_ino; }
uint32_t vfs_system(void) { return system_ino; }

/* The process's own folder, created the first time it's needed. */
static int storage_root(struct proc *p, uint32_t *out) {
    if (p->storage_ino) {
        *out = p->storage_ino;
        return 0;
    }
    uint32_t ino;
    int err = afs_lookup(apps_ino, p->name, &ino);
    if (err == -E_NOENT) {
        int n = afs_ialloc(AFS_DIR, LABEL_PUBLIC, apps_ino);
        if (n < 0) return n;
        if ((err = afs_dir_add(apps_ino, p->name, (uint32_t)n, AFS_DIR))) {
            afs_ifree((uint32_t)n);
            return err;
        }
        ino = (uint32_t)n;
    } else if (err) {
        return err;
    }
    *out = p->storage_ino = ino;
    return 0;
}

/*
 * A folder's entries (names, and the sizes beside them) are data too. An
 * untrusted app holding Confidential data could otherwise spell it out in
 * file names in its own folder, for a later, untainted run of itself to read
 * back and send. So for untrusted apps:
 *   - changing a folder's entries carries the app's label into the folder,
 *     just as writing a file raises the file's label;
 *   - looking names up or listing a folder taints the app with its label;
 *   - a file's size is the file's: stat taints with its label, and listings
 *     leave out the sizes of files labelled above what the app holds.
 * Trusted and system apps (Files, Terminal) aren't the threat this guards
 * against, and keep seeing everything as before.
 */
static int meta_tracked(void) { return current->trust == TRUST_UNTRUSTED; }

static void meta_read(const struct afs_inode *n, const char *what) {
    if (meta_tracked()) dlp_taint(current, n->label, what);
}

static int dir_changed(uint32_t dir) {
    if (!meta_tracked()) return 0;
    struct afs_inode d;
    int err = afs_iget(dir, &d);
    if (err) return err;
    if (d.label >= current->taint) return 0;
    dlp_audit("folder label raised %s -> %s: changed by %s", dlp_label_name(d.label), dlp_label_name((int)current->taint),
              current->name);
    d.label = (uint8_t)current->taint;
    return afs_iput(dir, &d);
}

/* For the Linux layer: metadata of `ino` is about to be shown to the app. */
void vfs_meta_read(uint32_t ino) {
    struct afs_inode n;
    if (meta_tracked() && !afs_iget(ino, &n)) meta_read(&n, n.mode == AFS_DIR ? "a folder" : "a file's size");
}

/* Data from the caller is about to change file `ino`: its label rises to the caller's. */
int vfs_label_write(uint32_t ino) {
    struct afs_inode n;
    int err = afs_iget(ino, &n);
    if (err || n.label >= current->taint) return err;
    dlp_audit("file label raised %s -> %s: written by %s", dlp_label_name(n.label), dlp_label_name((int)current->taint),
              current->name);
    n.label = (uint8_t)current->taint;
    return afs_iput(ino, &n);
}

/*
 * Resolve `path` for process p. On success *dir is the folder holding the
 * last component, `leaf` its name (empty for the root itself) and *ino the
 * target, or 0 if it doesn't exist (yet).
 */
static int resolve(struct proc *p, const char *path, uint32_t *dir, char *leaf, uint32_t *ino) {
    if (!afs_mounted()) return -E_LOCKED;
    uint32_t cur;
    if (p->caps & CAP_FILES) {
        if (path[0] != '/') return -E_INVAL;
        cur = AFS_ROOT;
    } else if (p->caps & CAP_STORAGE) {
        if (path[0] == '/') return -E_PERM;
        int err = storage_root(p, &cur);
        if (err) return err;
    } else {
        return -E_PERM;
    }
    *dir = cur;
    *ino = cur;
    leaf[0] = 0;
    const char *s = path;
    while (*s) {
        while (*s == '/') s++;
        if (!*s) break;
        const char *e = s;
        while (*e && *e != '/') e++;
        size_t len = (size_t)(e - s);
        if (len > NAME_MAX) return -E_INVAL;
        if (len == 2 && s[0] == '.' && s[1] == '.') return -E_PERM;
        if (!(len == 1 && s[0] == '.')) {
            if (!*ino) return -E_NOENT; /* a middle component doesn't exist */
            struct afs_inode n;
            int err = afs_iget(*ino, &n);
            if (err) return err;
            if (n.mode != AFS_DIR) return -E_NOTDIR;
            meta_read(&n, "a folder");
            *dir = *ino;
            memcpy(leaf, s, len);
            leaf[len] = 0;
            if (*dir == AFS_ROOT && !strcmp(leaf, "system")) return -E_PERM; /* the kernel's own settings */
            err = afs_lookup(*dir, leaf, ino);
            if (err == -E_NOENT) *ino = 0;
            else if (err) return err;
        }
        s = e;
    }
    return 0;
}

int vfs_lookup(const char *path, uint32_t *ino) {
    char leaf[NAME_MAX + 1];
    uint32_t dir;
    int err = resolve(current, path, &dir, leaf, ino);
    if (!err && !*ino) err = -E_NOENT;
    return err;
}

static struct file_handle *handle(uint64_t h) {
    if (h >= MAX_FILES || !current->files[h].used) return 0;
    return &current->files[h];
}

static int new_handle(uint32_t ino, uint32_t flags, uint32_t type) {
    for (int i = 0; i < MAX_FILES; i++)
        if (!current->files[i].used) {
            current->files[i] = (struct file_handle){1, ino, flags, type, 0, 0};
            return i;
        }
    return -E_LIMIT;
}

/* A new file or folder gets the higher of its folder's label and the creator's
 * taint. `by_app`: the app chose the name (not the user, in the file picker). */
static int create(uint32_t dir, const char *name, uint16_t mode, int by_app) {
    struct afs_inode d;
    int err = afs_iget(dir, &d);
    if (err) return err;
    if (d.mode != AFS_DIR) return -E_NOTDIR;
    if (by_app && (err = dir_changed(dir))) return err;
    uint8_t label = (uint8_t)MAX((uint32_t)d.label, current->taint);
    int ino = afs_ialloc(mode, label, dir);
    if (ino < 0) return ino;
    if ((err = afs_dir_add(dir, name, (uint32_t)ino, (uint8_t)mode))) {
        afs_ifree((uint32_t)ino);
        return err;
    }
    return ino;
}

static int open_ino(uint32_t ino, uint32_t flags) {
    struct afs_inode n;
    int err = afs_iget(ino, &n);
    if (err) return err;
    if ((flags & O_DIR) && n.mode != AFS_DIR) return -E_NOTDIR;
    if (!(flags & O_DIR) && n.mode == AFS_DIR) return -E_ISDIR;
    if ((flags & O_DIR) && (flags & O_WRITE)) return -E_INVAL;
    meta_read(&n, "a file"); /* opening an existing file reveals its label */
    int h = new_handle(ino, flags, n.mode);
    if (h < 0) return h;
    if ((flags & O_TRUNC) && ((err = vfs_label_write(ino)) || (err = afs_truncate(ino, 0)))) {
        current->files[h].used = 0;
        return err;
    }
    return h;
}

int64_t sys_open(uint64_t upath, uint64_t len, uint64_t flags) {
    char path[PATH_MAX + 1];
    int err = copy_path(path, upath, len);
    return err ? err : vfs_open(path, flags);
}

int64_t vfs_open(const char *path, uint64_t flags) {
    char leaf[NAME_MAX + 1];
    int err;
    if (!(flags & (O_READ | O_WRITE | O_DIR)) || ((flags & (O_TRUNC | O_APPEND)) && !(flags & O_WRITE))) return -E_INVAL;
    uint32_t dir, ino;
    if ((err = resolve(current, path, &dir, leaf, &ino))) return err;
    if (ino && (flags & O_CREATE) && (flags & O_EXCL)) return -E_EXIST;
    if (!ino) {
        if (!(flags & O_CREATE) || (flags & O_DIR)) return -E_NOENT;
        int n = create(dir, leaf, AFS_FILE, 1);
        if (n < 0) return n;
        ino = (uint32_t)n;
    }
    return open_ino(ino, (uint32_t)flags);
}

int64_t sys_close(uint64_t h) {
    struct file_handle *f = handle(h);
    if (!f) return -E_INVAL;
    if (f->dirty) afs_sync();
    f->used = 0;
    return 0;
}

int64_t sys_read(uint64_t h, uint64_t ubuf, uint64_t len) {
    struct file_handle *f = handle(h);
    if (!f || !(f->flags & O_READ) || f->type != AFS_FILE) return -E_INVAL;
    if (!user_range_ok(ubuf, len, 1)) return -E_FAULT;
    struct afs_inode n;
    int err = afs_iget(f->ino, &n);
    if (err) return err;
    /* Taint before the first byte leaves. A later block can fail its
     * integrity check, and the bytes already copied are still file data. */
    if (len) meta_read(&n, "a file");
    uint64_t done = 0;
    int64_t result = 0;
    while (done < len) {
        int64_t got = afs_read(f->ino, f->off, chunk, MIN(len - done, (uint64_t)CHUNK));
        if (got < 0) {
            result = done ? (int64_t)done : got;
            break;
        }
        if (got == 0) {
            result = (int64_t)done;
            break;
        }
        if (copy_out(ubuf + done, chunk, (uint64_t)got)) {
            result = -E_FAULT;
            break;
        }
        f->off += (uint64_t)got;
        done += (uint64_t)got;
        result = (int64_t)done;
    }
    memset(chunk, 0, sizeof chunk);
    return result;
}

int64_t sys_write(uint64_t h, uint64_t ubuf, uint64_t len) {
    struct file_handle *f = handle(h);
    if (!f || !(f->flags & O_WRITE)) return -E_INVAL;
    if (!user_range_ok(ubuf, len, 0)) return -E_FAULT;
    struct afs_inode n;
    int err = afs_iget(f->ino, &n);
    if (err) return err;
    if (n.label < current->taint) { /* the data carries its label into the file */
        dlp_audit("file label raised %s -> %s: written by %s", dlp_label_name(n.label), dlp_label_name((int)current->taint),
                  current->name);
        n.label = (uint8_t)current->taint;
        if ((err = afs_iput(f->ino, &n))) return err;
    }
    /* O_APPEND chooses the offset for this write. Do not publish the file
     * size through f->off before a byte has actually been written: seek()
     * would otherwise reveal a Confidential file's size to a clean run. */
    uint64_t off = (f->flags & O_APPEND) ? n.size : f->off;
    uint64_t done = 0;
    while (done < len) {
        uint64_t part = MIN(len - done, (uint64_t)CHUNK);
        if (copy_in(chunk, ubuf + done, part)) {
            memset(chunk, 0, sizeof chunk);
            return done ? (int64_t)done : -E_FAULT;
        }
        int64_t put = afs_write(f->ino, off, chunk, part);
        if (put <= 0) {
            memset(chunk, 0, sizeof chunk);
            return done ? (int64_t)done : (put ? put : -E_IO);
        }
        off += (uint64_t)put;
        done += (uint64_t)put;
    }
    memset(chunk, 0, sizeof chunk);
    f->off = off;
    if (done) f->dirty = 1;
    return (int64_t)done;
}

int64_t sys_seek(uint64_t h, int64_t off, uint64_t whence) {
    struct file_handle *f = handle(h);
    if (!f || f->type != AFS_FILE) return -E_INVAL;
    if (whence != SEEK_SET && whence != SEEK_CUR && whence != SEEK_END) return -E_INVAL;
    struct afs_inode n;
    int err = afs_iget(f->ino, &n);
    if (err) return err;
    /* The size is data about the file. SEEK_END always reveals it; SEEK_CUR
     * does too when an O_APPEND handle has not written yet. */
    if (whence == SEEK_END || ((f->flags & O_APPEND) && whence == SEEK_CUR)) meta_read(&n, "a file's size");
    uint64_t base = whence == SEEK_SET ? 0 : whence == SEEK_CUR ? f->off : n.size;
    if (off < 0 && (uint64_t)(-(off + 1)) + 1 > base) return -E_INVAL;
    uint64_t next = base + (uint64_t)off;
    if (next > (uint64_t)INT64_MAX) return -E_INVAL;
    f->off = next;
    return (int64_t)next;
}

static void fill_stat(const struct afs_inode *n, struct file_stat *st) {
    st->size = n->mode == AFS_DIR ? 0 : n->size;
    st->mtime = n->mtime;
    st->type = n->mode;
    st->label = n->label;
}

int64_t sys_truncate(uint64_t h, uint64_t size) {
    struct file_handle *f = handle(h);
    if (!f || !(f->flags & O_WRITE) || f->type != AFS_FILE) return -E_INVAL;
    int err = afs_truncate(f->ino, size);
    if (!err) {
        f->dirty = 1;
        if (f->off > size) f->off = size;
    }
    return err;
}

int64_t sys_stat(uint64_t upath, uint64_t len, uint64_t uout) {
    uint32_t ino;
    if (upath == 0) { /* fstat */
        struct file_handle *f = handle(len);
        if (!f) return -E_INVAL;
        ino = f->ino;
    } else {
        char path[PATH_MAX + 1], leaf[NAME_MAX + 1];
        uint32_t dir;
        int err = copy_path(path, upath, len);
        if (err || (err = resolve(current, path, &dir, leaf, &ino))) return err;
        if (!ino) return -E_NOENT;
    }
    struct afs_inode n;
    int err = afs_iget(ino, &n);
    if (err) return err;
    meta_read(&n, "a file's size");
    struct file_stat st = {0};
    fill_stat(&n, &st);
    return copy_out(uout, &st, sizeof st);
}

int64_t sys_readdir(uint64_t h, uint64_t idx, uint64_t uout) {
    struct file_handle *f = handle(h);
    if (!f || f->type != AFS_DIR) return -E_INVAL;
    struct dir_entry de = {0};
    uint32_t ino;
    uint8_t type;
    struct afs_inode d;
    if (afs_iget(f->ino, &d)) return -E_IO;
    meta_read(&d, "a folder");
    int r = afs_dir_entry(f->ino, (uint32_t)idx, de.name, &ino, &type);
    if (r <= 0) return r;
    if (f->ino == AFS_ROOT) { /* /system is the kernel's: listed to nobody, so indexes past it shift by one */
        uint32_t sys;
        if (!afs_lookup(AFS_ROOT, "system", &sys)) {
            uint32_t at = 0;
            char name[NAME_MAX + 1];
            for (uint32_t i = 0; i <= idx && afs_dir_entry(AFS_ROOT, i, name, &ino, &type) == 1; i++)
                if (ino == sys) at = i + 1;
            if (at && idx + 1 >= at) {
                r = afs_dir_entry(f->ino, (uint32_t)idx + 1, de.name, &ino, &type);
                if (r <= 0) return r;
            } else {
                afs_dir_entry(f->ino, (uint32_t)idx, de.name, &ino, &type);
            }
        }
    }
    struct afs_inode n;
    if (afs_iget(ino, &n)) return -E_IO;
    /* The name, label and size are metadata about the file's data. Listing a
     * Confidential/Secret entry must taint the caller too, or labels can be
     * used as a 2-bit-per-file exfiltration channel. */
    meta_read(&n, "a folder entry");
    de.type = type;
    de.label = n.label;
    de.size = n.mode == AFS_DIR || (meta_tracked() && n.label > current->taint) ? 0 : n.size;
    if (copy_out(uout, &de, sizeof de)) return -E_FAULT;
    return 1;
}

int64_t sys_mkdir(uint64_t upath, uint64_t len) {
    char path[PATH_MAX + 1];
    int err = copy_path(path, upath, len);
    return err ? err : vfs_mkdir(path);
}

int64_t vfs_mkdir(const char *path) {
    char leaf[NAME_MAX + 1];
    uint32_t dir, ino;
    int err;
    if ((err = resolve(current, path, &dir, leaf, &ino))) return err;
    if (ino) return -E_EXIST;
    int n = create(dir, leaf, AFS_DIR, 1);
    return n < 0 ? n : 0;
}

/* Folders the system itself depends on can't be removed or renamed. */
static int protected_ino(uint32_t ino) { return ino == AFS_ROOT || ino == home_ino || ino == apps_ino || ino == current->storage_ino; }

int64_t sys_remove(uint64_t upath, uint64_t len) {
    char path[PATH_MAX + 1];
    int err = copy_path(path, upath, len);
    return err ? err : vfs_remove(path);
}

int64_t vfs_remove(const char *path) {
    char leaf[NAME_MAX + 1];
    uint32_t dir, ino;
    int err;
    if ((err = resolve(current, path, &dir, leaf, &ino))) return err;
    if (!ino) return -E_NOENT;
    if (protected_ino(ino)) return -E_PERM;
    struct afs_inode n;
    if ((err = afs_iget(ino, &n))) return err;
    meta_read(&n, "a file name");
    if (n.mode == AFS_DIR && !afs_dir_empty(ino)) return -E_NOTEMPTY;
    if (proc_file_in_use(ino)) return -E_LIMIT;
    if ((err = dir_changed(dir))) return err;
    if ((err = afs_dir_remove(dir, leaf))) return err;
    return afs_ifree(ino);
}

int64_t sys_rename(uint64_t ufrom, uint64_t flen, uint64_t uto, uint64_t tlen) {
    char from[PATH_MAX + 1], to[PATH_MAX + 1];
    int err = copy_path(from, ufrom, flen);
    if (err || (err = copy_path(to, uto, tlen))) return err;
    return vfs_rename(from, to);
}

int64_t vfs_rename(const char *from, const char *to) {
    char fleaf[NAME_MAX + 1], tleaf[NAME_MAX + 1];
    uint32_t fdir, fino, tdir, tino;
    int err;
    if ((err = resolve(current, from, &fdir, fleaf, &fino)) || (err = resolve(current, to, &tdir, tleaf, &tino))) return err;
    if (!fino) return -E_NOENT;
    if (tino) return -E_EXIST;
    if (protected_ino(fino) || !tleaf[0]) return -E_PERM;
    /* A folder can't move inside itself. */
    for (uint32_t a = tdir, guard = 0; guard < 4096; guard++) {
        if (a == fino) return -E_INVAL;
        if (a == AFS_ROOT) break;
        struct afs_inode n;
        if (afs_iget(a, &n)) return -E_IO;
        a = n.parent;
    }
    struct afs_inode n;
    if ((err = dir_changed(fdir)) || (err = dir_changed(tdir))) return err;
    if ((err = afs_iget(fino, &n))) return err;
    meta_read(&n, "a file name");
    if ((err = afs_dir_add(tdir, tleaf, fino, (uint8_t)n.mode))) return err;
    if ((err = afs_dir_remove(fdir, fleaf))) return err;
    n.parent = tdir;
    return afs_iput(fino, &n);
}

int64_t sys_set_label(uint64_t upath, uint64_t len, uint64_t label) {
    char path[PATH_MAX + 1], leaf[NAME_MAX + 1];
    uint32_t dir, ino;
    if (label >= LABEL_COUNT) return -E_INVAL;
    int err = copy_path(path, upath, len);
    if (err || (err = resolve(current, path, &dir, leaf, &ino))) return err;
    if (!ino) return -E_NOENT;
    struct afs_inode n;
    if ((err = afs_iget(ino, &n))) return err;
    meta_read(&n, "a file's label");
    if (label < n.label) {
        /* Lowering a label is the one thing DLP always asks you about. */
        cur_thread->dialog_done = 0;
        if ((err = wm_dialog_label(current, leaf[0] ? leaf : "/", n.label, (int)label))) return err;
        while (!cur_thread->dialog_done) proc_block(WAIT_DIALOG, -1);
        if (!cur_thread->dialog_result) {
            dlp_audit("you kept %s as %s (asked by %s)", leaf, dlp_label_name(n.label), current->name);
            return -E_DENIED;
        }
        dlp_audit("you lowered %s from %s to %s (asked by %s)", leaf, dlp_label_name(n.label), dlp_label_name((int)label),
                  current->name);
        /* It may have changed while we waited: the user agreed to lower this
         * file from the label they were shown, so anything else (a write that
         * raised it, or the file removed and its inode reused) is refused. */
        int shown = n.label;
        uint32_t gen = n.gen;
        if ((err = afs_iget(ino, &n))) return err;
        if (n.label != shown || n.gen != gen) {
            dlp_audit("refused: %s changed while you were asked about it (asked by %s)", leaf, current->name);
            return -E_DENIED;
        }
    } else if (label > n.label) {
        dlp_audit("%s raised %s from %s to %s", current->name, leaf, dlp_label_name(n.label), dlp_label_name((int)label));
    }
    n.label = (uint8_t)label;
    return afs_iput(ino, &n);
}

int64_t sys_pick_file(uint64_t ureq, uint64_t uout) {
    if (!(current->caps & CAP_PICK)) return -E_PERM;
    if (!afs_mounted()) return -E_LOCKED;
    struct pick_req req;
    if (copy_in(&req, ureq, sizeof req)) return -E_FAULT;
    req.suggested[NAME_MAX] = 0;
    if (req.mode != PICK_OPEN && req.mode != PICK_SAVE) return -E_INVAL;
    if (!user_range_ok(uout, sizeof(struct pick_result), 1)) return -E_FAULT;
    cur_thread->dialog_done = 0;
    int err = wm_dialog_pick(current, (int)req.mode, req.suggested);
    if (err) return err;
    while (!cur_thread->dialog_done) proc_block(WAIT_DIALOG, -1);
    if (!cur_thread->dialog_result) return -E_DENIED; /* cancelled */

    uint32_t ino = cur_thread->dialog_ino;
    if (req.mode == PICK_SAVE) { /* dialog_ino is the folder, dialog_name the file in it */
        uint32_t existing;
        err = afs_lookup(ino, cur_thread->dialog_name, &existing);
        if (err == -E_NOENT) {
            int n = create(ino, cur_thread->dialog_name, AFS_FILE, 0);
            if (n < 0) return n;
            existing = (uint32_t)n;
        } else if (err) {
            return err;
        }
        ino = existing;
    }
    int h = open_ino(ino, req.mode == PICK_SAVE ? O_WRITE : O_READ);
    if (h < 0) return h;
    struct afs_inode n;
    if (afs_iget(ino, &n)) {
        sys_close((uint64_t)h);
        return -E_IO;
    }
    meta_read(&n, "a file"); /* the picker result names the label it holds */
    struct pick_result res = {0};
    strlcpy(res.name, cur_thread->dialog_name, sizeof res.name);
    res.label = n.label;
    dlp_audit("%s was given %s to %s (%s)", current->name, res.name, req.mode == PICK_SAVE ? "save" : "read",
              dlp_label_name(n.label));
    if (copy_out(uout, &res, sizeof res)) {
        sys_close((uint64_t)h);
        return -E_FAULT;
    }
    return h;
}

int64_t sys_fs_info(uint64_t uout) {
    struct fs_info fi;
    afs_info(&fi);
    fi.taint = current->taint;
    /* Global free-space counters are an unlabelled side channel: a tainted
     * app can move them by writing, and a later untainted app can read them.
     * Untrusted apps do not need the exact global allocation totals. */
    if (current->trust == TRUST_UNTRUSTED) {
        fi.total_blocks = fi.free_blocks = 0;
        fi.total_inodes = fi.free_inodes = 0;
    }
    return copy_out(uout, &fi, sizeof fi);
}

int64_t sys_dlp_log(uint64_t ubuf, uint64_t len) {
    if (!(current->caps & CAP_SYSINFO)) return -E_PERM;
    static char tmp[8192];
    len = MIN(len, sizeof tmp);
    if (!user_range_ok(ubuf, len, 1)) return -E_FAULT;
    size_t n = dlp_log_read(tmp, len);
    return copy_out(ubuf, tmp, n) ? -E_FAULT : (int64_t)n;
}
