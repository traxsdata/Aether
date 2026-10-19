/*
 * syscall.c - the system call layer: the only door from applications into
 * the kernel, so every call checks two things before doing anything:
 *   1. the caller holds the capability the call needs, and
 *   2. every pointer it passed lies in its own user memory with the right
 *      permissions (checked against its page tables, never trusted).
 * User memory is only touched through copy_in/copy_out, which open the
 * SMAP window (stac/clac) for exactly the length of the copy.
 */
#include "../include/abi.h"
#include "afs.h"
#include "apps.h"
#include "appstore.h"
#include "cpu.h"
#include "crypto.h"
#include "dlp.h"
#include "gdt.h"
#include "idt.h"
#include "kernel.h"
#include "linux.h"
#include "ktest.h"
#include "mm.h"
#include "proc.h"
#include "power.h"
#include "rtc.h"
#include "settings.h"
#include "update.h"
#include "uaccess.h"
#include "net.h"
#include "wm.h"

extern void syscall_entry(void);

void syscall_init(void) {
    wrmsr(MSR_EFER, rdmsr(MSR_EFER) | EFER_SCE);
    /* STAR: kernel CS/SS from 0x08, user CS/SS base 0x10 (so CS = 0x20|3, SS = 0x18|3). */
    wrmsr(MSR_STAR, ((uint64_t)0x10 << 48) | ((uint64_t)SEL_KCODE << 32));
    wrmsr(MSR_LSTAR, (uint64_t)syscall_entry);
    /* Clear IF, DF, TF and AC on entry: no interrupts until we're on the kernel
     * stack, and AC=0 so SMAP protection is active inside the kernel. */
    wrmsr(MSR_SFMASK, 0x200 | 0x400 | 0x100 | 0x40000);
}

/* Is [addr, addr+len) mapped user memory (writable if `write`) in the caller? */
int user_range_ok(uint64_t addr, uint64_t len, int write) {
    if (len == 0) return 1;
    if (addr < 0x1000 || addr >= USER_TOP || len > USER_TOP - addr) return 0;
    for (uint64_t page = addr & ~0xFFFull; page < addr + len; page += PAGE_SIZE) {
        uint64_t flags;
        /* Reserved memory the app hasn't touched yet is made now. */
        if (!vmm_translate(current->pml4, page, &flags) &&
            (umem_fault(page, write, 0) || !vmm_translate(current->pml4, page, &flags)))
            return 0;
        if (!(flags & PTE_US) || (write && !(flags & PTE_RW))) return 0;
    }
    return 1;
}

/* stac/clac only exist on CPUs with SMAP. */
static inline void user_access_begin(void) {
    if (cpu_features & CPU_FEAT_SMAP) stac();
}
static inline void user_access_end(void) {
    if (cpu_features & CPU_FEAT_SMAP) clac();
}

int copy_in(void *dst, uint64_t usrc, uint64_t len) {
    if (!user_range_ok(usrc, len, 0)) return -E_FAULT;
    user_access_begin();
    memcpy(dst, (const void *)usrc, len);
    user_access_end();
    return 0;
}

int copy_out(uint64_t udst, const void *src, uint64_t len) {
    if (!user_range_ok(udst, len, 1)) return -E_FAULT;
    user_access_begin();
    memcpy((void *)udst, src, len);
    user_access_end();
    return 0;
}

int copy_path(char *dst, uint64_t usrc, uint64_t len) {
    if (len == 0 || len > PATH_MAX) return -E_INVAL;
    if (copy_in(dst, usrc, len)) return -E_FAULT;
    dst[len] = 0;
    if (strlen(dst) != len) return -E_INVAL; /* embedded NUL */
    return 0;
}

#define REQUIRE(cap)                                  \
    do {                                              \
        if (!(current->caps & (cap))) return -E_PERM; \
    } while (0)

static int64_t sys_log(uint64_t msg, uint64_t len) {
    REQUIRE(CAP_LOG);
    char buf[256];
    len = MIN(len, sizeof buf - 1);
    if (copy_in(buf, msg, len)) return -E_FAULT;
    buf[len] = 0;
    if (dlp_egress(current, DLP_EGRESS_LOG)) return -E_DENIED;
    for (uint64_t i = 0; i < len; i++)
        if ((buf[i] < 32 || buf[i] > 126) && buf[i] != '\n') buf[i] = '?'; /* no control characters */
    kprintf("[%s] %s\n", current->name, buf);
    return 0;
}

