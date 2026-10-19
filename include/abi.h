/*
 * abi.h - the system call interface between æther applications and the kernel.
 *
 * Calling convention (the `syscall` instruction):
 *   rax = SYS_* number, args in rdi, rsi, rdx, r10, r8; result in rax.
 *   Negative results are -E* errors.
 *
 * Security model: a process can only do what its capability bits allow.
 * Capabilities are fixed when a process is spawned and can only shrink: a
 * child never receives a capability its parent doesn't hold.
 */
#pragma once
#include <stdint.h>

/* ---- Capabilities ------------------------------------------------------ */
#define CAP_WINDOW  (1u << 0) /* create windows, receive input */
#define CAP_LOG     (1u << 1) /* write to the kernel log */
#define CAP_SYSINFO (1u << 2) /* read system information, process list, kernel log */
#define CAP_SPAWN   (1u << 3) /* start other programs */
#define CAP_KILL    (1u << 4) /* terminate other processes */
#define CAP_FILES   (1u << 5) /* open the user's files by path (/home, /apps) */
#define CAP_PICK    (1u << 6) /* ask the user to choose a file (the system file picker) */
#define CAP_STORAGE  (1u << 7) /* a private folder of its own, /apps/<name> */
#define CAP_NET      (1u << 8) /* open outgoing connections, each one checked by the firewall */
#define CAP_FIREWALL (1u << 9) /* read and change the firewall's rules */
#define CAP_APPS     (1u << 10) /* install, update and remove app packages */
#define CAP_CONFIG   (1u << 11) /* change system settings: network, Wi-Fi, ... */
#define CAP_UPDATE   (1u << 12) /* install the OS, write A/B slots and reboot */
#define CAP_CLIPBOARD (1u << 13) /* read and write the label-aware system clipboard */

/* Which system call interface a program speaks. */
#define ABI_AETHER 0
#define ABI_LINUX  1

/* ---- Errors ------------------------------------------------------------ */
#define E_PERM    1  /* missing capability */
#define E_FAULT   2  /* bad pointer passed to the kernel */
#define E_INVAL   3  /* bad argument */
#define E_NOMEM   4
#define E_NOENT   5  /* no such program / process / window */
#define E_LIMIT   6  /* too many processes / windows */
#define E_TIMEOUT 7
#define E_EXIST   8  /* already exists */
#define E_NOTDIR  9
#define E_ISDIR   10
#define E_NOSPACE 11
#define E_IO      12 /* disk error, or data that failed its integrity check */
#define E_DENIED  13 /* refused by data loss prevention policy, or by the user */
#define E_LOCKED  14 /* encrypted storage isn't unlocked yet */
#define E_NOTEMPTY 15
#define E_BUSY    17 /* an app is running, or a device is busy */
#define E_NOTSUP  18 /* this system or hardware doesn't support it (yet) */

/* ---- System calls ------------------------------------------------------ */
enum {
    SYS_EXIT = 0,        /* (code) -> never returns */
    SYS_LOG = 1,         /* (const char *msg, len) [CAP_LOG] */
    SYS_WIN_CREATE = 2,  /* (const struct win_req *, struct win_info *out) [CAP_WINDOW] -> handle */
    SYS_WIN_PRESENT = 3, /* (handle) -> 0: show what's in the pixel buffer */
    SYS_EVENT_WAIT = 4,  /* (struct event *out, timeout_ms; -1 = forever) -> 1 event, 0 timeout */
    SYS_TIME = 5,        /* (struct time_info *out) */
    SYS_SYSINFO = 6,     /* (struct sys_info *out) [CAP_SYSINFO] */
    SYS_PROC_LIST = 7,   /* (struct proc_info *out, max) [CAP_SYSINFO] -> count */
    SYS_SPAWN = 8,       /* (const char *name, len, caps, arg) [CAP_SPAWN] -> pid */
    SYS_KILL = 9,        /* (pid) [CAP_KILL] */
    SYS_KLOG_READ = 10,  /* (char *buf, len) [CAP_SYSINFO] -> bytes */
    SYS_YIELD = 11,      /* () */
    SYS_WAIT = 12,       /* (pid, timeout_ms) -> exit status, see EXIT_* below */
    SYS_WIN_TITLE = 13,  /* (handle, const char *title, len) */
    SYS_SELF = 14,       /* () -> (caps << 32) | pid */

