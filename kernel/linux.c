/*
 * linux.c - a Linux system call layer, so apps built as ordinary static Linux
 * programs (Rust's x86_64-unknown-linux-musl target) run on æther unmodified.
 *
 * This is a personality, not an emulation of Linux: each call is mapped onto
 * æther's own mechanisms and keeps their rules. Paths go through the same
 * capability checks and DLP labels as native calls (an app's private folder
 * appears as /data), memory comes out of the same quota, nothing can be
 * writable and executable, and there is no fork, exec, networking or
 * signal delivery. Anything not implemented fails with ENOSYS.
 *
 * Native æther calls stay reachable from these programs as rax = 0x1000 + N
 * (that's how the eframe stand-in opens windows; see syscall.c).
 */
#include "linux.h"
#include "../include/abi.h"
#include "afs.h"
#include "apps.h"
#include "cpu.h"
#include "dlp.h"
#include "elf.h"
#include "idt.h"
#include "kernel.h"
#include "mm.h"
#include "proc.h"
#include "rng.h"
#include "rtc.h"
#include "uaccess.h"

/* Linux errno values. */
enum {
    EPERM = 1, ENOENT = 2, ESRCH = 3, EINTR = 4, EIO = 5, EBADF = 9, EAGAIN = 11, ENOMEM = 12, EACCES = 13,
    EFAULT = 14, EBUSY = 16, EEXIST = 17, EXDEV = 18, ENODEV = 19, ENOTDIR = 20, EISDIR = 21, EINVAL = 22,
    EMFILE = 24, ENOTTY = 25, ENOSPC = 28, ESPIPE = 29, ERANGE = 34, ENAMETOOLONG = 36, ENOSYS = 38,
    ENOTEMPTY = 39, EAFNOSUPPORT = 97, ETIMEDOUT = 110,
};

/* Linux system call numbers (x86_64). */
enum {
    NR_read = 0, NR_write = 1, NR_open = 2, NR_close = 3, NR_stat = 4, NR_fstat = 5, NR_lstat = 6, NR_poll = 7,
    NR_lseek = 8, NR_mmap = 9, NR_mprotect = 10, NR_munmap = 11, NR_brk = 12, NR_rt_sigaction = 13,
    NR_rt_sigprocmask = 14, NR_rt_sigreturn = 15, NR_ioctl = 16, NR_pread64 = 17, NR_pwrite64 = 18, NR_readv = 19,
    NR_writev = 20, NR_access = 21, NR_pipe = 22, NR_sched_yield = 24, NR_mremap = 25, NR_msync = 26,
    NR_madvise = 28, NR_dup = 32, NR_dup2 = 33, NR_nanosleep = 35, NR_getpid = 39, NR_socket = 41, NR_clone = 56,
    NR_fork = 57, NR_vfork = 58, NR_execve = 59, NR_exit = 60, NR_wait4 = 61, NR_kill = 62, NR_uname = 63,
    NR_fcntl = 72, NR_flock = 73, NR_fsync = 74, NR_fdatasync = 75, NR_truncate = 76, NR_ftruncate = 77,
    NR_getcwd = 79, NR_chdir = 80, NR_fchdir = 81, NR_rename = 82, NR_mkdir = 83, NR_rmdir = 84,
    NR_unlink = 87, NR_readlink = 89, NR_chmod = 90, NR_fchmod = 91, NR_umask = 95, NR_gettimeofday = 96,
    NR_getrlimit = 97, NR_getrusage = 98, NR_sysinfo = 99, NR_getuid = 102, NR_getgid = 104,
    NR_geteuid = 107, NR_getegid = 108, NR_getppid = 110, NR_getpgrp = 111, NR_setsid = 112,
    NR_getresuid = 118, NR_getresgid = 120, NR_sigaltstack = 131, NR_statfs = 137, NR_fstatfs = 138,
    NR_sched_getparam = 143, NR_sched_getscheduler = 145, NR_mlock = 149, NR_munlock = 150, NR_prctl = 157,
    NR_arch_prctl = 158, NR_setrlimit = 160, NR_gettid = 186, NR_tkill = 200, NR_time = 201, NR_futex = 202,
    NR_sched_setaffinity = 203, NR_sched_getaffinity = 204, NR_getdents64 = 217, NR_set_tid_address = 218,
    NR_fadvise64 = 221, NR_clock_gettime = 228, NR_clock_getres = 229, NR_clock_nanosleep = 230,
    NR_exit_group = 231, NR_epoll_wait = 232, NR_tgkill = 234, NR_openat = 257, NR_mkdirat = 258,
    NR_newfstatat = 262, NR_unlinkat = 263, NR_renameat = 264, NR_readlinkat = 267, NR_faccessat = 269,
    NR_pselect6 = 270, NR_ppoll = 271, NR_set_robust_list = 273, NR_get_robust_list = 274, NR_eventfd2 = 290,
    NR_epoll_create1 = 291, NR_dup3 = 292, NR_pipe2 = 293, NR_prlimit64 = 302, NR_getcpu = 309,
    NR_renameat2 = 316, NR_getrandom = 318, NR_memfd_create = 319, NR_membarrier = 324, NR_statx = 332,
    NR_rseq = 334, NR_clone3 = 435, NR_faccessat2 = 439,
};

#define AT_FDCWD (-100)
#define AT_REMOVEDIR 0x200
#define AT_EMPTY_PATH 0x1000

#define L_O_ACCMODE 3
#define L_O_WRONLY 1
#define L_O_RDWR 2
#define L_O_CREAT 0x40
#define L_O_EXCL 0x80
#define L_O_TRUNC 0x200
#define L_O_APPEND 0x400
#define L_O_DIRECTORY 0x10000

#define L_MAP_FIXED 0x10
#define L_MAP_ANONYMOUS 0x20
#define L_MAP_FIXED_NOREPLACE 0x100000

#define STACK_SIZE (8ull << 20)   /* the main thread's stack, made as it's used */
#define BRK_MAX (1ull << 30)      /* the classic heap can grow to 1 GiB */
#define FD_BASE 3                 /* fd = handle + 3; 0, 1 and 2 are the console */

static int64_t errno_of(int64_t e) {
    switch (-e) {
    case E_PERM: return -EACCES;
    case E_FAULT: return -EFAULT;
    case E_INVAL: return -EINVAL;
    case E_NOMEM: return -ENOMEM;
    case E_NOENT: return -ENOENT;
    case E_LIMIT: return -EMFILE;
    case E_TIMEOUT: return -ETIMEDOUT;
    case E_EXIST: return -EEXIST;
    case E_NOTDIR: return -ENOTDIR;
    case E_ISDIR: return -EISDIR;
    case E_NOSPACE: return -ENOSPC;
    case E_IO: return -EIO;
    case E_DENIED: return -EACCES;
    case E_LOCKED: return -EACCES;
    case E_NOTEMPTY: return -ENOTEMPTY;
    default: return e < 0 ? -EINVAL : e;
    }
}