static int64_t sys_win_create(uint64_t ureq, uint64_t uinfo) {
    REQUIRE(CAP_WINDOW);
    struct win_req req;
    if (copy_in(&req, ureq, sizeof req)) return -E_FAULT;
    req.title[WIN_TITLE_MAX - 1] = 0;
    if (req.width < 16 || req.height < 16 || req.width > WIN_MAX_W || req.height > WIN_MAX_H) return -E_INVAL;
    struct win_info info;
    /* Validate the output before creating anything: otherwise a bad pointer
     * leaks a window slot and its pixel buffer on every failed call. */
    if (!user_range_ok(uinfo, sizeof info, 1)) return -E_FAULT;
    int h = wm_create_window(current, req.width, req.height, req.title, &info);
    if (h < 0) return h;
    if (copy_out(uinfo, &info, sizeof info)) return -E_FAULT;
    return h;
}

static int64_t sys_win_resize(uint64_t handle, uint64_t uinfo) {
    if (!user_range_ok(uinfo, sizeof(struct win_info), 1)) return -E_FAULT;
    struct win_info info;
    int r = wm_win_resize(current, (int)handle, &info);
    if (r < 0) return r;
    return copy_out(uinfo, &info, sizeof info) ? -E_FAULT : 0;
}

static int64_t sys_event_wait(uint64_t uev, int64_t timeout_ms) {
    if (!user_range_ok(uev, sizeof(struct event), 1)) return -E_FAULT;
    if (current->ev_head == current->ev_tail && timeout_ms != 0) proc_block(WAIT_EVENT, timeout_ms);
    if (current->ev_head == current->ev_tail) return 0;
    struct event ev = current->events[current->ev_tail % EVENT_QUEUE_LEN];
    current->ev_tail++;
    if (ev.type == EV_WAKE) current->wake_queued = 0;
    if (copy_out(uev, &ev, sizeof ev)) return -E_FAULT;
    return 1;
}

static int64_t sys_time(uint64_t uout) {
    struct time_info t = {0};
    struct rtc_time now;
    rtc_now(&now);
    t.uptime_ms = ticks;
    t.year = now.year;
    t.month = now.month;
    t.day = now.day;
    t.hour = now.hour;
    t.minute = now.minute;
    t.second = now.second;
    return copy_out(uout, &t, sizeof t);
}

static int64_t sys_sysinfo(uint64_t uout) {
    REQUIRE(CAP_SYSINFO);
    struct sys_info si = {0};
    si.mem_total_kib = pmm_total_pages() * 4;
    si.mem_free_kib = pmm_free_pages() * 4;
    si.uptime_ms = ticks;
    si.nproc = (uint32_t)proc_count();
    si.screen_w = boot_fb.width;
    si.screen_h = boot_fb.height;
    si.cpu_features = cpu_features;
    memcpy(si.cpu_vendor, cpu_vendor, sizeof si.cpu_vendor);
    memcpy(si.cpu_brand, cpu_brand, sizeof si.cpu_brand);
    return copy_out(uout, &si, sizeof si);
}

static int64_t sys_proc_list(uint64_t uout, uint64_t max) {
    REQUIRE(CAP_SYSINFO);
    struct proc_info list[MAX_PROCS];
    int n = proc_list(list, (int)MIN(max, (uint64_t)MAX_PROCS));
    if (copy_out(uout, list, (uint64_t)n * sizeof list[0])) return -E_FAULT;
    return n;
}

static int64_t sys_spawn(uint64_t uname, uint64_t len, uint64_t caps, uint64_t arg) {
    REQUIRE(CAP_SPAWN);
    char name[16];
    if (len == 0 || len >= sizeof name) return -E_INVAL;
    if (copy_in(name, uname, len)) return -E_FAULT;
    name[len] = 0;
    const struct program *prog = program_find(name);
    if (!prog) return -E_NOENT;
    /* Least privilege: what the program asks for, what the caller requested,
     * and never anything the caller doesn't hold itself. */
    uint32_t granted = prog->caps & (uint32_t)caps & current->caps;
    return proc_spawn(name, granted, arg, current->pid);
}