    /* Files. Paths are UTF-8, '/'-separated, at most PATH_MAX bytes. With
     * CAP_FILES they start at the root ("/home/notes.txt"); a CAP_STORAGE app
     * sees only its own folder, so "drafts/a.txt" means /apps/<name>/drafts/a.txt.
     * Handles are small integers private to the process. */
    SYS_OPEN = 15,       /* (const char *path, len, flags O_*) -> handle */
    SYS_CLOSE = 16,      /* (handle) */
    SYS_READ = 17,       /* (handle, void *buf, len) -> bytes (0 at end) */
    SYS_WRITE = 18,      /* (handle, const void *buf, len) -> bytes */
    SYS_SEEK = 19,       /* (handle, offset, whence SEEK_*) -> new offset */
    SYS_STAT = 20,       /* (const char *path, len, struct file_stat *out); path 0 + len = handle: fstat */
    SYS_READDIR = 21,    /* (handle, index, struct dir_entry *out) -> 1, or 0 past the end */
    SYS_MKDIR = 22,      /* (const char *path, len) */
    SYS_REMOVE = 23,     /* (const char *path, len): a file or an empty folder */
    SYS_RENAME = 24,     /* (const char *from, len, const char *to, len) within one folder tree */
    SYS_SET_LABEL = 25,  /* (const char *path, len, label): raising is free, lowering asks the user */
    SYS_PICK_FILE = 26,  /* (const struct pick_req *, struct pick_result *out) [CAP_PICK] -> handle */
    SYS_FS_INFO = 27,    /* (struct fs_info *out) */
    SYS_DLP_LOG = 28,    /* (char *buf, len) [CAP_SYSINFO] -> bytes of the audit log */

    /* Memory: anonymous, zeroed, never writable and executable at once. */
    SYS_MEM_MAP = 29,    /* (len, prot PROT_*) -> user address */
    SYS_MEM_UNMAP = 30,  /* (addr, len) */
    SYS_MEM_PROTECT = 31,/* (addr, len, prot) */
    SYS_DISPLAY_INFO = 32, /* (struct display_info *out) */
    SYS_EVENT_WAKE = 33,   /* () queue one EV_WAKE for this process: another of its threads wants a frame */

    /* Network (0.5). Outgoing TCP only: nothing can listen, so nothing
     * reaches an app unless it asked. The kernel resolves the name and the
     * firewall decides, by app and host name, before any packet is sent
     * (asking the user the first time). An app that has read Confidential
     * or Secret data can't connect (E_DENIED). Handles are their own
     * small integers, separate from file handles. */
    SYS_NET_CONNECT = 34,  /* (const char *host, len, port, timeout_ms) [CAP_NET] -> handle */
    SYS_NET_SEND = 35,     /* (handle, const void *buf, len) -> bytes accepted (blocks while the window is full) */
    SYS_NET_RECV = 36,     /* (handle, void *buf, len, timeout_ms; -1 = forever) -> bytes, 0 = peer closed, -E_TIMEOUT */
    SYS_NET_CLOSE = 37,    /* (handle) */
    SYS_NET_INFO = 38,     /* (struct net_info *out) */
    SYS_FIREWALL = 39,     /* (op FW_*, struct fw_rule *, index) [system apps] -> see FW_* */

    /* Kernel unit tests (the terminal's `test` command). Numbered apart from
     * the rest so new calls can keep filling in from 32. */
    SYS_SELFTEST = 40,   /* (const char *suite, len, char *report, len) [CAP_SYSINFO, system apps]
                            -> (passed << 16) | failed; report gets one line per test */