/* ---- Starting a program ------------------------------------------------ */

static uint64_t boot_unix;

int linux_setup(struct proc *p, const struct program *prog, const struct elf_info *ei, uint64_t arg, uint64_t *rsp) {
    /* Reserve the stack; its top two pages exist now for argv and friends. */
    enum { TOP_PAGES = 2 };
    uint64_t lo = USER_STACK_TOP - STACK_SIZE;
    uint64_t lazy = PTE_LAZY | PTE_US | PTE_RW | PTE_NX;
    uint8_t *top[TOP_PAGES];
    uint64_t tables0 = vmm_user_tables;
    for (uint64_t va = lo; va < USER_STACK_TOP; va += PAGE_SIZE) {
        uint64_t *pte = vmm_pte(p->pml4, va, 1);
        if (!pte) return -E_NOMEM;
        uint64_t idx = (USER_STACK_TOP - va) / PAGE_SIZE; /* 1 = top page */
        if (idx <= TOP_PAGES) {
            uint64_t page = pmm_alloc();
            if (!page) return -E_NOMEM;
            *pte = page | PTE_P | PTE_US | PTE_RW | PTE_NX | PTE_QUOTA;
            top[idx - 1] = P2V(page);
            p->mem_pages++;
        } else {
            *pte = lazy;
            p->reserved_pages++;
        }
    }
    /* The 8 MiB stack needs a few page tables of its own; charge them too. */
    p->pt_pages += vmm_user_tables - tables0;
    if (p->mem_pages + p->pt_pages > p->mem_quota_pages) return -E_NOMEM;

    /* Build the initial stack in a kernel buffer that mirrors the top pages. */
    static uint8_t img[TOP_PAGES * PAGE_SIZE];
    memset(img, 0, sizeof img);
    uint64_t base = USER_STACK_TOP - sizeof img, sp = USER_STACK_TOP;
#define PUSH_BYTES(src, n) (sp -= (n), memcpy(img + (sp - base), (src), (n)), sp)
    uint8_t rnd[16];
    random_bytes(rnd, sizeof rnd);
    uint64_t at_random = PUSH_BYTES(rnd, sizeof rnd);
    uint64_t at_platform = PUSH_BYTES("x86_64", 7);

    char envs[12][72];
    int envc = 0;
    snprintf(envs[envc++], 72, "AETHER=1");
    snprintf(envs[envc++], 72, "AETHER_APP=%s", prog->name);
    snprintf(envs[envc++], 72, "HOME=/data");
    snprintf(envs[envc++], 72, "XDG_CONFIG_HOME=/data/config");
    snprintf(envs[envc++], 72, "XDG_DATA_HOME=/data/share");
    snprintf(envs[envc++], 72, "XDG_CACHE_HOME=/data/cache");
    snprintf(envs[envc++], 72, "XDG_STATE_HOME=/data/state");
    snprintf(envs[envc++], 72, "TMPDIR=/data/tmp");
    snprintf(envs[envc++], 72, "LANG=C.UTF-8");
    snprintf(envs[envc++], 72, "PATH=/");
    /* argv[0] is the app name; a spawn argument becomes argv[1] (the probe
     * uses it to pick an attack; ordinary apps see only their name). */
    uint64_t argv[2];
    int argc = 1;
    argv[0] = PUSH_BYTES(prog->name, strlen(prog->name) + 1);
    if (arg) {
        char argbuf[24];
        snprintf(argbuf, sizeof argbuf, "%lu", arg);
        argv[argc++] = PUSH_BYTES(argbuf, strlen(argbuf) + 1);
    }
    uint64_t env_ptr[12];
    for (int i = 0; i < envc; i++) env_ptr[i] = PUSH_BYTES(envs[i], strlen(envs[i]) + 1);
    sp &= ~15ull;

    uint32_t a, b, c, d;
    cpuid(1, 0, &a, &b, &c, &d);
    uint64_t aux[][2] = {
        {3, ei->phdr},  {4, 56},       {5, ei->phnum}, {6, PAGE_SIZE},     {7, 0},  {8, 0},
        {9, ei->entry}, {11, 1000},    {12, 1000},     {13, 1000},         {14, 1000},
        {15, at_platform}, {16, d},    {17, 100},      {23, 0},            {25, at_random},
        {26, 0},        {31, argv[0]}, {0, 0},
    };
    uint64_t words = 1 + (uint64_t)argc + 1 + (uint64_t)envc + 1 + 2 * ARRAY_LEN(aux);
    if (words & 1) sp -= 8; /* rsp must be 16-byte aligned at argc */
    sp -= words * 8;
    if (sp < base) return -E_NOMEM;
    uint64_t *w = (uint64_t *)(img + (sp - base));
    *w++ = (uint64_t)argc;
    for (int i = 0; i < argc; i++) *w++ = argv[i];
    *w++ = 0;
    for (int i = 0; i < envc; i++) *w++ = env_ptr[i];
    *w++ = 0;
    for (unsigned i = 0; i < ARRAY_LEN(aux); i++) {
        *w++ = aux[i][0];
        *w++ = aux[i][1];
    }
    for (int i = 0; i < TOP_PAGES; i++) memcpy(top[i], img + sizeof img - (uint64_t)(i + 1) * PAGE_SIZE, PAGE_SIZE);
    *rsp = sp;

    /* The classic heap (brk) starts a random distance after the image, but
     * it must stay below the mmap bump area: otherwise brk and mmap could
     * hand out the same virtual addresses and silently replace mappings. */
    p->brk_start = p->brk_end = ei->end + ((random_u64() & 0xFF) << 12);
    if (p->brk_start >= USER_MMAP_BASE) p->brk_start = p->brk_end = USER_MMAP_BASE;
    if (!boot_unix) boot_unix = rtc_unix() - ticks / 1000;
    return 0;
}

/* ---- Helpers ----------------------------------------------------------- */

static int copy_cstr(char *dst, uint64_t usrc, size_t max) {
    for (size_t i = 0; i < max; i++) {
        if (copy_in(&dst[i], usrc + i, 1)) return -EFAULT;
        if (!dst[i]) return 0;
    }
    return -ENAMETOOLONG;
}

/*
 * A Linux path becomes an æther path. The app's private folder is /data
 * (and the working directory), so "/data/x" and "x" both mean its file x.
 * Other absolute paths only exist for apps holding CAP_FILES.
 */