static int64_t sys_kill(uint64_t pid) {
    REQUIRE(CAP_KILL);
    struct proc *p = proc_find((int)pid);
    if (!p || p->pid == 0) return -E_NOENT;
    if (p == current) proc_kill_current(EXIT_KILLED_BY_WM, 0);
    proc_kill(p, EXIT_KILLED_BY_WM, "was stopped by another process");
    return 0;
}

static int64_t sys_klog_read(uint64_t ubuf, uint64_t len) {
    REQUIRE(CAP_SYSINFO);
    static char tmp[4096];
    len = MIN(len, sizeof tmp);
    if (!user_range_ok(ubuf, len, 1)) return -E_FAULT;
    size_t n = klog_read(tmp, len);
    if (copy_out(ubuf, tmp, n)) return -E_FAULT;
    return (int64_t)n;
}

static int64_t sys_wait(uint64_t pid, int64_t timeout_ms) {
    int status = proc_exit_status((int)pid, current->pid);
    if (status == EXIT_STILL_RUNNING && timeout_ms != 0) {
        cur_thread->wait_pid = (int)pid;
        proc_block(WAIT_CHILD, timeout_ms);
        status = proc_exit_status((int)pid, current->pid);
    }
    return status < 0 ? -E_NOENT : status;
}

static int64_t sys_win_title(uint64_t handle, uint64_t utitle, uint64_t len) {
    REQUIRE(CAP_WINDOW);
    char title[WIN_TITLE_MAX];
    len = MIN(len, sizeof title - 1);
    if (copy_in(title, utitle, len)) return -E_FAULT;
    title[len] = 0;
    return wm_set_title(current, (int)handle, title);
}

/* Runs the kernel's unit tests. They spawn and stop processes and park the
 * file system for a moment, so only system apps (the terminal) may ask. */
static int64_t sys_selftest(uint64_t usuite, uint64_t slen, uint64_t ubuf, uint64_t blen) {
    REQUIRE(CAP_SYSINFO);
    if (current->trust != TRUST_SYSTEM) return -E_PERM;
    char suite[16];
    if (slen >= sizeof suite) return -E_NOENT;
    if (copy_in(suite, usuite, slen)) return -E_FAULT;
    suite[slen] = 0;
    static char report[8192];
    blen = MIN(blen, sizeof report);
    if (!user_range_ok(ubuf, blen, 1)) return -E_FAULT;
    struct ktest_result res;
    int err = ktest_run(suite, report, blen, &res);
    if (err) return err;
    if (copy_out(ubuf, report, MIN(strlen(report) + 1, blen))) return -E_FAULT;
    return (int64_t)(MIN(res.passed, 0xFFFFu) << 16 | MIN(res.failed, 0xFFFFu));
}

/* ---- Apps and settings (0.6) ------------------------------------------- */

static int64_t sys_app_list(uint64_t ubuf, uint64_t max) {
    REQUIRE(CAP_APPS);
    static struct app_info list[32];
    int n = appstore_list(list, (int)MIN(max, (uint64_t)ARRAY_LEN(list)));
    return copy_out(ubuf, list, (uint64_t)n * sizeof list[0]) ? -E_FAULT : n;
}

static int64_t sys_app_install(uint64_t upkg, uint64_t len, uint64_t flags) {
    REQUIRE(CAP_APPS);
    if (!len || len > APP_MAX_BYTES) return -E_INVAL;
    uint64_t pages = ALIGN_UP(len, PAGE_SIZE) / PAGE_SIZE;
    uint64_t phys = pmm_alloc_contig(pages);
    if (!phys) return -E_NOMEM;
    if (copy_in(P2V(phys), upkg, len)) {
        pmm_free_contig(phys, pages);
        return -E_FAULT;
    }
    int err = appstore_install(phys, len, (uint32_t)flags);
    if (err) pmm_free_contig(phys, pages); /* on success the program table owns it */
    return err;
}

static int64_t sys_app_remove(uint64_t uname, uint64_t len) {
    REQUIRE(CAP_APPS);
    char name[APP_NAME_MAX];
    if (len == 0 || len >= sizeof name) return -E_INVAL;
    if (copy_in(name, uname, len)) return -E_FAULT;
    name[len] = 0;
    return appstore_remove(name);
}