    /* App installation and updates (0.6). Packages are checked by the kernel
     * before they are stored in /system/apps, a folder no app can open. A
     * running app is never restarted behind the user's back: an update is
     * installed, the running copy keeps its old code, and its title bar says
     * that an update is pending until it is next started. */
    SYS_APP_LIST = 41,    /* (struct app_info *out, max) [CAP_APPS] -> count */
    SYS_APP_INSTALL = 42, /* (const void *package, len, flags APP_INSTALL_*) [CAP_APPS] -> 0 */
    SYS_APP_REMOVE = 43,  /* (const char *name, len) [CAP_APPS] */
    SYS_APP_INFO = 44,    /* (const char *name, len, struct app_info *out) [CAP_APPS] */

    /* Settings (0.6). Keys are short strings ("net.mode", "net.ip", ...);
     * values are text. They live in /system/settings, which apps can't open. */
    SYS_NET_CONFIG = 45,    /* (op NETCFG_*, const struct net_config *in, struct net_config *out) [CAP_CONFIG] */
    SYS_SETTINGS_GET = 46,  /* (const char *key, len, char *out, outlen) [CAP_CONFIG] -> bytes, or -E_NOENT */
    SYS_SETTINGS_SET = 47,  /* (const char *key, len, const char *value, len) [CAP_CONFIG] */
    SYS_OS_INFO = 48,       /* (struct os_info *out) */
    SYS_WIFI_INFO = 49,     /* (struct wifi_info *out) */

    /* App icons (0.7). App Store can draw an installed package's icon
     * without reading /system/apps. Returns 0 when the app has no icon. */
    SYS_APP_ICON = 50,      /* (const char *name, len, void *rgba, max) [CAP_APPS]
                               -> (width << 16) | height, or 0; -E_INVAL if max is too small */

    /* System installation and A/B updates (0.8). The installer and App Store
     * hold CAP_UPDATE. The kernel validates a system image's header and
     * SHA-256 before writing it, and never touches the running slot. */
    SYS_DISK_LIST = 51,      /* (struct disk_info *out, max) [CAP_UPDATE] -> count */
    SYS_SYSTEM_INSTALL = 52, /* (const struct system_install_req *) [CAP_UPDATE] -> 0 */
    SYS_UPDATE_INFO = 53,    /* (struct update_info *out) [CAP_UPDATE] */
    SYS_UPDATE_INSTALL = 54, /* (const void *sysimage, len) [CAP_UPDATE] -> 0 */
    SYS_REBOOT = 55,         /* () [CAP_UPDATE] -> never returns */

    /* Resizable windows (0.9). The user zooms or resizes a window from its
     * kernel-drawn frame; an app never picks its own new size. The window
     * manager proposes a size with EV_RESIZE, and the app takes it with
     * SYS_WIN_RESIZE, which swaps in a new pixel buffer at the same address.
     * An app that never opted in keeps its size (its zoom button is off). */
    SYS_WIN_FLAGS = 56,      /* (handle, flags WIN_FLAG_*, min_w, min_h) -> 0; the user can't make the
                                window smaller than min_w x min_h (0: WIN_MIN_*) */
    SYS_WIN_RESIZE = 57,     /* (handle, struct win_info *out) -> 0: take the proposed size, or
                                report the current one when nothing is proposed */
    SYS_LOCK = 58,           /* () [CAP_CONFIG] -> 0; lock the desktop, or -E_NOTSUP on temporary storage */
    SYS_TRUNCATE = 59,       /* (handle, size) [O_WRITE] -> 0; set the file's length (save without a gap) */
    SYS_CHANGE_PASSPHRASE = 60, /* (old, oldlen, new, newlen) [CAP_CONFIG] -> 0; rewrap the mounted volume key */
    SYS_SHUTDOWN = 61,       /* () [CAP_CONFIG] -> never returns; ACPI S5 power off */

    /* Notifications (1.6). An app can post a short title/message; the kernel
     * records the app identity and rate-limits bursts. The history panel is
     * kernel-drawn (Super+N), so an app cannot spoof another app's entry. */
    SYS_NOTIFY = 62,         /* (title, title_len, msg, msg_len) [CAP_WINDOW] -> 0 */