static int lpath(uint64_t upath, char *out) {
    char raw[PATH_MAX + 1];
    int err = copy_cstr(raw, upath, sizeof raw);
    if (err) return err;
    if (!raw[0]) return -ENOENT;
    const char *s = raw;
    if (!strncmp(raw, "/data", 5) && (raw[5] == '/' || !raw[5])) {
        s = raw + 5;
        while (*s == '/') s++;
        if (!*s) s = ".";
    } else if (raw[0] == '/') {
        if (!(current->caps & CAP_FILES)) return -ENOENT; /* /proc, /etc, ...: not here */
    } else if (current->caps & CAP_FILES) {
        return -ENOENT;
    }
    strlcpy(out, s, PATH_MAX + 1);
    return 0;
}

static struct file_handle *lfile(int64_t fd) {
    if (fd < FD_BASE || fd >= FD_BASE + MAX_FILES) return 0;
    struct file_handle *f = &current->files[fd - FD_BASE];
    return f->used ? f : 0;
}

static uint64_t now_ns(void) { return ticks * 1000000ull; }
static uint64_t realtime_ns(void) {
    if (!boot_unix) boot_unix = rtc_unix() - ticks / 1000;
    return boot_unix * 1000000000ull + now_ns();
}

struct ltimespec {
    int64_t sec, nsec;
};

static int read_timespec(uint64_t u, uint64_t *ns) {
    struct ltimespec ts;
    if (copy_in(&ts, u, sizeof ts)) return -EFAULT;
    if (ts.sec < 0 || ts.nsec < 0 || ts.nsec >= 1000000000) return -EINVAL;
    *ns = (uint64_t)ts.sec * 1000000000ull + (uint64_t)ts.nsec;
    return 0;
}

static int64_t ns_to_ms(uint64_t ns) { return (int64_t)((ns + 999999) / 1000000); }

/* ---- Console: stdout and stderr go to the system log -------------------- */

static void console_flush(struct proc *p) {
    if (!p->log_len) return;
    p->log_buf[p->log_len] = 0;
    if (ticks - p->log_window >= 1000) {
        p->log_window = ticks;
        p->log_lines = 0;
    }
    if (++p->log_lines <= 40) kprintf("[%s] %s\n", p->name, p->log_buf);
    else if (p->log_lines == 41) kprintf("[%s] (more output this second was dropped)\n", p->name);
    p->log_len = 0;
}

static int64_t console_write(uint64_t ubuf, uint64_t len) {
    struct proc *p = current;
    if (!user_range_ok(ubuf, len, 0)) return -EFAULT;
    /* Data labelled Confidential or above never reaches the log. The app
     * isn't told: Rust's println! would crash it on an error. */
    if (dlp_egress(p, DLP_EGRESS_LOG)) return (int64_t)len;
    char buf[256];
    for (uint64_t done = 0; done < len;) {
        uint64_t n = MIN(len - done, sizeof buf);
        if (copy_in(buf, ubuf + done, n)) return -EFAULT;
        for (uint64_t i = 0; i < n; i++) {
            char ch = buf[i];
            if (ch == '\n' || p->log_len >= (int)sizeof p->log_buf - 1) console_flush(p);
            if (ch == '\n') continue;
            p->log_buf[p->log_len++] = (ch < 32 || ch > 126) ? '?' : ch;
        }
        done += n;
    }
    return (int64_t)len;
}

/* ---- Files -------------------------------------------------------------- */

static int64_t l_openat(int64_t dirfd, uint64_t upath, uint64_t lflags) {
    char path[PATH_MAX + 1];
    int err = lpath(upath, path);
    if (err) return err;
    if (dirfd != AT_FDCWD && path[0] != '/') {
        /* Relative to a folder handle: only the private folder's root is supported. */
        if (!lfile(dirfd)) return -EBADF;
        return -ENOSYS;
    }
    uint64_t acc = lflags & L_O_ACCMODE, flags = 0;
    if (acc == 0) flags = O_READ;
    else if (acc == L_O_WRONLY) flags = O_WRITE;
    else flags = O_READ | O_WRITE;
    if (lflags & L_O_CREAT) flags |= O_CREATE;
    if (lflags & L_O_EXCL) flags |= O_EXCL;
    if ((lflags & L_O_TRUNC) && (flags & O_WRITE)) flags |= O_TRUNC;
    if (lflags & L_O_APPEND) flags |= O_APPEND;
    if (lflags & L_O_DIRECTORY) flags = O_DIR;
    int64_t h = vfs_open(path, flags);
    if (h == -E_ISDIR && acc == 0) h = vfs_open(path, O_DIR); /* Linux lets you open a folder for reading */
    return h < 0 ? errno_of(h) : h + FD_BASE;
}

static int64_t l_close(int64_t fd) {
    if (fd >= 0 && fd < FD_BASE) return 0;
    if (!lfile(fd)) return -EBADF;
    return errno_of(sys_close((uint64_t)(fd - FD_BASE)));
}

static int64_t l_read(int64_t fd, uint64_t buf, uint64_t len) {
    if (fd == 0) return 0; /* stdin: always at its end */
    struct file_handle *f = lfile(fd);
    if (!f) return -EBADF;
    if (f->type == FT_DIR) return -EISDIR;
    return errno_of(sys_read((uint64_t)(fd - FD_BASE), buf, len));
}

static int64_t l_write(int64_t fd, uint64_t buf, uint64_t len) {
    if (fd == 1 || fd == 2) return console_write(buf, len);
    if (!lfile(fd)) return -EBADF;
    return errno_of(sys_write((uint64_t)(fd - FD_BASE), buf, len));
}

struct liovec {
    uint64_t base, len;
};

static int64_t l_rwv(int64_t fd, uint64_t uiov, uint64_t cnt, int write) {
    if (cnt > 1024) return -EINVAL;
    int64_t total = 0;
    for (uint64_t i = 0; i < cnt; i++) {
        struct liovec v;
        if (copy_in(&v, uiov + i * sizeof v, sizeof v)) return -EFAULT;
        if (!v.len) continue;
        int64_t r = write ? l_write(fd, v.base, v.len) : l_read(fd, v.base, v.len);
        if (r < 0) return total ? total : r;
        total += r;
        if ((uint64_t)r < v.len) break;
    }
    return total;
}

static int64_t l_prw(int64_t fd, uint64_t buf, uint64_t len, int64_t off, int write) {
    struct file_handle *f = lfile(fd);
    if (!f) return fd >= 0 && fd < FD_BASE ? -ESPIPE : -EBADF;
    if (off < 0) return -EINVAL;
    uint64_t saved = f->off;
    f->off = (uint64_t)off;
    int64_t r = write ? l_write(fd, buf, len) : l_read(fd, buf, len);
    f->off = saved;
    return r;
}