static int64_t sys_app_info(uint64_t uname, uint64_t len, uint64_t uout) {
    REQUIRE(CAP_APPS);
    char name[APP_NAME_MAX];
    if (len == 0 || len >= sizeof name) return -E_INVAL;
    if (copy_in(name, uname, len)) return -E_FAULT;
    name[len] = 0;
    struct app_info info;
    int err = appstore_info(name, &info);
    if (err) return err;
    return copy_out(uout, &info, sizeof info);
}

static int64_t sys_app_icon(uint64_t uname, uint64_t len, uint64_t ubuf, uint64_t max) {
    REQUIRE(CAP_APPS);
    char name[APP_NAME_MAX];
    if (len == 0 || len >= sizeof name) return -E_INVAL;
    if (copy_in(name, uname, len)) return -E_FAULT;
    name[len] = 0;
    const uint8_t *data;
    uint32_t w, h;
    if (!appstore_icon(name, &data, &w, &h)) return 0;
    uint64_t size = (uint64_t)w * h * 4;
    if (max < size) return -E_INVAL;
    if (copy_out(ubuf, data, size)) return -E_FAULT;
    return (int64_t)(((uint32_t)w << 16) | h);
}

static int64_t sys_net_config(uint64_t op, uint64_t uin, uint64_t uout) {
    REQUIRE(CAP_CONFIG);
    struct net_config cfg;
    if (op == NETCFG_GET) {
        net_get_config(&cfg);
    } else if (op == NETCFG_SET) {
        if (copy_in(&cfg, uin, sizeof cfg)) return -E_FAULT;
        int err = net_apply_config(&cfg);
        if (err) return err;
        settings_save_net(&cfg);
        net_get_config(&cfg);
    } else {
        return -E_INVAL;
    }
    return uout && copy_out(uout, &cfg, sizeof cfg) ? -E_FAULT : 0;
}

static int64_t sys_settings_get(uint64_t ukey, uint64_t klen, uint64_t uout, uint64_t olen) {
    REQUIRE(CAP_CONFIG);
    char key[SETTINGS_KEY_MAX], value[SETTINGS_VALUE_MAX];
    if (klen == 0 || klen >= sizeof key) return -E_INVAL;
    if (copy_in(key, ukey, klen)) return -E_FAULT;
    key[klen] = 0;
    int n = settings_get(key, value, sizeof value);
    if (n < 0) return n;
    if ((uint64_t)n >= olen) n = olen ? (int)olen - 1 : 0;
    if (olen && copy_out(uout, value, (uint64_t)n)) return -E_FAULT;
    return n;
}

static int64_t sys_settings_set(uint64_t ukey, uint64_t klen, uint64_t uval, uint64_t vlen) {
    REQUIRE(CAP_CONFIG);
    char key[SETTINGS_KEY_MAX], value[SETTINGS_VALUE_MAX];
    if (klen == 0 || klen >= sizeof key || vlen >= sizeof value) return -E_INVAL;
    if (copy_in(key, ukey, klen) || copy_in(value, uval, vlen)) return -E_FAULT;
    key[klen] = 0;
    value[vlen] = 0;
    return settings_set(key, value);
}

static int64_t sys_os_info(uint64_t uout) {
    struct os_info oi = {0};
    strlcpy(oi.version, AETHER_VERSION, sizeof oi.version);
    strlcpy(oi.build, AETHER_BUILD, sizeof oi.build);
    oi.abi_version = 1;
    oi.uptime_ms = ticks;
    return copy_out(uout, &oi, sizeof oi);
}

static int64_t sys_wifi_info(uint64_t uout) {
    struct wifi_info wi = {0};
    strlcpy(wi.detail, "No Wi-Fi adapter found (no Wi-Fi driver yet)", sizeof wi.detail);
    return copy_out(uout, &wi, sizeof wi);
}

/* ---- Installation and updates (0.8) ------------------------------------ */

/* The kernel's yes/no gate for destructive update operations. The dialog is
 * drawn by the kernel and input goes only to it, so the app cannot silently
 * confirm its own request. */