    /* Clipboard (1.7). Kernel-owned memory with a DLP label. An app needs
     * CAP_CLIPBOARD to read or write it; reading raises the reader's taint.
     * The kernel clears it when the session locks. */
    SYS_CLIPBOARD = 63,      /* (op CLIP_*, arg, len, out) [CAP_CLIPBOARD]: see CLIP_* below */

    SYS_COUNT
};

/* SYS_WAIT status: plain exit codes are 0..255; a process the kernel killed
 * reports EXIT_KILLED_BASE + the CPU exception vector (or EXIT_KILLED_BY_*). */
#define EXIT_KILLED_BASE   0x100
#define EXIT_KILLED_BY_WM  0x1FE /* closed from the window manager or SYS_KILL */
#define EXIT_STILL_RUNNING 0x1FF

/* ---- Network ----------------------------------------------------------- */
#define E_NET     16 /* no network, the host can't be found, or the connection was refused or reset */

#define NET_HOST_MAX 64

struct net_info {
    uint32_t link;            /* 1 when a network card is up */
    uint32_t configured;      /* 1 once DHCP gave us an address */
    uint8_t mac[6], pad[2];
    uint32_t ip, netmask, gateway, dns; /* IPv4, network byte order */
    uint64_t rx_packets, tx_packets, dropped; /* dropped: blocked by the firewall or malformed */
    uint32_t connections;     /* open TCP connections */
    uint32_t pad2;
};

/* Firewall rules: the first rule matching (app, host, port) decides. Host
 * "*" matches any host, "*.example.com" any name under example.com; port 0
 * matches any port. After the rules comes the built-in web policy (the
 * browser may reach any public host on 443 and 80; FW_BUILTIN reads entry
 * `index`). Nothing matches: the user is asked. Inbound is always refused. */
enum { FW_ALLOW = 1, FW_DENY = 2 };
enum { FW_COUNT = 0, FW_GET = 1, FW_ADD = 2, FW_REMOVE = 3, FW_BUILTIN = 4 };
struct fw_rule {
    char app[16];
    char host[NET_HOST_MAX];
    uint16_t port;
    uint16_t action;          /* FW_ALLOW / FW_DENY */
    uint32_t hits;
};

/* Network configuration, changed by Settings. Addresses are IPv4 in network
 * byte order; 0 means "not set". Static configuration survives a reboot. */
enum { NET_MODE_DHCP = 0, NET_MODE_STATIC = 1 };
struct net_config {
    uint32_t mode; /* NET_MODE_* */
    uint32_t ip, netmask, gateway, dns;
};
enum { NETCFG_GET = 0, NETCFG_SET = 1 };

/* Wi-Fi is a placeholder in 0.6: there is no Wi-Fi driver yet, so Settings
 * can show that no adapter was found instead of pretending otherwise. */
struct wifi_info {
    uint32_t present, enabled, connected;
    int32_t signal; /* dBm, 0 when not connected */
    char ssid[32];
    char detail[64];
};

/* ---- Apps -------------------------------------------------------------- */
#define APP_NAME_MAX 16
#define APP_ICON_MAX 256 /* largest package icon, in pixels per side */
#define APP_TITLE_MAX 32
#define APP_VERSION_MAX 32
#define APP_SOURCE_MAX 96

struct app_info {
    char name[APP_NAME_MAX];
    char title[APP_TITLE_MAX];
    char version[APP_VERSION_MAX];
    char source[APP_SOURCE_MAX];
    uint32_t caps, trust, abi;
    uint32_t system;         /* 1: part of the OS image, can't be removed */
    uint32_t installed;      /* 1: a package (ISO or /system/apps), 0: built in */
    uint32_t running;        /* 1: a process with this name is alive */
    uint32_t update_pending; /* 1: a newer package was installed while it ran */
    uint32_t icon_color;
    uint32_t mem_quota_mib;
    uint64_t size;           /* program image bytes */
};

#define CLIP_MAX 65536

struct clip_result {
    uint64_t len;   /* bytes stored / copied */
    uint32_t label; /* DLP label */
    uint32_t pad;
};

#define CLIP_SET  1 /* (data, len, label) -> 0 */
#define CLIP_GET  2 /* (buf, max, struct clip_result *out) -> bytes */
#define CLIP_INFO 3 /* (struct clip_result *out) -> 0 */