static int64_t l_lseek(int64_t fd, int64_t off, uint64_t whence) {
    if (fd >= 0 && fd < FD_BASE) return -ESPIPE;
    struct file_handle *f = lfile(fd);
    if (!f) return -EBADF;
    if (f->type == FT_DIR) { /* rewinddir */
        if (off == 0 && whence == SEEK_SET) f->off = 0;
        return (int64_t)f->off;
    }
    return errno_of(sys_seek((uint64_t)(fd - FD_BASE), off, whence));
}

struct lstat {
    uint64_t dev, ino, nlink;
    uint32_t mode, uid, gid, pad0;
    uint64_t rdev;
    int64_t size, blksize, blocks;
    uint64_t atime, atime_ns, mtime, mtime_ns, ctime, ctime_ns;
    int64_t unused[3];
};
_Static_assert(sizeof(struct lstat) == 144, "Linux struct stat");

static int64_t stat_out(uint32_t ino, uint64_t uout) {
    struct lstat st = {0};
    st.dev = 0xae;
    st.nlink = 1;
    st.uid = st.gid = 1000;
    st.blksize = 4096;
    if (ino == 0) { /* the console */
        st.mode = 0020620;
        st.rdev = 0x8801;
    } else {
        struct afs_inode n;
        int err = afs_iget(ino, &n);
        if (err) return errno_of(err);
        vfs_meta_read(ino);
        st.ino = ino;
        st.mode = n.mode == AFS_DIR ? 0040755 : 0100644;
        st.size = n.mode == AFS_DIR ? 4096 : (int64_t)n.size;
        st.blocks = (st.size + 511) / 512;
        st.atime = st.mtime = n.mtime;
        st.ctime = n.ctime;
    }
    return copy_out(uout, &st, sizeof st) ? -EFAULT : 0;
}

static int64_t l_fstat(int64_t fd, uint64_t uout) {
    if (fd >= 0 && fd < FD_BASE) return stat_out(0, uout);
    struct file_handle *f = lfile(fd);
    return f ? stat_out(f->ino, uout) : -EBADF;
}

static int64_t l_fstatat(int64_t dirfd, uint64_t upath, uint64_t uout, uint64_t flags) {
    if (flags & AT_EMPTY_PATH) {
        char c;
        if (!copy_in(&c, upath, 1) && !c) return dirfd == AT_FDCWD ? -EINVAL : l_fstat(dirfd, uout);
    }
    char path[PATH_MAX + 1];
    uint32_t ino;
    int err = lpath(upath, path);
    if (err) return err;
    if (dirfd != AT_FDCWD && path[0] != '/') return -ENOSYS;
    if ((err = vfs_lookup(path, &ino))) return errno_of(err);
    return stat_out(ino, uout);
}

static int64_t l_access(int64_t dirfd, uint64_t upath) {
    char path[PATH_MAX + 1];
    uint32_t ino;
    int err = lpath(upath, path);
    if (err) return err;
    if (dirfd != AT_FDCWD && path[0] != '/') return -ENOSYS;
    return errno_of(vfs_lookup(path, &ino));
}

static int64_t l_getdents64(int64_t fd, uint64_t ubuf, uint64_t len) {
    struct file_handle *f = lfile(fd);
    if (!f) return -EBADF;
    if (f->type != FT_DIR) return -ENOTDIR;
    vfs_meta_read(f->ino); /* names are the folder's data */
    uint64_t done = 0;
    for (;;) {
        char name[NAME_MAX + 2];
        uint32_t ino;
        uint8_t type;
        int r = afs_dir_entry(f->ino, (uint32_t)f->off, name, &ino, &type);
        if (r < 0) return done ? (int64_t)done : errno_of(r);
        if (r == 0) break;
        vfs_meta_read(ino); /* the name/label is data too */
        size_t nlen = strlen(name);
        uint64_t reclen = ALIGN_UP(19 + nlen + 1, 8);
        if (done + reclen > len) {
            if (!done) return -EINVAL;
            break;
        }
        uint8_t rec[96] = {0};
        *(uint64_t *)&rec[0] = ino;
        *(int64_t *)&rec[8] = (int64_t)f->off + 1;
        *(uint16_t *)&rec[16] = (uint16_t)reclen;
        rec[18] = type == AFS_DIR ? 4 : 8; /* DT_DIR / DT_REG */
        memcpy(&rec[19], name, nlen + 1);
        if (copy_out(ubuf + done, rec, reclen)) return -EFAULT;
        done += reclen;
        f->off++;
    }
    return (int64_t)done;
}

static int64_t l_ftruncate(int64_t fd, uint64_t len) {
    struct file_handle *f = lfile(fd);
    if (!f) return -EBADF;
    if (!(f->flags & O_WRITE) || f->type != FT_FILE) return -EINVAL;
    int err = vfs_label_write(f->ino); /* a new length is written data too */
    if (err) return errno_of(err);
    f->dirty = 1;
    return errno_of(afs_truncate(f->ino, len));
}

static int64_t l_mkdirat(int64_t dirfd, uint64_t upath) {
    char path[PATH_MAX + 1];
    int err = lpath(upath, path);
    if (err) return err;
    if (dirfd != AT_FDCWD && path[0] != '/') return -ENOSYS;
    return errno_of(vfs_mkdir(path));
}

static int64_t l_unlinkat(int64_t dirfd, uint64_t upath, uint64_t flags) {
    char path[PATH_MAX + 1];
    uint32_t ino;
    int err = lpath(upath, path);
    if (err) return err;
    if (dirfd != AT_FDCWD && path[0] != '/') return -ENOSYS;
    if ((err = vfs_lookup(path, &ino))) return errno_of(err);
    struct afs_inode n;
    if ((err = afs_iget(ino, &n))) return errno_of(err);
    if ((flags & AT_REMOVEDIR) && n.mode != AFS_DIR) return -ENOTDIR;
    if (!(flags & AT_REMOVEDIR) && n.mode == AFS_DIR) return -EISDIR;
    err = vfs_remove(path);
    return err == -E_LIMIT ? -EBUSY : errno_of(err);
}