static int confirm_destructive(const char *title, const char *detail) {
    for (;;) {
        cur_thread->dialog_done = 0;
        int r = wm_dialog_confirm(current, title, detail);
        if (r == 0) break;
        if (r != -E_LIMIT) return 0;
        proc_block(WAIT_SLEEP, 100); /* another dialog is up: wait our turn */
        if (current->state == PROC_ZOMBIE) return 0;
    }
    while (!cur_thread->dialog_done) {
        proc_block(WAIT_DIALOG, -1);
        if (current->state == PROC_ZOMBIE) return 0;
    }
    return cur_thread->dialog_result == 1;
}

static int64_t sys_disk_list(uint64_t ubuf, uint64_t max) {
    REQUIRE(CAP_UPDATE);
    static struct disk_info list[8];
    int n = update_list_disks(list, (int)MIN(max, (uint64_t)ARRAY_LEN(list)));
    return copy_out(ubuf, list, (uint64_t)n * sizeof list[0]) ? -E_FAULT : n;
}

static int64_t sys_update_info(uint64_t uout) {
    REQUIRE(CAP_UPDATE);
    struct update_info ui;
    update_info(&ui);
    return copy_out(uout, &ui, sizeof ui);
}

/* Copy a user image into physical pages. The size is the actual image size,
 * so a release does not have to reserve the full 256 MiB slot. */
static uint64_t copy_image(uint64_t uptr, uint64_t len, uint64_t max) {
    if (!uptr || !len || len > max) return 0;
    uint64_t pages = ALIGN_UP(len, PAGE_SIZE) / PAGE_SIZE;
    uint64_t phys = pmm_alloc_contig(pages);
    if (!phys) return 0;
    if (copy_in(P2V(phys), uptr, len)) {
        pmm_free_contig(phys, pages);
        return 0;
    }
    return phys;
}

static void install_confirm_detail(int disk, char *out, size_t n) {
    struct disk_info list[8];
    int dn = update_list_disks(list, (int)ARRAY_LEN(list));
    if (disk >= 0 && disk < dn) {
        uint64_t gb = list[disk].sectors * 512 / (1000ull * 1000ull * 1000ull);
        snprintf(out, n, "%s will replace the partition table on %s (%s, %lu GB) and erase all its files.",
                 current->name, list[disk].name, list[disk].model[0] ? list[disk].model : "disk", gb);
    } else {
        snprintf(out, n, "%s will replace the partition table and erase all files on the target disk.", current->name);
    }
}

static int64_t sys_system_install(uint64_t ureq) {
    REQUIRE(CAP_UPDATE);
    if (!(boot_flags & BOOT_FLAG_NETBOOT)) return -E_PERM;
    struct system_install_req req;
    if (copy_in(&req, ureq, sizeof req)) return -E_FAULT;
    size_t pwlen = strnlen(req.passphrase, sizeof req.passphrase);
    if (pwlen < 8 || pwlen >= sizeof req.passphrase) {
        crypto_wipe(req.passphrase, sizeof req.passphrase);
        return -E_INVAL;
    }
    char detail[160];
    install_confirm_detail((int)req.disk, detail, sizeof detail);
    if (!confirm_destructive("Install æther on this disk?", detail)) {
        crypto_wipe(req.passphrase, sizeof req.passphrase);
        return -E_DENIED;
    }
    uint64_t boot = copy_image(req.boot_image, req.boot_len, 64ull * 1024 * 1024);
    if (!boot) {
        crypto_wipe(req.passphrase, sizeof req.passphrase);
        return -E_NOMEM;
    }
    uint64_t system = copy_image(req.system_image, req.system_len, 256ull * 1024 * 1024);
    if (!system) {
        pmm_free_contig(boot, ALIGN_UP(req.boot_len, PAGE_SIZE) / PAGE_SIZE);
        crypto_wipe(req.passphrase, sizeof req.passphrase);
        return -E_NOMEM;
    }
    int err = update_install((int)req.disk, P2V(boot), req.boot_len, P2V(system), req.system_len, req.passphrase,
                             pwlen);
    pmm_free_contig(boot, ALIGN_UP(req.boot_len, PAGE_SIZE) / PAGE_SIZE);
    pmm_free_contig(system, ALIGN_UP(req.system_len, PAGE_SIZE) / PAGE_SIZE);
    crypto_wipe(req.passphrase, sizeof req.passphrase);
    return err;
}

