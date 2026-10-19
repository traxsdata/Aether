/*
 * monitor.c - System Monitor: memory, uptime, CPU features, storage, network
 * and the process table. It is a trusted app with exactly CAP_WINDOW and
 * CAP_SYSINFO; it cannot read files, spawn, or change anything.
 */
#include "lib/ui.h"

#define W 760
#define H 520
#define PAD 16
#define ROW_H 22
#define MAX_ROWS 12

#define C_BG 0x0D1222
#define C_BAR 0x131A31
#define C_FG 0xE2E8FA
#define C_DIM 0x7C87A8
#define C_FAINT 0x4A5578
#define C_ACCENT 0x38E1F5
#define C_OK 0x5CF2B0
#define C_WARN 0xFFB547
#define C_ERR 0xFF5C7A

static struct window win;
static struct sys_info si;
static struct proc_info procs[MAX_ROWS];
static struct fs_info fsi;
static struct net_info ni;
static struct os_info oi;
static int nprocs;
static uint64_t last;

static const char *trust_name(uint32_t t) { return t == TRUST_SYSTEM ? "system" : t == TRUST_TRUSTED ? "trusted" : "untrusted"; }

static void format_kib(uint64_t kib, char *out, size_t n) {
    if (kib < 1024) snprintf(out, n, "%lu KiB", (unsigned long)kib);
    else if (kib < 1024 * 1024) snprintf(out, n, "%lu MiB", (unsigned long)(kib / 1024));
    else snprintf(out, n, "%lu.%lu GiB", (unsigned long)(kib / 1048576), (unsigned long)((kib % 1048576) * 10 / 1048576));
}

static void format_uptime(uint64_t ms, char *out, size_t n) {
    uint64_t s = ms / 1000;
    snprintf(out, n, "%luh %02lum %02lus", (unsigned long)(s / 3600), (unsigned long)(s / 60 % 60),
             (unsigned long)(s % 60));
}

static void sample(void) {
    sys_sysinfo(&si);
    nprocs = (int)sys_proc_list(procs, MAX_ROWS);
    sys_fs_info(&fsi);
    sys_net_info(&ni);
    sys_os_info(&oi);
    last = si.uptime_ms;
}

static void stat_box(struct surface *s, int x, int y, int w, const char *label, const char *value, uint32_t color) {
    gfx_round_rect(s, x, y, w, 64, 10, 0x141B33, 255);
    gfx_round_outline(s, x, y, w, 64, 10, 0xFFFFFF, 18);
    gfx_text(s, x + 12, y + 9, label, C_FAINT, FONT_SMALL);
    gfx_text(s, x + 12, y + 31, value, color, FONT_MEDIUM);
}