static int64_t l_renameat(int64_t olddir, uint64_t uold, int64_t newdir, uint64_t unew, uint64_t flags) {
    char from[PATH_MAX + 1], to[PATH_MAX + 1];
    int err = lpath(uold, from);
    if (err || (err = lpath(unew, to))) return err;
    if (flags) return -EINVAL;
    if ((olddir != AT_FDCWD && from[0] != '/') || (newdir != AT_FDCWD && to[0] != '/')) return -ENOSYS;
    uint32_t src, ino;
    if ((err = vfs_lookup(from, &src))) return errno_of(err);
    /* Linux rename("x", "x") is a no-op. Do not remove the target in that
     * case, or the source (the same file) would be deleted. */
    if (!vfs_lookup(to, &ino) && ino == src) return 0;
    /* Linux replaces an existing target file; æther refuses, so remove it first. */
    if (!vfs_lookup(to, &ino)) {
        struct afs_inode n;
        if (!afs_iget(ino, &n) && n.mode == AFS_FILE && (err = vfs_remove(to))) return errno_of(err);
    }
    return errno_of(vfs_rename(from, to));
}

static int64_t l_fcntl(int64_t fd, uint64_t cmd, uint64_t arg) {
    (void)arg;
    struct file_handle *f = lfile(fd);
    if (!f && !(fd >= 0 && fd < FD_BASE)) return -EBADF;
    switch (cmd) {
    case 1: return 1;  /* F_GETFD: close-on-exec (there is no exec) */
    case 2: return 0;  /* F_SETFD */
    case 3:            /* F_GETFL */
        if (!f) return fd == 0 ? 0 : 1;
        return (f->flags & O_READ) && (f->flags & O_WRITE) ? 2 : (f->flags & O_WRITE) ? 1 : 0;
    case 4: return 0;  /* F_SETFL */
    case 0:            /* F_DUPFD */
    case 1030: {       /* F_DUPFD_CLOEXEC */
        if (!f) return -EINVAL;
        for (int i = 0; i < MAX_FILES; i++)
            if (!current->files[i].used) {
                current->files[i] = *f; /* its own position, same file and rights */
                return i + FD_BASE;
            }
        return -EMFILE;
    }
    default: return -EINVAL;
    }
}

struct lpollfd {
    int32_t fd;
    int16_t events, revents;
};

static int64_t l_poll(uint64_t ufds, uint64_t n, int64_t timeout_ms) {
    if (n > 256) return -EINVAL;
    int64_t ready = 0;
    for (uint64_t i = 0; i < n; i++) {
        struct lpollfd p;
        if (copy_in(&p, ufds + i * sizeof p, sizeof p)) return -EFAULT;
        p.revents = 0;
        if (p.fd >= 0) {
            if (p.fd < FD_BASE || lfile(p.fd)) p.revents = (int16_t)(p.events & 0x5); /* POLLIN | POLLOUT */
            else p.revents = 0x20;                                                   /* POLLNVAL */
            if (p.fd == 0) p.revents = (int16_t)(p.events & 0x4);                    /* stdin never has data */
        }
        if (p.revents) ready++;
        if (copy_out(ufds + i * sizeof p, &p, sizeof p)) return -EFAULT;
    }
    if (!ready && timeout_ms != 0) proc_block(WAIT_SLEEP, timeout_ms < 0 ? -1 : timeout_ms);
    return ready;
}

/* Copy a file into fresh memory: mmap of a file, MAP_PRIVATE only. */
static int64_t map_file(uint64_t addr, uint64_t len, int64_t fd, uint64_t off) {
    struct file_handle *f = lfile(fd);
    if (!f || f->type != FT_FILE || !(f->flags & O_READ)) return -EBADF;
    struct afs_inode n;
    int err = afs_iget(f->ino, &n);
    if (err) return errno_of(err);
    static uint8_t buf[4096];
    for (uint64_t done = 0; off + done < n.size && done < len;) {
        int64_t got = afs_read(f->ino, off + done, buf, MIN((uint64_t)sizeof buf, len - done));
        if (got <= 0) break;
        if (copy_out(addr + done, buf, (uint64_t)got)) return -EFAULT;
        done += (uint64_t)got;
    }
    memset(buf, 0, sizeof buf);
    dlp_taint(current, n.label, "a mapped file");
    return 0;
}

/* ---- Memory ------------------------------------------------------------- */

static int prot_ok(uint64_t prot) {
    if (prot & ~7ull) return 0;
    if ((prot & PROT_WRITE) && (prot & PROT_EXEC)) return 0;              /* W^X, always */
    if ((prot & PROT_EXEC) && current->trust == TRUST_UNTRUSTED) return 0; /* only the code it shipped with */
    return 1;
}

/* Ranges an app may manage itself: below the window buffers. */
static int user_area(uint64_t addr, uint64_t len) {
    return !(addr & 0xFFF) && len && addr >= 0x10000 && addr < USER_TOP && len <= USER_TOP - addr &&
           (addr + len <= USER_WIN_BASE || addr >= USER_STACK_TOP - STACK_SIZE);
}

static int64_t l_mmap(uint64_t addr, uint64_t len, uint64_t prot, uint64_t flags, int64_t fd, uint64_t off) {
    if (!len || len > (1ull << 36)) return -EINVAL;
    len = ALIGN_UP(len, PAGE_SIZE);
    if (!prot_ok(prot)) return -EACCES;
    int file = !(flags & L_MAP_ANONYMOUS);
    if (file && ((off & 0xFFF) || !lfile(fd))) return file && !lfile(fd) ? -EBADF : -EINVAL;
    if (file && (flags & 3) != 2) return -ENODEV; /* shared file mappings: no */
    uint64_t va;
    if (flags & (L_MAP_FIXED | L_MAP_FIXED_NOREPLACE)) {
        if (!user_area(addr, len) || addr + len > USER_MMAP_TOP || umem_has_shared(addr, len)) return -EINVAL;
        if (!umem_range_free(addr, len)) {
            if (flags & L_MAP_FIXED_NOREPLACE) return -EEXIST;
            umem_release(addr, len);
        }
        umem_claim_va(addr, len);
        va = addr;
    } else {
        if (!(va = umem_alloc_va(len))) return -ENOMEM;
    }
    int err = umem_reserve(va, len, file ? (PROT_READ | PROT_WRITE) : prot);
    if (err) return errno_of(err);
    if (file) {
        int64_t r = map_file(va, len, fd, off);
        if (r || (r = umem_protect(va, len, prot))) {
            umem_release(va, len);
            return errno_of(r);
        }
    }
    return (int64_t)va;
}

static int64_t l_munmap(uint64_t addr, uint64_t len) {
    len = ALIGN_UP(len, PAGE_SIZE);
    if (!user_area(addr, len) || umem_has_shared(addr, len)) return -EINVAL;
    umem_release(addr, len);
    return 0;
}