static int64_t sys_update_install(uint64_t uimage, uint64_t len) {
    REQUIRE(CAP_UPDATE);
    if (!(boot_flags & BOOT_FLAG_INSTALLED)) return -E_NOTSUP;
    char detail[160];
    snprintf(detail, sizeof detail,
             "%s will write a signed system image to the inactive slot; it is tried on the next boot.",
             current->name);
    if (!confirm_destructive("Install this æther update?", detail)) return -E_DENIED;
    uint64_t image = copy_image(uimage, len, 256ull * 1024 * 1024);
    if (!image) return -E_NOMEM;
    int err = update_install_system(P2V(image), len);
    pmm_free_contig(image, ALIGN_UP(len, PAGE_SIZE) / PAGE_SIZE);
    return err;
}

static int64_t sys_reboot(void) {
    REQUIRE(CAP_UPDATE);
    system_reboot();
    return 0; /* not reached */
}

static int64_t sys_change_passphrase(uint64_t uold, uint64_t oldlen, uint64_t unew, uint64_t newlen) {
    REQUIRE(CAP_CONFIG);
    char old[SYSTEM_PASSPHRASE_MAX], new[SYSTEM_PASSPHRASE_MAX];
    if (oldlen == 0 || oldlen >= sizeof old || newlen < 8 || newlen >= sizeof new) return -E_INVAL;
    if (copy_in(old, uold, oldlen) || copy_in(new, unew, newlen)) return -E_FAULT;
    old[oldlen] = new[newlen] = 0;
    int err = afs_change_passphrase(old, oldlen, new, newlen);
    crypto_wipe(old, sizeof old);
    crypto_wipe(new, sizeof new);
    return err;
}

static int64_t sys_shutdown(void) {
    REQUIRE(CAP_CONFIG);
    system_shutdown();
}

static int64_t sys_lock(void) {
    REQUIRE(CAP_CONFIG);
    return wm_lock_session();
}