static void draw(void) {
    struct surface *s = &win.s;
    gfx_fill(s, 0, 0, s->w, s->h, C_BG);
    gfx_fill(s, 0, 0, s->w, 44, C_BAR);
    gfx_text(s, PAD, 12, "System Monitor", C_FG, FONT_MEDIUM);
    char up[48];
    format_uptime(si.uptime_ms, up, sizeof up);
    gfx_text(s, s->w - PAD - gfx_text_width(up, FONT_SMALL), 16, up, C_DIM, FONT_SMALL);
    gfx_blend(s, 0, 43, s->w, 1, C_ACCENT, 50);

    /* Three headline boxes: memory, processes, storage. */
    int bw = (s->w - 2 * PAD - 2 * 12) / 3;
    char mem[48], free_mem[48], proc[32], disk[48];
    format_kib(si.mem_total_kib - si.mem_free_kib, mem, sizeof mem);
    format_kib(si.mem_free_kib, free_mem, sizeof free_mem);
    snprintf(mem + strlen(mem), sizeof mem - strlen(mem), " used / %s", free_mem);
    snprintf(proc, sizeof proc, "%lu running", (unsigned long)si.nproc);
    if (fsi.total_blocks) {
        uint64_t used = (fsi.total_blocks - fsi.free_blocks) * fsi.block_size / 1024;
        format_kib(used, disk, sizeof disk);
        snprintf(disk + strlen(disk), sizeof disk - strlen(disk), " used");
    } else {
        strlcpy(disk, "no storage", sizeof disk);
    }
    stat_box(s, PAD, 56, bw, "MEMORY", mem, C_ACCENT);
    stat_box(s, PAD + bw + 12, 56, bw, "PROCESSES", proc, C_OK);
    stat_box(s, PAD + 2 * (bw + 12), 56, bw, "STORAGE", disk, C_WARN);

    /* System details. */
    int y = 134;
    gfx_text(s, PAD, y, si.cpu_brand[0] ? si.cpu_brand : si.cpu_vendor, C_FG, FONT_SMALL);
    y += 20;
    char line[96];
    snprintf(line, sizeof line, "%s %s", oi.version[0] ? oi.version : "aether", oi.build[0] ? oi.build : "");
    gfx_text(s, PAD, y, line, C_DIM, FONT_SMALL);
    y += 20;
    snprintf(line, sizeof line, "Network: %s%s", ni.link ? "link up" : "offline", ni.configured ? ", configured" : "");
    if (ni.configured) {
        size_t n = strlen(line);
        snprintf(line + n, sizeof line - n, "  %u.%u.%u.%u", (ni.ip >> 24) & 255, (ni.ip >> 16) & 255, (ni.ip >> 8) & 255,
                 ni.ip & 255);
    }
    gfx_text(s, PAD, y, line, C_DIM, FONT_SMALL);
    y += 20;
    snprintf(line, sizeof line, "Features:%s%s%s%s%s", (si.cpu_features & CPU_FEAT_SMEP) ? " SMEP" : "",
             (si.cpu_features & CPU_FEAT_SMAP) ? " SMAP" : "", (si.cpu_features & CPU_FEAT_UMIP) ? " UMIP" : "",
             (si.cpu_features & CPU_FEAT_RDRAND) ? " RDRAND" : "", fsi.kind == FS_TEMPORARY ? "  temporary files" : "");
    gfx_text(s, PAD, y, line, C_DIM, FONT_SMALL);

    /* Process table. */
    y += 32;
    gfx_text(s, PAD, y, "PID   NAME              TRUST       TAINT        CAPS    MEMORY", C_FAINT, FONT_SMALL);
    y += 20;
    gfx_blend(s, PAD, y - 4, s->w - 2 * PAD, 1, 0xFFFFFF, 18);
    for (int i = 0; i < nprocs && i < MAX_ROWS; i++) {
        struct proc_info *p = &procs[i];
        char mem[32];
        format_kib(p->mem_kib, mem, sizeof mem);
        snprintf(line, sizeof line, "%-5lu %-17s %-11s %-12s %-7x %s", (unsigned long)p->pid, p->name,
                 trust_name(p->trust), label_name((int)p->taint), p->caps, mem);
        uint32_t ink = p->trust == TRUST_UNTRUSTED ? C_WARN : p->pid == 0 ? C_ACCENT : C_FG;
        gfx_text(s, PAD, y + i * ROW_H, line, ink, FONT_SMALL);
    }
    if (si.nproc > MAX_ROWS)
        gfx_text(s, PAD, y + MAX_ROWS * ROW_H, "(more processes are running)", C_FAINT, FONT_SMALL);

    window_present(&win);
}

int main(void) {
    if (window_open(&win, W, H, "System Monitor") < 0) return 1;
    window_set_resizable(&win, 1, 620, 420);
    sample();
    draw();
    for (;;) {
        struct event ev;
        /* Refresh once a second even when there is no input. */
        if (sys_event_wait(&ev, 1000) == 0) {
            sample();
            draw();
            continue;
        }
        switch (ev.type) {
        case EV_KEY:
            if (ev.key == 27) return 0;
            break;
        case EV_RESIZE:
            if (window_resize(&win) == 0) draw();
            break;
        case EV_CLOSE:
            return 0;
        default:
            continue;
        }
        sample();
        draw();
    }
}