static int64_t l_mprotect(uint64_t addr, uint64_t len, uint64_t prot) {
    len = ALIGN_UP(len, PAGE_SIZE);
    if (!len) return 0;
    if (!user_area(addr, len)) return -EINVAL;
    if (!prot_ok(prot)) return -EACCES;
    return errno_of(umem_protect(addr, len, prot));
}

static int64_t l_madvise(uint64_t addr, uint64_t len, uint64_t advice) {
    len = ALIGN_UP(len, PAGE_SIZE);
    if (advice != 4 && advice != 8) return 0; /* only DONTNEED and FREE change anything */
    if (!user_area(addr, len) || umem_has_shared(addr, len)) return -EINVAL;
    /* Give the pages back; the range reads as zeros again when next touched. */
    for (uint64_t va = addr; va < addr + len; va += PAGE_SIZE) {
        uint64_t *pte = vmm_pte(current->pml4, va, 0);
        if (!pte || !(*pte & PTE_P) || !(*pte & PTE_QUOTA)) continue;
        uint64_t e = *pte;
        pmm_free(e & PTE_ADDR);
        current->mem_pages--;
        current->reserved_pages++;
        *pte = PTE_LAZY | (e & (PTE_US | PTE_RW | PTE_NX));
        invlpg(va);
    }
    return 0;
}

static int64_t l_mremap(uint64_t old, uint64_t oldlen, uint64_t newlen, uint64_t flags) {
    if ((old & 0xFFF) || (flags & ~1ull) || !newlen) return -EINVAL;
    oldlen = ALIGN_UP(oldlen, PAGE_SIZE);
    newlen = ALIGN_UP(newlen, PAGE_SIZE);
    if (!oldlen || !user_area(old, oldlen)) return -EINVAL;
    int64_t r = umem_remap(old, oldlen, newlen, (int)(flags & 1));
    return r < 0 ? errno_of(r) : r;
}

static int64_t l_brk(uint64_t want) {
    struct proc *p = current;
    uint64_t limit = MIN(p->brk_start + BRK_MAX, USER_MMAP_BASE);
    if (p->brk_start >= USER_MMAP_BASE || want < p->brk_start || want > limit) return (int64_t)p->brk_end;
    uint64_t have = ALIGN_UP(p->brk_end, PAGE_SIZE), need = ALIGN_UP(want, PAGE_SIZE);
    if (need > have) {
        if (!umem_range_free(have, need - have) || umem_reserve(have, need - have, PROT_READ | PROT_WRITE))
            return (int64_t)p->brk_end;
    } else if (need < have) {
        umem_release(need, have - need);
    }
    p->brk_end = want;
    return (int64_t)want;
}

/* ---- Threads and time --------------------------------------------------- */

#define CLONE_VM 0x100
#define CLONE_SIGHAND 0x800
#define CLONE_VFORK 0x4000
#define CLONE_THREAD 0x10000
#define CLONE_SETTLS 0x80000
#define CLONE_PARENT_SETTID 0x100000
#define CLONE_CHILD_CLEARTID 0x200000
#define CLONE_CHILD_SETTID 0x1000000

static int64_t l_clone(struct isr_frame *f, uint64_t flags, uint64_t stack, uint64_t ptid, uint64_t ctid, uint64_t tls) {
    /* Threads only: no fork, so no second copy of an app's memory. */
    uint64_t need = CLONE_VM | CLONE_SIGHAND | CLONE_THREAD;
    if ((flags & need) != need || (flags & CLONE_VFORK) || !stack) return -ENOSYS;
    /* The new thread's first iretq loads this stack pointer. A non-canonical
     * value would fault in ring 0 and panic the kernel. */
    if (stack >= USER_TOP) return -EINVAL;
    /* The new thread's FS base is loaded with wrmsr on every switch to it: a
     * non-canonical value would fault inside the kernel. Same rule as ARCH_SET_FS. */
    if ((flags & CLONE_SETTLS) && tls >= USER_TOP) return -EPERM;
    int tid = thread_create(f, stack, tls, (flags & CLONE_SETTLS) != 0, (flags & CLONE_CHILD_CLEARTID) ? ctid : 0);
    if (tid < 0) return tid == -E_LIMIT ? -EAGAIN : tid == -E_INVAL ? -EINVAL : -ENOMEM;
    uint32_t t32 = (uint32_t)tid;
    if (flags & CLONE_PARENT_SETTID) copy_out(ptid, &t32, sizeof t32);
    if (flags & CLONE_CHILD_SETTID) copy_out(ctid, &t32, sizeof t32);
    return tid;
}

static int64_t l_futex(uint64_t uaddr, uint64_t op, uint64_t val, uint64_t utime, uint64_t uaddr2, uint64_t val3) {
    if (uaddr & 3) return -EINVAL;
    uint64_t cmd = op & 127;
    switch (cmd) {
    case 0:   /* FUTEX_WAIT: relative timeout */
    case 9: { /* FUTEX_WAIT_BITSET: absolute timeout */
        uint32_t cur;
        if (copy_in(&cur, uaddr, sizeof cur)) return -EFAULT;
        if (cur != (uint32_t)val) return -EAGAIN;
        int64_t ms = -1;
        if (utime) {
            uint64_t ns;
            int err = read_timespec(utime, &ns);
            if (err) return err;
            if (cmd == 9) {
                uint64_t now = (op & 256) ? realtime_ns() : now_ns();
                if (ns <= now) return -ETIMEDOUT;
                ns -= now;
            }
            ms = ns_to_ms(ns);
        }
        cur_thread->futex_addr = uaddr;
        cur_thread->futex_woken = 0;
        proc_block(WAIT_FUTEX, ms);
        return cur_thread->futex_woken ? 0 : -ETIMEDOUT;
    }
    case 1:  /* FUTEX_WAKE */
    case 10: /* FUTEX_WAKE_BITSET */
        return futex_wake(current, uaddr, (int)MIN(val, 0x7FFFFFFFull));
    case 3: /* FUTEX_REQUEUE */
        return futex_requeue(current, uaddr, uaddr2, (int)MIN(val, 0x7FFFFFFFull), (int)MIN(utime, 0x7FFFFFFFull));
    case 4: { /* FUTEX_CMP_REQUEUE */
        uint32_t cur;
        if (copy_in(&cur, uaddr, sizeof cur)) return -EFAULT;
        if (cur != (uint32_t)val3) return -EAGAIN;
        return futex_requeue(current, uaddr, uaddr2, (int)MIN(val, 0x7FFFFFFFull), (int)MIN(utime, 0x7FFFFFFFull));
    }
    default: return -ENOSYS;
    }
}

