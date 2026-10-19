/* sys.h - system call wrappers for æther apps. */
#pragma once
#include <stdint.h>
#include "../../common/string.h"
#include "../../include/abi.h"

static inline int64_t syscall5(uint64_t n, uint64_t a, uint64_t b, uint64_t c, uint64_t d, uint64_t e) {
    int64_t ret;
    register uint64_t r10 __asm__("r10") = d;
    register uint64_t r8 __asm__("r8") = e;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8)
                     : "rcx", "r11", "memory");
    return ret;
}

#define syscall0(n) syscall5(n, 0, 0, 0, 0, 0)
#define syscall1(n, a) syscall5(n, (uint64_t)(a), 0, 0, 0, 0)
#define syscall2(n, a, b) syscall5(n, (uint64_t)(a), (uint64_t)(b), 0, 0, 0)
#define syscall3(n, a, b, c) syscall5(n, (uint64_t)(a), (uint64_t)(b), (uint64_t)(c), 0, 0)
#define syscall4(n, a, b, c, d) syscall5(n, (uint64_t)(a), (uint64_t)(b), (uint64_t)(c), (uint64_t)(d), 0)

__attribute__((noreturn)) static inline void sys_exit(int code) {
    syscall1(SYS_EXIT, code);
    __builtin_unreachable();
}
static inline int64_t sys_log(const char *s) { return syscall2(SYS_LOG, s, strlen(s)); }
static inline int64_t sys_event_wait(struct event *ev, int64_t timeout_ms) { return syscall2(SYS_EVENT_WAIT, ev, timeout_ms); }
static inline int64_t sys_time(struct time_info *t) { return syscall1(SYS_TIME, t); }
static inline int64_t sys_sysinfo(struct sys_info *si) { return syscall1(SYS_SYSINFO, si); }
static inline int64_t sys_proc_list(struct proc_info *p, int max) { return syscall2(SYS_PROC_LIST, p, max); }
static inline int64_t sys_spawn(const char *name, uint32_t caps, uint64_t arg) {
    return syscall4(SYS_SPAWN, name, strlen(name), caps, arg);
}
static inline int64_t sys_kill(int pid) { return syscall1(SYS_KILL, pid); }
static inline int64_t sys_klog_read(char *buf, uint64_t len) { return syscall2(SYS_KLOG_READ, buf, len); }
static inline int64_t sys_wait(int pid, int64_t timeout_ms) { return syscall2(SYS_WAIT, pid, timeout_ms); }
static inline int64_t sys_yield(void) { return syscall0(SYS_YIELD); }
static inline int sys_pid(void) { return (int)(syscall0(SYS_SELF) & 0xFFFFFFFF); }
static inline uint32_t sys_caps(void) { return (uint32_t)((uint64_t)syscall0(SYS_SELF) >> 32); }
static inline int64_t sys_win_title(int h, const char *t) { return syscall3(SYS_WIN_TITLE, h, t, strlen(t)); }

/* Files */
static inline int64_t sys_open(const char *path, uint32_t flags) { return syscall3(SYS_OPEN, path, strlen(path), flags); }
static inline int64_t sys_close(int h) { return syscall1(SYS_CLOSE, h); }
static inline int64_t sys_read(int h, void *buf, uint64_t len) { return syscall3(SYS_READ, h, buf, len); }
static inline int64_t sys_write(int h, const void *buf, uint64_t len) { return syscall3(SYS_WRITE, h, buf, len); }
static inline int64_t sys_seek(int h, int64_t off, int whence) { return syscall3(SYS_SEEK, h, off, whence); }
static inline int64_t sys_truncate(int h, uint64_t size) { return syscall2(SYS_TRUNCATE, h, size); }
static inline int64_t sys_stat(const char *path, struct file_stat *st) { return syscall3(SYS_STAT, path, strlen(path), st); }
static inline int64_t sys_fstat(int h, struct file_stat *st) { return syscall3(SYS_STAT, 0, h, st); }
static inline int64_t sys_readdir(int h, uint32_t idx, struct dir_entry *de) { return syscall3(SYS_READDIR, h, idx, de); }
static inline int64_t sys_mkdir(const char *path) { return syscall2(SYS_MKDIR, path, strlen(path)); }
static inline int64_t sys_remove(const char *path) { return syscall2(SYS_REMOVE, path, strlen(path)); }
static inline int64_t sys_rename(const char *from, const char *to) {
    return syscall4(SYS_RENAME, from, strlen(from), to, strlen(to));
}
static inline int64_t sys_set_label(const char *path, int label) { return syscall3(SYS_SET_LABEL, path, strlen(path), label); }
static inline int64_t sys_pick_file(const struct pick_req *req, struct pick_result *out) {
    return syscall2(SYS_PICK_FILE, req, out);
}
static inline int64_t sys_fs_info(struct fs_info *fi) { return syscall1(SYS_FS_INFO, fi); }
static inline int64_t sys_dlp_log(char *buf, uint64_t len) { return syscall2(SYS_DLP_LOG, buf, len); }

/* Memory */
static inline int64_t sys_mem_map(uint64_t len, int prot) { return syscall2(SYS_MEM_MAP, len, prot); }
static inline int64_t sys_mem_unmap(uint64_t addr, uint64_t len) { return syscall2(SYS_MEM_UNMAP, addr, len); }
static inline int64_t sys_mem_protect(uint64_t addr, uint64_t len, int prot) {
    return syscall3(SYS_MEM_PROTECT, addr, len, prot);
}
static inline int64_t sys_display_info(struct display_info *out) { return syscall1(SYS_DISPLAY_INFO, out); }

/* Network: outgoing TCP, checked by the firewall. */
static inline int64_t sys_net_connect(const char *host, uint16_t port, uint32_t timeout_ms) {
    return syscall4(SYS_NET_CONNECT, host, strlen(host), port, timeout_ms);
}
static inline int64_t sys_net_send(int h, const void *buf, uint64_t len) { return syscall3(SYS_NET_SEND, h, buf, len); }
static inline int64_t sys_net_recv(int h, void *buf, uint64_t len, int64_t timeout_ms) {
    return syscall4(SYS_NET_RECV, h, buf, len, timeout_ms);
}
static inline int64_t sys_net_close(int h) { return syscall1(SYS_NET_CLOSE, h); }
static inline int64_t sys_net_info(struct net_info *out) { return syscall1(SYS_NET_INFO, out); }
static inline int64_t sys_firewall(int op, struct fw_rule *r, int index) { return syscall3(SYS_FIREWALL, op, r, index); }
static inline int64_t sys_net_config(int op, struct net_config *in, struct net_config *out) {
    return syscall3(SYS_NET_CONFIG, op, in, out);
}
static inline int64_t sys_app_list(struct app_info *out, int max) { return syscall2(SYS_APP_LIST, out, max); }
static inline int64_t sys_app_remove(const char *name) { return syscall2(SYS_APP_REMOVE, name, strlen(name)); }
static inline int64_t sys_os_info(struct os_info *out) { return syscall1(SYS_OS_INFO, out); }

/* Kernel unit tests: (passed << 16) | failed, or -E_*. */
static inline int64_t sys_selftest(const char *suite, char *report, uint64_t len) {
    return syscall4(SYS_SELFTEST, suite, strlen(suite), report, len);
}

static inline const char *label_name(uint32_t l) {
    static const char *n[] = {"Public", "Personal", "Confidential", "Secret"};
    return l < 4 ? n[l] : "?";
}
static inline int64_t sys_notify(const char *title, const char *msg) {
    return syscall4(SYS_NOTIFY, title, strlen(title), msg, strlen(msg));
}