#define APP_INSTALL_REPLACE 1 /* allow replacing an existing, non-system app */
#define APP_INSTALL_SIGNED  2 /* require a valid publisher signature (network downloads) */

/* ---- Settings ---------------------------------------------------------- */
#define SETTINGS_KEY_MAX 32
#define SETTINGS_VALUE_MAX 128
#define OS_VERSION_MAX 16
#define OS_BUILD_MAX 32

struct os_info {
    char version[OS_VERSION_MAX];
    char build[OS_BUILD_MAX];
    uint32_t abi_version;
    uint32_t pad;
    uint64_t uptime_ms;
};

/* ---- Installation and updates (0.8) ------------------------------------ */
#define DISK_NAME_MAX 16
#define DISK_MODEL_MAX 41
#define SYSTEM_PASSPHRASE_MAX 128

struct disk_info {
    char name[DISK_NAME_MAX];
    char model[DISK_MODEL_MAX];
    uint8_t pad[3];
    uint64_t sectors;
    uint32_t installed; /* 1: an A/B æther system is present */
    uint32_t has_gpt;
    uint32_t target_ok; /* large enough for the installer layout */
    uint32_t pad2;
};

struct update_info {
    uint32_t boot_flags; /* BOOT_FLAG_* from bootinfo.h */
    uint32_t boot_slot;  /* 0/1, or BOOT_NO_SLOT when booted live */
    uint32_t installed;
    uint32_t slots;
    uint32_t running_slot;
    uint32_t active_slot;
    uint32_t good_slot;
    uint32_t pending_slot;
    uint32_t attempts;
    uint32_t pad;
    uint64_t slot_bytes;
    char running_version[32];
    char running_build[32];
    char other_version[32];
    char other_build[32];
};

struct system_install_req {
    uint64_t boot_image; /* user address */
    uint64_t boot_len;
    uint64_t system_image;
    uint64_t system_len;
    uint32_t disk;
    uint32_t flags;
    char passphrase[SYSTEM_PASSPHRASE_MAX];
};

/* ---- Files ------------------------------------------------------------- */
#define PATH_MAX 256
#define NAME_MAX 58

#define O_READ   1
#define O_WRITE  2
#define O_CREATE 4  /* create the file if it doesn't exist */
#define O_TRUNC  8  /* empty it on open */
#define O_APPEND 16
#define O_DIR    32 /* open a folder, for SYS_READDIR */
#define O_EXCL   64 /* with O_CREATE: fail if it exists */

enum { SEEK_SET = 0, SEEK_CUR = 1, SEEK_END = 2 };
enum { FT_FILE = 1, FT_DIR = 2 };

/* Data loss prevention labels, lowest to highest. Every file has one, and so
 * does every process: the highest label of anything it has read. */
enum { LABEL_PUBLIC = 0, LABEL_PERSONAL = 1, LABEL_CONFIDENTIAL = 2, LABEL_SECRET = 3, LABEL_COUNT };

struct file_stat {
    uint64_t size;
    uint64_t mtime; /* seconds since 1970 */
    uint32_t type;  /* FT_* */
    uint32_t label; /* LABEL_* */
};

struct dir_entry {
    char name[NAME_MAX + 2];
    uint32_t type;
    uint32_t label;
    uint64_t size;
};

#define PICK_OPEN 1
#define PICK_SAVE 2
struct pick_req {
    uint32_t mode;      /* PICK_OPEN or PICK_SAVE */
    uint32_t pad;
    char suggested[NAME_MAX + 2]; /* PICK_SAVE: the file name to propose */
};
struct pick_result {
    char name[NAME_MAX + 2]; /* the chosen file's name, never its full path */
    uint32_t label;
    uint32_t pad;
};

enum { FS_NONE = 0, FS_TEMPORARY = 1, FS_ENCRYPTED = 2 };
struct fs_info {
    uint32_t kind;          /* FS_*: temporary = in memory, lost at shutdown */
    uint32_t block_size;
    uint64_t total_blocks, free_blocks;
    uint32_t total_inodes, free_inodes;
    uint32_t taint;         /* the caller's own DLP label */
    uint32_t pad;
    char device[32];
};