void syscall_dispatch(struct isr_frame *f) {
    uint64_t a = f->rdi, b = f->rsi, c = f->rdx, d = f->r10;
    int64_t r;
    /* iretq loads this value into RSP after switching back to ring 3. If it
     * is non-canonical the #GP happens in ring 0 and panics the kernel.
     * A user address must be below USER_TOP (canonical low half). */
    if (f->rsp >= USER_TOP)
        proc_kill_current(EXIT_KILLED_BASE + 13, "blocked: invalid user stack pointer");
    if (current->abi == ABI_LINUX) {
        /* Linux programs reach æther's own calls at 0x1000 + N. */
        if (f->rax < 0x1000 || f->rax >= 0x1000 + 64) {
            linux_syscall(f);
            return;
        }
        f->rax -= 0x1000;
    }
    if (f->rax >= 64 || !(current->syscall_allow & (1ull << f->rax))) {
        static int last_pid;
        if (last_pid != current->pid) { /* log once per process, not once per call */
            kprintf("[sec] %s (pid %d): system call %lu is outside its sandbox\n", current->name, current->pid, f->rax);
            last_pid = current->pid;
        }
        f->rax = (uint64_t)-E_PERM;
        return;
    }
    switch (f->rax) {
    case SYS_EXIT: proc_exit_current((int)a); r = 0; break;
    case SYS_LOG: r = sys_log(a, b); break;
    case SYS_WIN_CREATE: r = sys_win_create(a, b); break;
    case SYS_WIN_PRESENT: r = wm_present(current, (int)a); break;
    case SYS_EVENT_WAIT: r = sys_event_wait(a, (int64_t)b); break;
    case SYS_TIME: r = sys_time(a); break;
    case SYS_SYSINFO: r = sys_sysinfo(a); break;
    case SYS_PROC_LIST: r = sys_proc_list(a, b); break;
    case SYS_SPAWN: r = sys_spawn(a, b, c, d); break;
    case SYS_KILL: r = sys_kill(a); break;
    case SYS_KLOG_READ: r = sys_klog_read(a, b); break;
    case SYS_YIELD: schedule(); r = 0; break;
    case SYS_WAIT: r = sys_wait(a, (int64_t)b); break;
    case SYS_WIN_TITLE: r = sys_win_title(a, b, c); break;
    case SYS_WIN_FLAGS: r = wm_win_flags(current, (int)a, (uint32_t)b, (uint32_t)c, (uint32_t)d); break;
    case SYS_WIN_RESIZE: r = sys_win_resize(a, b); break;
    case SYS_SELF: r = (int64_t)(((uint64_t)current->caps << 32) | (uint32_t)current->pid); break;
    case SYS_OPEN: r = sys_open(a, b, c); break;
    case SYS_CLOSE: r = sys_close(a); break;
    case SYS_READ: r = sys_read(a, b, c); break;
    case SYS_WRITE: r = sys_write(a, b, c); break;
    case SYS_SEEK: r = sys_seek(a, (int64_t)b, c); break;
    case SYS_STAT: r = sys_stat(a, b, c); break;
    case SYS_READDIR: r = sys_readdir(a, b, c); break;
    case SYS_MKDIR: r = sys_mkdir(a, b); break;
    case SYS_REMOVE: r = sys_remove(a, b); break;
    case SYS_RENAME: r = sys_rename(a, b, c, d); break;
    case SYS_SET_LABEL: r = sys_set_label(a, b, c); break;
    case SYS_PICK_FILE: r = sys_pick_file(a, b); break;
    case SYS_FS_INFO: r = sys_fs_info(a); break;
    case SYS_DLP_LOG: r = sys_dlp_log(a, b); break;
    case SYS_MEM_MAP: r = sys_mem_map(a, b); break;
    case SYS_MEM_UNMAP: r = sys_mem_unmap(a, b); break;
    case SYS_MEM_PROTECT: r = sys_mem_protect(a, b, c); break;
    case SYS_EVENT_WAKE:
        if (!current->wake_queued) {
            current->wake_queued = 1;
            proc_post_event(current, &(struct event){.type = EV_WAKE});
        }
        r = 0;
        break;
    case SYS_DISPLAY_INFO: {
        struct display_info di;
        wm_display_info(&di);
        r = copy_out(a, &di, sizeof di);
        break;
    }
    case SYS_NET_CONNECT: r = sys_net_connect(a, b, c, d); break;
    case SYS_NET_SEND: r = sys_net_send(a, b, c); break;
    case SYS_NET_RECV: r = sys_net_recv(a, b, c, (int64_t)d); break;
    case SYS_NET_CLOSE: r = sys_net_close(a); break;
    case SYS_NET_INFO: r = sys_net_info(a); break;
    case SYS_FIREWALL: r = sys_firewall(a, b, c); break;
    case SYS_SELFTEST: r = sys_selftest(a, b, c, d); break;
    case SYS_APP_LIST: r = sys_app_list(a, b); break;
    case SYS_APP_INSTALL: r = sys_app_install(a, b, c); break;
    case SYS_APP_REMOVE: r = sys_app_remove(a, b); break;
    case SYS_APP_INFO: r = sys_app_info(a, b, c); break;
    case SYS_APP_ICON: r = sys_app_icon(a, b, c, d); break;
    case SYS_NET_CONFIG: r = sys_net_config(a, b, c); break;
    case SYS_SETTINGS_GET: r = sys_settings_get(a, b, c, d); break;
    case SYS_SETTINGS_SET: r = sys_settings_set(a, b, c, d); break;
    case SYS_OS_INFO: r = sys_os_info(a); break;
    case SYS_WIFI_INFO: r = sys_wifi_info(a); break;
    case SYS_DISK_LIST: r = sys_disk_list(a, b); break;
    case SYS_SYSTEM_INSTALL: r = sys_system_install(a); break;
    case SYS_UPDATE_INFO: r = sys_update_info(a); break;
    case SYS_UPDATE_INSTALL: r = sys_update_install(a, b); break;
    case SYS_REBOOT: r = sys_reboot(); break;
    case SYS_LOCK: r = sys_lock(); break;
    case SYS_TRUNCATE: r = sys_truncate(a, b); break;
    case SYS_CHANGE_PASSPHRASE: r = sys_change_passphrase(a, b, c, d); break;
    case SYS_SHUTDOWN: r = sys_shutdown(); break;
    default: r = -E_INVAL; break;
    }
    f->rax = (uint64_t)r;
}