static int64_t l_clock_gettime(uint64_t clk, uint64_t uts) {
    uint64_t ns = (clk == 0 || clk == 5 || clk == 8 || clk == 11) ? realtime_ns() : now_ns();
    struct ltimespec ts = {(int64_t)(ns / 1000000000ull), (int64_t)(ns % 1000000000ull)};
    return copy_out(uts, &ts, sizeof ts) ? -EFAULT : 0;
}

static int64_t l_sleep(uint64_t clk, uint64_t flags, uint64_t ureq) {
    uint64_t ns;
    int err = read_timespec(ureq, &ns);
    if (err) return err;
    if (flags & 1) { /* TIMER_ABSTIME */
        uint64_t now = clk == 0 ? realtime_ns() : now_ns();
        if (ns <= now) return 0;
        ns -= now;
    }
    if (ns) proc_block(WAIT_SLEEP, MAX(ns_to_ms(ns), 1));
    return 0;
}

struct lutsname {
    char sysname[65], nodename[65], release[65], version[65], machine[65], domainname[65];
};

static int64_t l_uname(uint64_t uout) {
    struct lutsname u;
    memset(&u, 0, sizeof u);
    strlcpy(u.sysname, "Linux", sizeof u.sysname); /* the ABI it speaks */
    strlcpy(u.nodename, "aether", sizeof u.nodename);
    strlcpy(u.release, "6.0.0-aether", sizeof u.release);
    strlcpy(u.version, "aether " AETHER_VERSION, sizeof u.version);
    strlcpy(u.machine, "x86_64", sizeof u.machine);
    return copy_out(uout, &u, sizeof u) ? -EFAULT : 0;
}

static int64_t l_prlimit(uint64_t res, uint64_t unew, uint64_t uold) {
    (void)unew; /* limits can't be raised; lowering requests are accepted and ignored */
    if (!uold) return 0;
    uint64_t lim[2] = {~0ull, ~0ull};
    if (res == 3) lim[0] = lim[1] = STACK_SIZE;                                 /* RLIMIT_STACK */
    else if (res == 7) lim[0] = lim[1] = FD_BASE + MAX_FILES;                   /* RLIMIT_NOFILE */
    else if (res == 9 || res == 2) lim[0] = lim[1] = current->mem_quota_pages * PAGE_SIZE; /* AS, DATA */
    else if (res == 6) lim[0] = lim[1] = 32;                                    /* RLIMIT_NPROC */
    return copy_out(uold, lim, sizeof lim) ? -EFAULT : 0;
}

static int64_t l_kill_self(uint64_t pid_or_tid, uint64_t sig) {
    if (!sig) return 0;
    int own = (int)pid_or_tid == current->pid || (int)pid_or_tid == cur_thread->tid || pid_or_tid == 0;
    if (!own) return -EPERM; /* apps can't signal each other */
    char why[64];
    snprintf(why, sizeof why, sig == 6 ? "stopped itself (abort)" : "stopped itself (signal %lu)", sig);
    proc_kill_current(EXIT_KILLED_BASE + (int)(sig & 31), why);
}

static int64_t zero_out(uint64_t u, uint64_t len) {
    static const uint8_t zeros[256];
    if (u && len && copy_out(u, zeros, MIN(len, sizeof zeros))) return -EFAULT;
    return 0;
}

/* ---- Dispatch ----------------------------------------------------------- */