#define PROT_READ  1
#define PROT_WRITE 2
#define PROT_EXEC  4

/* ---- Windows ----------------------------------------------------------- */
#define WIN_MAX_W 1600
#define WIN_MAX_H 1000
#define WIN_TITLE_MAX 40
#define WIN_MIN_W 160 /* the smallest size the user can resize a window to */
#define WIN_MIN_H 100
#define WIN_FLAG_RESIZABLE 1 /* the app redraws at any size it is given (EV_RESIZE) */

struct win_req {
    uint32_t width, height; /* client area in pixels */
    char title[WIN_TITLE_MAX];
};

/* Pixels are 0x00RRGGBB, row-major, `pitch` pixels per row. */
struct win_info {
    uint64_t pixels; /* user address of the pixel buffer */
    uint32_t width, height, pitch;
    uint32_t pad;
};

/* ---- Events ------------------------------------------------------------ */
enum {
    EV_KEY = 1,
    EV_MOUSE_MOVE = 2,
    EV_MOUSE_DOWN = 3,
    EV_MOUSE_UP = 4,
    EV_CLOSE = 5, /* the user clicked the window's close button */
    EV_FOCUS = 6,
    EV_UNFOCUS = 7,
    EV_SCROLL = 8, /* mouse wheel: key = notches as int32, positive = scroll down; x, y = pointer */
    EV_WAKE = 9,   /* from SYS_EVENT_WAKE; at most one is queued at a time */
    EV_RESIZE = 10, /* x, y = the client size the user asked for; take it with SYS_WIN_RESIZE */
};

/* Key codes: printable ASCII, '\n', '\b', '\t', 27 (Esc), or one of these. */
enum {
    KEY_UP = 0x100, KEY_DOWN, KEY_LEFT, KEY_RIGHT,
    KEY_HOME, KEY_END, KEY_PGUP, KEY_PGDN, KEY_DELETE,
};
#define MOD_SHIFT 1
#define MOD_CTRL  2
#define MOD_ALT   4
#define MOD_SUPER 8 /* the logo key; the window manager keeps Super+<key> for itself */

struct event {
    uint32_t type;
    uint32_t window; /* handle */
    int32_t x, y;    /* mouse position relative to the client area */
    uint32_t key;    /* EV_KEY */
    uint32_t mods;
    uint32_t buttons; /* bit 0 left, bit 1 right, bit 2 middle */
    uint32_t pad;
};

/* ---- Information ------------------------------------------------------- */
struct time_info {
    uint64_t uptime_ms;
    uint16_t year;
    uint8_t month, day, hour, minute, second, pad;
};

#define CPU_FEAT_NX     (1u << 0)
#define CPU_FEAT_SMEP   (1u << 1)
#define CPU_FEAT_SMAP   (1u << 2)
#define CPU_FEAT_UMIP   (1u << 3)
#define CPU_FEAT_RDRAND (1u << 4)

/* The screen, and the largest window that fits between the top bar and the
 * dock (its client area, without the title bar). */
struct display_info {
    uint32_t screen_w, screen_h;
    uint32_t max_client_w, max_client_h;
};

struct sys_info {
    uint64_t mem_total_kib;
    uint64_t mem_free_kib;
    uint64_t uptime_ms;
    uint32_t nproc;
    uint32_t screen_w, screen_h;
    uint32_t cpu_features; /* CPU_FEAT_* that are present AND enabled */
    char cpu_vendor[16];
    char cpu_brand[48];
};

enum { PROC_READY = 1, PROC_BLOCKED = 2, PROC_ZOMBIE = 3 };

enum { TRUST_SYSTEM = 0, TRUST_TRUSTED = 1, TRUST_UNTRUSTED = 2 };

struct proc_info {
    uint32_t pid, parent, state, caps;
    uint64_t mem_kib;
    char name[16];
    uint32_t trust, taint; /* TRUST_*, LABEL_* */
};