void linux_syscall(struct isr_frame *f) {
    uint64_t nr = f->rax, a = f->rdi, b = f->rsi, c = f->rdx, d = f->r10, e = f->r8, g = f->r9;
    int64_t r;
    switch (nr) {
    case NR_read: r = l_read((int64_t)a, b, c); break;
    case NR_write: r = l_write((int64_t)a, b, c); break;
    case NR_open: r = l_openat(AT_FDCWD, a, b); break;
    case NR_openat: r = l_openat((int64_t)(int32_t)a, b, c); break;
    case NR_close: r = l_close((int64_t)a); break;
    case NR_stat:
    case NR_lstat: r = l_fstatat(AT_FDCWD, a, b, 0); break;
    case NR_fstat: r = l_fstat((int64_t)a, b); break;
    case NR_newfstatat: r = l_fstatat((int64_t)(int32_t)a, b, c, d); break;
    case NR_statx: r = -ENOSYS; break; /* callers fall back to fstatat */
    case NR_poll: r = l_poll(a, b, (int64_t)(int32_t)c); break;
    case NR_ppoll: {
        int64_t ms = -1;
        uint64_t ns;
        if (c) {
            int err = read_timespec(c, &ns);
            if (err) {
                r = err;
                break;
            }
            ms = ns_to_ms(ns);
        }
        r = l_poll(a, b, ms);
        break;
    }
    case NR_lseek: r = l_lseek((int64_t)a, (int64_t)b, c); break;
    case NR_pread64: r = l_prw((int64_t)a, b, c, (int64_t)d, 0); break;
    case NR_pwrite64: r = l_prw((int64_t)a, b, c, (int64_t)d, 1); break;
    case NR_readv: r = l_rwv((int64_t)a, b, c, 0); break;
    case NR_writev: r = l_rwv((int64_t)a, b, c, 1); break;
    case NR_access: r = l_access(AT_FDCWD, a); break;
    case NR_faccessat:
    case NR_faccessat2: r = l_access((int64_t)(int32_t)a, b); break;
    case NR_getdents64: r = l_getdents64((int64_t)a, b, c); break;
    case NR_fcntl: r = l_fcntl((int64_t)a, b, c); break;
    case NR_dup: r = l_fcntl((int64_t)a, 0, 0); break;
    case NR_ioctl: r = -ENOTTY; break;
    case NR_fsync:
    case NR_fdatasync:
        if (!lfile((int64_t)a) && a >= FD_BASE) { r = -EBADF; break; }
        afs_sync();
        r = 0;
        break;
    case NR_ftruncate: r = l_ftruncate((int64_t)a, b); break;
    case NR_mkdir: r = l_mkdirat(AT_FDCWD, a); break;
    case NR_mkdirat: r = l_mkdirat((int64_t)(int32_t)a, b); break;
    case NR_unlink: r = l_unlinkat(AT_FDCWD, a, 0); break;
    case NR_rmdir: r = l_unlinkat(AT_FDCWD, a, AT_REMOVEDIR); break;
    case NR_unlinkat: r = l_unlinkat((int64_t)(int32_t)a, b, c); break;
    case NR_rename: r = l_renameat(AT_FDCWD, a, AT_FDCWD, b, 0); break;
    case NR_renameat: r = l_renameat((int64_t)(int32_t)a, b, (int64_t)(int32_t)c, d, 0); break;
    case NR_renameat2: r = l_renameat((int64_t)(int32_t)a, b, (int64_t)(int32_t)c, d, e); break;
    case NR_getcwd:
        if (b < 6) r = -ERANGE;
        else r = copy_out(a, "/data", 6) ? -EFAULT : (int64_t)a;
        break;
    case NR_chdir: r = -EACCES; break;
    case NR_readlink:
    case NR_readlinkat: r = -ENOENT; break; /* no symbolic links, no /proc/self/exe */
    case NR_chmod:
    case NR_fchmod: r = 0; break;
    case NR_umask: r = 022; break;

    case NR_mmap: r = l_mmap(a, b, c, d, (int64_t)(int32_t)e, g); break;
    case NR_munmap: r = l_munmap(a, b); break;
    case NR_mprotect: r = l_mprotect(a, b, c); break;
    case NR_mremap: r = l_mremap(a, b, c, d); break;
    case NR_madvise: r = l_madvise(a, b, c); break;
    case NR_brk: r = l_brk(a); break;
    case NR_msync:
    case NR_mlock:
    case NR_munlock: r = 0; break;

    case NR_rt_sigaction: r = zero_out(c, 32); break;   /* accepted; signals are never delivered */
    case NR_rt_sigprocmask: r = zero_out(c, MIN(d, 8ull)); break;
    case NR_sigaltstack: {
        uint64_t oss[3] = {0, 2 /* SS_DISABLE */, 0};
        r = b && copy_out(b, oss, sizeof oss) ? -EFAULT : 0;
        break;
    }
    case NR_rt_sigreturn: proc_kill_current(EXIT_KILLED_BASE, "crashed: returned from a signal it never got");
    case NR_kill: r = l_kill_self(a, b); break;
    case NR_tkill: r = l_kill_self(a, b); break;
    case NR_tgkill: r = l_kill_self(b, c); break;

    case NR_clone: r = l_clone(f, a, b, c, d, e); break;
    case NR_clone3:
    case NR_fork:
    case NR_vfork:
    case NR_execve:
    case NR_wait4: r = -ENOSYS; break;
    case NR_exit: thread_exit_current((int)a);
    case NR_exit_group: proc_exit_current((int)a); r = 0; break;
    case NR_set_tid_address: cur_thread->clear_child_tid = a; r = cur_thread->tid; break;
    case NR_set_robust_list: r = 0; break;
    case NR_futex: r = l_futex(a, b, c, d, e, g); break;
    case NR_arch_prctl:
        if (a == 0x1002) { /* ARCH_SET_FS */
            if (b >= USER_TOP) { r = -EPERM; break; }
            cur_thread->fs_base = b;
            wrmsr(0xC0000100, b);
            r = 0;
        } else if (a == 0x1003) { /* ARCH_GET_FS */
            r = copy_out(b, &cur_thread->fs_base, 8) ? -EFAULT : 0;
        } else {
            r = -EINVAL;
        }
        break;
    case NR_prctl:
        if (a == 15) r = 0; /* PR_SET_NAME */
        else if (a == 16) r = copy_out(b, current->name, 16) ? -EFAULT : 0;
        else r = -EINVAL;
        break;
    case NR_sched_yield: schedule(); r = 0; break;
    case NR_sched_getaffinity: {
        uint64_t mask = 1; /* one processor */
        if (b < 8) r = -EINVAL;
        else r = copy_out(c, &mask, 8) ? -EFAULT : 8;
        break;
    }
    case NR_sched_setaffinity: r = 0; break;
    case NR_getcpu: {
        uint32_t zero = 0;
        if (a) copy_out(a, &zero, 4);
        if (b) copy_out(b, &zero, 4);
        r = 0;
        break;
    }
    case NR_membarrier: r = 0; break;
    case NR_rseq: r = -ENOSYS; break;

    case NR_getpid: r = current->pid; break;
    case NR_gettid: r = cur_thread->tid; break;
    case NR_getppid: r = current->parent; break;
    case NR_getpgrp: r = current->pid; break;
    case NR_getuid:
    case NR_geteuid:
    case NR_getgid:
    case NR_getegid: r = 1000; break;
    case NR_getresuid:
    case NR_getresgid: {
        uint32_t id = 1000;
        r = copy_out(a, &id, 4) || copy_out(b, &id, 4) || copy_out(c, &id, 4) ? -EFAULT : 0;
        break;
    }
    case NR_uname: r = l_uname(a); break;
    case NR_getrlimit: r = l_prlimit(a, 0, b); break;
    case NR_setrlimit: r = 0; break;
    case NR_prlimit64: r = (a && (int)a != current->pid) ? -EPERM : l_prlimit(b, c, d); break;
    case NR_getrusage: r = zero_out(b, 144); break;

    case NR_getrandom: {
        uint8_t buf[256];
        uint64_t done = 0;
        r = 0;
        while (done < b) {
            uint64_t n = MIN(b - done, sizeof buf);
            random_bytes(buf, n);
            if (copy_out(a + done, buf, n)) { r = -EFAULT; break; }
            done += n;
        }
        memset(buf, 0, sizeof buf);
        if (!r) r = (int64_t)done;
        break;
    }
    case NR_clock_gettime: r = l_clock_gettime(a, b); break;
    case NR_clock_getres: {
        struct ltimespec ts = {0, 1000000};
        r = b && copy_out(b, &ts, sizeof ts) ? -EFAULT : 0;
        break;
    }
    case NR_gettimeofday: {
        uint64_t ns = realtime_ns();
        int64_t tv[2] = {(int64_t)(ns / 1000000000ull), (int64_t)(ns % 1000000000ull / 1000)};
        r = a && copy_out(a, tv, sizeof tv) ? -EFAULT : 0;
        break;
    }
    case NR_time: {
        int64_t t = (int64_t)(realtime_ns() / 1000000000ull);
        r = a && copy_out(a, &t, 8) ? -EFAULT : t;
        break;
    }
    case NR_nanosleep: r = l_sleep(1, 0, a); break;
    case NR_clock_nanosleep: r = l_sleep(a, b, c); break;

    case NR_socket: r = -EAFNOSUPPORT; break; /* networking comes later, behind the firewall */
    case NR_fadvise64: r = 0; break;

    default: {
        static uint64_t logged[8]; /* each unknown call is logged once */
        if (nr < 512 && !(logged[nr / 64] & (1ull << (nr % 64)))) {
            logged[nr / 64] |= 1ull << (nr % 64);
            kprintf("[linux] %s: system call %lu isn't supported\n", current->name, nr);
        }
        r = -ENOSYS;
        break;
    }
    }
    f->rax = (uint64_t)r;
}
