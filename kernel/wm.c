/*
 * wm.c - the æther window manager and compositor.
 *
 * Everything on screen is composed into a back buffer in RAM and then
 * copied to the framebuffer, only for the rectangle that changed ("dirty"
 * rect), so moving the mouse redraws a few hundred pixels, not the screen.
 *
 * Layers, bottom to top: wallpaper, windows (in z-order), menu bar, dock,
 * notifications, open menu, window switcher, system dialog, cursor.
 *
 * Window controls follow the familiar desktop pattern, drawn from scratch
 * here: three lights on the left of each title bar close, minimize (to the
 * dock) and zoom (fill the space between the menu bar and the dock). Super+Tab
 * or Alt+Tab opens the window switcher; Super+M minimizes, Super+Up zooms,
 * Super+W / Super+Q close. Resizable windows can also be dragged by an edge.
 * A window is resized only when its app opts in (WIN_FLAG_RESIZABLE): the
 * kernel proposes a size with EV_RESIZE and the app takes it with
 * SYS_WIN_RESIZE, which swaps in a new pixel buffer at the same address.
 *
 * Security notes:
 *   - Each window's pixel buffer is shared with exactly one process (mapped
 *     into its address space); apps never see the screen or each other.
 *   - Input goes only to the focused window's owner, so a background app
 *     can't read your keystrokes.
 *   - Title bars, the dock and notifications are drawn by the kernel, so an
 *     app can't fake them inside its own window. Each title bar shows whether
 *     the app is untrusted and the DLP label of the data it holds.
 *   - The unlock screen, the file picker and the label dialog are drawn and
 *     answered by the kernel: no app sees the passphrase or the folder
 *     listing, and no app can click "allow" for you.
 */
#include "wm.h"
#include "net.h"
#include "../common/gfx.h"
#include "afs.h"
#include "apps.h"
#include "crypto.h"
#include "dlp.h"
#include "cpu.h"
#include "input.h"
#include "kernel.h"
#include "mm.h"
#include "proc.h"
#include "rng.h"
#include "usb.h"
#include "i2c_hid.h"
#include "rtc.h"
#include "storage.h"
#include "settings.h"
#include "update.h"
#include "power.h"

#define MAX_WINDOWS 16
#define TITLE_H 36
#define WIN_RADIUS 12
#define TOPBAR_H 30
#define DOCK_ICON 52
#define DOCK_PAD 12
#define DOCK_H (DOCK_ICON + 2 * DOCK_PAD)
#define TOAST_W 360
#define TOAST_H 66
#define TOAST_MS 6000
#define MAX_TOASTS 4
#define MAX_NOTIFS 16
#define NOTIF_RATE 4
#define NOTIF_RATE_MS 10000
#define LIGHT_R 6         /* title bar lights: radius, spacing, first centre */
#define LIGHT_GAP 20
#define LIGHT_X 20
#define EDGE 6            /* grab band around a resizable window */
#define DOUBLE_CLICK_MS 400
#define ANIM_MS 220       /* minimize / restore */
#define MENU_W 240
#define MENU_ITEM_H 26
#define MENU_MAX 28
#define SWITCH_TILE 84

/* Theme */
#define C_ACCENT 0x38E1F5
#define C_ACCENT2 0xB06CFF
#define C_GLASS 0x0E1428
#define C_TEXT 0xE6ECFF
#define C_TEXT_DIM 0x8C96B8
#define C_DANGER 0xFF5C7A
#define C_AMBER 0xFFB547
#define C_MINT 0x5CF2B0
#define C_IDLE 0x3A4466 /* lights of a window that isn't focused */

struct window {
    int used, id;
    struct proc *owner;
    int x, y;   /* outer top-left (title bar) */
    int cw, ch; /* client size */
    int slot;   /* index in the owner's window address space, recycled on close */
    uint64_t buf_phys, buf_pages;
    uint64_t va; /* where the pixels are mapped in the owner */
    uint32_t *buf;
    char title[WIN_TITLE_MAX];
    uint32_t flags;      /* WIN_FLAG_* the app opted in to */
    int min_w, min_h;    /* the smallest client size the app can lay out */
    int minimized;       /* in the dock, not on screen */
    uint64_t min_seq;    /* order in the dock */
    int zoomed;          /* filling the work area; `unzoomed` is where it was */
    struct rect unzoomed;
    int resize_pending;  /* the user asked for `pend` (x, y, client w, h); the app hasn't taken it yet */
    int pend_zoomed;
    struct rect pend;
};

static struct framebuffer fb;
static struct surface screen; /* back buffer */
static uint32_t *wallpaper;
static struct window windows[MAX_WINDOWS];
static int zorder[MAX_WINDOWS], nz; /* window indices, bottom to top */
static int focused = -1;            /* window index */
static int next_window_id = 1;
static struct rect dirty;

static int mx, my, mbuttons;
static int drag_win = -1, drag_dx, drag_dy;
static int hover_lights = -1, hover_dock = -1;
static int last_minute = -1;
static uint64_t last_input; /* for the idle lock */
static uint64_t min_counter;

/* Resizing by an edge: the outline follows the mouse and the app is asked on release. */
enum { EDGE_L = 1, EDGE_R = 2, EDGE_T = 4, EDGE_B = 8 };
static int rs_win = -1, rs_edges, rs_mx, rs_my;
static struct rect rs_start, rs_cur; /* outer rects */

/* Title bar double-click zooms. */
static int click_win = -1;
static uint64_t click_at;

/* Minimize / restore animation: one at a time. */
static struct {
    int wi; /* -1: none */
    struct rect from, to, last;
    uint64_t start;
} anim = {.wi = -1};

/* Menu bar. */
enum { MENU_NONE, MENU_SYSTEM, MENU_APP, MENU_WINDOW, MENU_COUNT };
static int menu_open, menu_hover = -1;

/* Window switcher (Super+Tab / Alt+Tab): window ids, most recent first. */
static struct {
    int open, n, sel;
    int ids[MAX_WINDOWS];
} sw;

static struct {
    char title[24], msg[96];
    uint64_t until;
} toasts[MAX_TOASTS];
static int ntoasts;

/* Notification history (1.6). Toasts are transient; this ring keeps the
 * last few with the app identity so Super+N can show what was missed. */
struct notification {
    char app[16], title[40], msg[112];
    uint64_t at;
};
static struct notification notifications[MAX_NOTIFS];
static int nnotifs;
static int notif_open;

/* Per-app burst limit: an app cannot fill the screen with toasts. Unknown
 * callers share the kernel's own bucket. */
static struct {
    char app[16];
    uint64_t first;
    int count;
} notif_rate[8];

static void draw_chip(int x, int y, const char *text, uint32_t color);
static int chip_width(const char *text);
static struct rect dock_rect(void);
static struct rect dock_item_rect(int i);
static int dock_item_of_window(int wi);
static void draw_icon(const struct program *p, struct rect r, int hot);

static void damage(struct rect r) { dirty = rect_union(dirty, rect_intersect(r, (struct rect){0, 0, screen.w, screen.h})); }
static void damage_all(void) { damage((struct rect){0, 0, screen.w, screen.h}); }
static struct rect win_rect(const struct window *w) { return (struct rect){w->x, w->y, w->cw, w->ch + TITLE_H}; }
/* Area a window touches on screen, including its shadow. */
static struct rect win_damage_rect(const struct window *w) {
    struct rect r = win_rect(w);
    return (struct rect){r.x - 24, r.y - 24, r.w + 48, r.h + 56};
}

/* ---- Wallpaper ----------------------------------------------------------- */

static void render_wallpaper(void) {
    struct surface s;
    gfx_init_surface(&s, wallpaper, screen.w, screen.h, screen.w);
    gfx_gradient_v(&s, 0, 0, s.w, s.h, 0x060A18, 0x150C33);

    /* Two soft nebula glows: many faint concentric discs. */
    int cx1 = s.w / 5, cy1 = s.h / 4, cx2 = s.w * 4 / 5, cy2 = s.h * 3 / 4;
    int rmax = MAX(s.w, s.h) / 2;
    for (int r = rmax; r > 8; r -= 12) {
        gfx_circle(&s, cx1, cy1, r, 0x1FA5D6, 3);
        gfx_circle(&s, cx2, cy2, r * 4 / 5, 0x8B3DDB, 3);
    }
    /* A perspective grid fading in toward the bottom of the screen. */
    int horizon = s.h * 58 / 100;
    for (int i = 1; i < 14; i++) {
        int y = horizon + (s.h - horizon) * i * i / 169;
        gfx_blend(&s, 0, y, s.w, 1, C_ACCENT, (unsigned)(6 + i * 2));
    }
    for (int i = -12; i <= 12; i++) {
        int xb = s.w / 2 + i * s.w / 10; /* bottom x */
        int xt = s.w / 2 + i * s.w / 60; /* x at the horizon */
        for (int y = horizon; y < s.h; y++) {
            int x = xt + (xb - xt) * (y - horizon) / (s.h - horizon);
            gfx_blend(&s, x, y, 1, 1, C_ACCENT, (unsigned)(4 + 26 * (y - horizon) / (s.h - horizon)));
        }
    }
    /* Faint stars. */
    uint64_t seed = 0x5DEECE66Dull;
    for (int i = 0; i < s.w * s.h / 3000; i++) {
        seed = seed * 6364136223846793005ull + 1442695040888963407ull;
        int x = (int)((seed >> 33) % (uint64_t)s.w), y = (int)((seed >> 13) % (uint64_t)horizon);
        gfx_blend(&s, x, y, 1, 1, 0xFFFFFF, (unsigned)(40 + (seed >> 50) % 120));
    }
    /* The wordmark, faint, just above the horizon. */
    const char *mark = "\xC3\xA6ther";
    gfx_text(&s, (s.w - gfx_text_width(mark, FONT_LARGE)) / 2, horizon - 56, mark, 0x2B3E78, FONT_LARGE);
}

/* ---- Window bookkeeping ---------------------------------------------------- */

static int win_index_by_handle(struct proc *p, int handle) {
    for (int i = 0; i < MAX_WINDOWS; i++)
        if (windows[i].used && windows[i].id == handle && windows[i].owner == p) return i;
    return -1;
}

static void send(int wi, struct event ev) {
    if (wi < 0) return;
    ev.window = (uint32_t)windows[wi].id;
    proc_post_event(windows[wi].owner, &ev);
}

static void raise_window(int wi) {
    int k = -1;
    for (int i = 0; i < nz; i++)
        if (zorder[i] == wi) k = i;
    if (k < 0 || k == nz - 1) return; /* already on top */
    for (int i = k; i < nz - 1; i++) zorder[i] = zorder[i + 1];
    zorder[nz - 1] = wi;
    damage(win_damage_rect(&windows[wi]));
}

static void damage_topbar(void) { damage((struct rect){0, 0, screen.w, TOPBAR_H}); }
/* The dock changes width as windows are minimized: redraw the whole band. */
static void damage_dock(void) {
    struct rect d = dock_rect();
    damage((struct rect){0, d.y - 50, screen.w, screen.h - d.y + 50});
}

/* The program a window belongs to (for its name and dock icon). */
static const struct program *win_program(const struct window *w) { return program_find(w->owner->name); }
static const char *win_app_name(const struct window *w) {
    const struct program *p = win_program(w);
    return p && p->title ? p->title : w->owner->name;
}

/* The topmost window that is on screen, or -1. */
static int top_visible(void) {
    for (int i = nz - 1; i >= 0; i--)
        if (!windows[zorder[i]].minimized) return zorder[i];
    return -1;
}

static void focus_window(int wi) {
    if (wi == focused) {
        if (wi >= 0) raise_window(wi);
        return;
    }
    if (focused >= 0) {
        send(focused, (struct event){.type = EV_UNFOCUS});
        damage(win_damage_rect(&windows[focused]));
    }
    focused = wi;
    if (wi >= 0) {
        raise_window(wi);
        send(wi, (struct event){.type = EV_FOCUS});
    }
    damage_topbar(); /* the menu bar names the focused app */
}

static int window_by_id(int id) {
    for (int i = 0; i < MAX_WINDOWS; i++)
        if (windows[i].used && windows[i].id == id) return i;
    return -1;
}

static void destroy_window(int wi) {
    struct window *w = &windows[wi];
    damage(win_damage_rect(w));
    int k = 0;
    for (int i = 0; i < nz; i++)
        if (zorder[i] != wi) zorder[k++] = zorder[i];
    nz = k;
    /* Drop the owner's mappings before the pages can be handed to anyone
     * else. (Usually the process is already dead; this also keeps a future
     * "close this window" call safe while the app is still alive.) */
    for (uint64_t i = 0; i < w->buf_pages; i++) vmm_unmap(w->owner->pml4, w->va + i * PAGE_SIZE);
    vmm_prune(w->owner->pml4, w->va, w->va + w->buf_pages * PAGE_SIZE);
    pmm_free_contig(w->buf_phys, w->buf_pages);
    w->owner->nwindows--;
    damage_dock(); /* its app's running light, or its place among the minimized */
    w->used = 0;
    if (drag_win == wi) drag_win = -1;
    if (rs_win == wi) rs_win = -1;
    if (hover_lights == wi) hover_lights = -1;
    if (click_win == wi) click_win = -1;
    if (anim.wi == wi) {
        damage(anim.last);
        anim.wi = -1;
    }
    hover_dock = -1;
    if (focused == wi) {
        focused = -1;
        focus_window(top_visible());
    }
    damage_topbar();
    /* The switcher holds ids and checks them again when it commits. */
}

int wm_create_window(struct proc *p, uint32_t cw, uint32_t ch, const char *title, struct win_info *out) {
    if (p->nwindows >= 4) return -E_LIMIT;
    /* Recycle the lowest free window VA slot. A long-lived app (Files, Notes)
     * may open and close many windows; counting up forever would eventually
     * refuse every new one. */
    int slot = -1;
    for (int s = 0; s < 64 && slot < 0; s++) {
        int used = 0;
        for (int i = 0; i < MAX_WINDOWS; i++)
            if (windows[i].used && windows[i].owner == p && windows[i].slot == s) {
                used = 1;
                break;
            }
        if (!used) slot = s;
    }
    if (slot < 0) return -E_LIMIT;
    int wi = -1;
    for (int i = 0; i < MAX_WINDOWS; i++)
        if (!windows[i].used) {
            wi = i;
            break;
        }
    if (wi < 0) return -E_LIMIT;
    if ((int)cw > screen.w - 40) cw = (uint32_t)(screen.w - 40);
    if ((int)ch > screen.h - TOPBAR_H - DOCK_H - TITLE_H - 20) ch = (uint32_t)(screen.h - TOPBAR_H - DOCK_H - TITLE_H - 20);

    uint64_t pages = ALIGN_UP((uint64_t)cw * ch * 4, PAGE_SIZE) / PAGE_SIZE;
    uint64_t phys = pmm_alloc_contig(pages);
    if (!phys) return -E_NOMEM;
    uint64_t va = USER_WIN_BASE + (uint64_t)slot * USER_WIN_SLOT;
    for (uint64_t i = 0; i < pages; i++) {
        if (vmm_map(p->pml4, va + i * PAGE_SIZE, phys + i * PAGE_SIZE, PTE_US | PTE_RW | PTE_NX | PTE_SHARED)) {
            /* Undo every PTE installed before the failure. Freeing the
             * physical pages while user mappings still point at them would
             * leave a user-writable use-after-free. */
            for (uint64_t j = 0; j < i; j++) vmm_unmap(p->pml4, va + j * PAGE_SIZE);
            vmm_prune(p->pml4, va, va + pages * PAGE_SIZE);
            pmm_free_contig(phys, pages);
            return -E_NOMEM;
        }
    }
    p->nwindows++;

    struct window *w = &windows[wi];
    memset(w, 0, sizeof *w);
    w->used = 1;
    w->id = next_window_id++;
    w->owner = p;
    w->cw = (int)cw;
    w->ch = (int)ch;
    w->buf_phys = phys;
    w->buf_pages = pages;
    w->va = va;
    w->slot = slot;
    w->buf = P2V(phys);
    w->min_w = WIN_MIN_W;
    w->min_h = WIN_MIN_H;
    strlcpy(w->title, title, sizeof w->title);
    for (uint64_t i = 0; i < (uint64_t)cw * ch; i++) w->buf[i] = 0x10162A;

    /* Cascade new windows from the centre of the screen. */
    static int cascade;
    int avail_h = screen.h - TOPBAR_H - DOCK_H;
    w->x = (screen.w - w->cw) / 2 + (cascade % 6) * 32 - 80;
    w->y = TOPBAR_H + (avail_h - w->ch - TITLE_H) / 2 + (cascade % 6) * 28 - 60;
    cascade++;
    w->x = MAX(8, MIN(w->x, screen.w - w->cw - 8));
    w->y = MAX(TOPBAR_H + 8, MIN(w->y, screen.h - DOCK_H - w->ch - TITLE_H));

    int may_focus = p->focus_on_first_window || focused < 0 || (focused >= 0 && windows[focused].owner == p);
    p->focus_on_first_window = 0;
    if (may_focus) {
        zorder[nz++] = wi;
        focus_window(wi);
    } else {
        /* A background app's new window goes to the bottom, and input stays
         * where the user put it. It can be raised by clicking it. */
        for (int i = nz; i > 0; i--) zorder[i] = zorder[i - 1];
        zorder[0] = wi;
        nz++;
    }
    damage(win_damage_rect(w));
    damage_dock();

    out->pixels = va;
    out->width = cw;
    out->height = ch;
    out->pitch = cw;
    return w->id;
}

int wm_present(struct proc *p, int handle) {
    int wi = win_index_by_handle(p, handle);
    if (wi < 0) return -E_NOENT;
    struct window *w = &windows[wi];
    damage((struct rect){w->x, w->y + TITLE_H, w->cw, w->ch});
    return 0;
}

void wm_display_info(struct display_info *out) {
    out->screen_w = (uint32_t)screen.w;
    out->screen_h = (uint32_t)screen.h;
    out->max_client_w = (uint32_t)MIN(screen.w - 16, WIN_MAX_W);
    out->max_client_h = (uint32_t)MIN(screen.h - TOPBAR_H - DOCK_H - 24 - TITLE_H - 8, WIN_MAX_H);
}

int wm_set_title(struct proc *p, int handle, const char *title) {
    int wi = win_index_by_handle(p, handle);
    if (wi < 0) return -E_NOENT;
    strlcpy(windows[wi].title, title, sizeof windows[wi].title);
    damage((struct rect){windows[wi].x, windows[wi].y, windows[wi].cw, TITLE_H});
    return 0;
}

/* ---- Minimize, zoom and resize -------------------------------------------------- */

/* The space between the menu bar and the dock. */
static struct rect work_area(void) {
    struct rect d = dock_rect();
    return (struct rect){0, TOPBAR_H, screen.w, d.y - 8 - TOPBAR_H};
}

static int max_client_w(void) { return MIN(screen.w, WIN_MAX_W); }
static int max_client_h(void) { return MIN(screen.h - TOPBAR_H - TITLE_H, WIN_MAX_H); }

/* Ask the window's app to take a new size (client w x h, title bar at x, y).
 * Nothing moves until the app answers with SYS_WIN_RESIZE; an app that
 * ignores the request keeps its window as it was. Only the latest request
 * is kept, so a busy app can't be made to queue anything. */
static void propose(int wi, struct rect r, int zoomed) {
    struct window *w = &windows[wi];
    if (!(w->flags & WIN_FLAG_RESIZABLE)) return;
    r.w = MAX(w->min_w, MIN(r.w, max_client_w()));
    r.h = MAX(w->min_h, MIN(r.h, max_client_h()));
    r.y = MAX(TOPBAR_H, r.y);
    w->pend = r;
    w->pend_zoomed = zoomed;
    w->resize_pending = 1;
    send(wi, (struct event){.type = EV_RESIZE, .x = r.w, .y = r.h});
}

static void zoom_window(int wi) {
    struct window *w = &windows[wi];
    if (!(w->flags & WIN_FLAG_RESIZABLE) || w->minimized) return;
    if (w->zoomed) {
        propose(wi, w->unzoomed, 0);
        return;
    }
    struct rect a = work_area();
    int cw = MIN(a.w, max_client_w()), ch = MIN(a.h - TITLE_H, max_client_h());
    w->unzoomed = (struct rect){w->x, w->y, w->cw, w->ch};
    propose(wi, (struct rect){a.x + (a.w - cw) / 2, a.y + (a.h - TITLE_H - ch) / 2, cw, ch}, 1);
}

/* Swap the window's pixel buffer for a cw x ch one, mapped at the same user
 * address. The old picture is copied in so the window isn't blank until the
 * app redraws. On failure the old buffer stays exactly as it was. */
static int swap_buffer(struct window *w, int cw, int ch) {
    uint64_t pml4 = w->owner->pml4, flags = PTE_US | PTE_RW | PTE_NX | PTE_SHARED;
    uint64_t pages = ALIGN_UP((uint64_t)cw * ch * 4, PAGE_SIZE) / PAGE_SIZE;
    if (pages * PAGE_SIZE > USER_WIN_SLOT) return -E_INVAL; /* can't happen: WIN_MAX_* fit a slot */
    uint64_t phys = pmm_alloc_contig(pages);
    if (!phys) return -E_NOMEM;
    uint32_t *buf = P2V(phys);
    for (int j = 0; j < ch; j++)
        for (int i = 0; i < cw; i++)
            buf[(long)j * cw + i] = i < w->cw && j < w->ch ? w->buf[(long)j * w->cw + i] : 0x10162A;

    for (uint64_t i = 0; i < w->buf_pages; i++) vmm_unmap(pml4, w->va + i * PAGE_SIZE);
    for (uint64_t i = 0; i < pages; i++) {
        if (vmm_map(pml4, w->va + i * PAGE_SIZE, phys + i * PAGE_SIZE, flags)) {
            /* Out of memory for a page table: put the old buffer back. Its
             * page tables were never freed, so remapping it can't fail. */
            for (uint64_t j = 0; j < i; j++) vmm_unmap(pml4, w->va + j * PAGE_SIZE);
            for (uint64_t j = 0; j < w->buf_pages; j++)
                vmm_map(pml4, w->va + j * PAGE_SIZE, w->buf_phys + j * PAGE_SIZE, flags);
            if (pages > w->buf_pages) vmm_prune(pml4, w->va + w->buf_pages * PAGE_SIZE, w->va + pages * PAGE_SIZE);
            pmm_free_contig(phys, pages);
            return -E_NOMEM;
        }
    }
    if (pages < w->buf_pages) vmm_prune(pml4, w->va + pages * PAGE_SIZE, w->va + w->buf_pages * PAGE_SIZE);
    pmm_free_contig(w->buf_phys, w->buf_pages);
    w->buf_phys = phys;
    w->buf_pages = pages;
    w->buf = buf;
    w->cw = cw;
    w->ch = ch;
    return 0;
}

int wm_win_flags(struct proc *p, int handle, uint32_t flags, uint32_t min_w, uint32_t min_h) {
    int wi = win_index_by_handle(p, handle);
    if (wi < 0) return -E_NOENT;
    if (flags & ~(uint32_t)WIN_FLAG_RESIZABLE) return -E_INVAL;
    windows[wi].flags = flags;
    windows[wi].min_w = (int)MIN(MAX(min_w, (uint32_t)WIN_MIN_W), (uint32_t)max_client_w());
    windows[wi].min_h = (int)MIN(MAX(min_h, (uint32_t)WIN_MIN_H), (uint32_t)max_client_h());
    if (!(flags & WIN_FLAG_RESIZABLE)) windows[wi].resize_pending = 0;
    damage(win_damage_rect(&windows[wi])); /* the zoom light turns on or off */
    return 0;
}

int wm_win_resize(struct proc *p, int handle, struct win_info *out) {
    int wi = win_index_by_handle(p, handle);
    if (wi < 0) return -E_NOENT;
    struct window *w = &windows[wi];
    if (w->resize_pending) {
        w->resize_pending = 0;
        struct rect r = w->pend;
        struct rect old = win_damage_rect(w);
        if (r.w != w->cw || r.h != w->ch) {
            int err = swap_buffer(w, r.w, r.h);
            if (err) return err;
        }
        damage(old);
        w->x = r.x;
        w->y = r.y;
        w->zoomed = w->pend_zoomed;
        damage(win_damage_rect(w));
        kprintf("[wm] %s: window %d is now %dx%d%s\n", w->owner->name, w->id, w->cw, w->ch, w->zoomed ? " (zoomed)" : "");
    }
    out->pixels = w->va;
    out->width = (uint32_t)w->cw;
    out->height = (uint32_t)w->ch;
    out->pitch = (uint32_t)w->cw;
    return 0;
}

static void anim_start(int wi, struct rect from, struct rect to) {
    if (anim.wi >= 0) damage(anim.last);
    anim.wi = wi;
    anim.from = anim.last = from;
    anim.to = to;
    anim.start = ticks;
    damage(from);
}

static void minimize_window(int wi) {
    struct window *w = &windows[wi];
    if (w->minimized) return;
    struct rect from = win_rect(w);
    damage(win_damage_rect(w));
    damage_dock();
    w->minimized = 1;
    w->min_seq = ++min_counter;
    if (drag_win == wi) drag_win = -1;
    if (rs_win == wi) rs_win = -1;
    if (hover_lights == wi) hover_lights = -1;
    damage_dock();
    anim_start(wi, from, dock_item_rect(dock_item_of_window(wi)));
    if (focused == wi) {
        send(wi, (struct event){.type = EV_UNFOCUS});
        focused = -1;
        focus_window(top_visible());
        damage_topbar();
    }
    kprintf("[wm] %s: window %d minimized\n", w->owner->name, w->id);
}

static void restore_window(int wi) {
    struct window *w = &windows[wi];
    if (w->minimized) {
        struct rect from = dock_item_rect(dock_item_of_window(wi));
        damage_dock();
        w->minimized = 0;
        hover_dock = -1;
        anim_start(wi, from, win_rect(w));
        kprintf("[wm] %s: window %d restored\n", w->owner->name, w->id);
    }
    focus_window(wi);
    damage(win_damage_rect(w));
}

static void close_window(int wi) { /* the kernel ends the process; apps can't refuse */
    kprintf("[wm] %s: closed by the user\n", windows[wi].owner->name);
    proc_kill(windows[wi].owner, EXIT_KILLED_BY_WM, 0);
}

static void dlg_cancel_for(struct proc *p);

void wm_proc_exited(struct proc *p) {
    dlg_cancel_for(p);
    for (int i = 0; i < MAX_WINDOWS; i++)
        if (windows[i].used && windows[i].owner == p) destroy_window(i);
}

static int notif_allowed(const char *app) {
    int slot = -1, free = -1;
    for (int i = 0; i < (int)ARRAY_LEN(notif_rate); i++) {
        if (!notif_rate[i].app[0]) { free = i; break; }
        if (!strcmp(notif_rate[i].app, app)) { slot = i; break; }
    }
    if (slot < 0) {
        slot = free;
        if (slot < 0) slot = 0; /* all buckets in use: reuse the first */
        memset(&notif_rate[slot], 0, sizeof notif_rate[slot]);
        strlcpy(notif_rate[slot].app, app, sizeof notif_rate[slot].app);
        notif_rate[slot].first = ticks;
    }
    if (ticks - notif_rate[slot].first >= NOTIF_RATE_MS) {
        notif_rate[slot].first = ticks;
        notif_rate[slot].count = 0;
    }
    if (++notif_rate[slot].count > NOTIF_RATE) return 0;
    return 1;
}

int wm_notify_from(const char *app, const char *title, const char *msg) {
    if (!app || !app[0]) app = "app";
    if (!notif_allowed(app)) {
        kprintf("[wm] %s: notification rate limit reached\n", app);
        return -E_LIMIT;
    }
    if (nnotifs == MAX_NOTIFS) {
        for (int i = 1; i < MAX_NOTIFS; i++) notifications[i - 1] = notifications[i];
        nnotifs--;
    }
    memset(&notifications[nnotifs], 0, sizeof notifications[nnotifs]);
    strlcpy(notifications[nnotifs].app, app, sizeof notifications[nnotifs].app);
    strlcpy(notifications[nnotifs].title, title ? title : "", sizeof notifications[nnotifs].title);
    strlcpy(notifications[nnotifs].msg, msg ? msg : "", sizeof notifications[nnotifs].msg);
    notifications[nnotifs].at = ticks;
    nnotifs++;

    if (ntoasts == MAX_TOASTS) {
        for (int i = 1; i < MAX_TOASTS; i++) toasts[i - 1] = toasts[i];
        ntoasts--;
    }
    strlcpy(toasts[ntoasts].title, title ? title : app, sizeof toasts[0].title);
    strlcpy(toasts[ntoasts].msg, msg ? msg : "", sizeof toasts[0].msg);
    toasts[ntoasts].until = ticks + TOAST_MS;
    ntoasts++;
    damage((struct rect){screen.w - TOAST_W - 40, TOPBAR_H, TOAST_W + 40, (TOAST_H + 10) * MAX_TOASTS + 40});
    damage_topbar();
    if (notif_open) damage_all();
    return 0;
}

void wm_notify(const char *title, const char *msg) { wm_notify_from("\xC3\xA6ther", title, msg); }

void wm_notifications_clear(void) {
    nnotifs = 0;
    if (notif_open) damage_all();
}

int wm_notifications_visible(void) { return nnotifs; }

/* ---- Layout helpers ------------------------------------------------------------ */

/* Programs with a title that can open a window (console-only apps stay out). */
static int in_dock(const struct program *p) { return p->title && (p->caps & CAP_WINDOW); }

static int dock_count(void) {
    int n = 0;
    for (int i = 0; program_at(i); i++)
        if (in_dock(program_at(i))) n++;
    return n;
}

/* The dock: app icons, then (after a divider) minimized windows, oldest
 * first. Only as many minimized windows as fit on screen are shown; the
 * rest are still reachable from the switcher and the Window menu. */
#define DOCK_DIVIDER (DOCK_PAD + 1)

static int dock_minimized(int *out) { /* window indices in dock order */
    int n = 0;
    for (int i = 0; i < MAX_WINDOWS; i++)
        if (windows[i].used && windows[i].minimized) {
            int k = n++;
            while (k > 0 && windows[out[k - 1]].min_seq > windows[i].min_seq) {
                out[k] = out[k - 1];
                k--;
            }
            out[k] = i;
        }
    int fit = (screen.w - 40) / (DOCK_ICON + DOCK_PAD) - dock_count();
    return MAX(0, MIN(n, fit));
}

static int dock_items(void) {
    int tmp[MAX_WINDOWS];
    return dock_count() + dock_minimized(tmp);
}

static struct rect dock_rect(void) {
    int tmp[MAX_WINDOWS];
    int nmin = dock_minimized(tmp), n = dock_count() + nmin;
    int w = n * DOCK_ICON + (n + 1) * DOCK_PAD + 2 * 8 + (nmin ? DOCK_DIVIDER : 0);
    return (struct rect){(screen.w - w) / 2, screen.h - DOCK_H - 12, w, DOCK_H};
}

/* Item i: an app icon for i < dock_count(), else a minimized window. -1 is
 * the middle of the dock (where a window goes that doesn't fit). */
static struct rect dock_item_rect(int i) {
    struct rect d = dock_rect();
    if (i < 0) return (struct rect){d.x + d.w / 2 - DOCK_ICON / 2, d.y + DOCK_PAD, DOCK_ICON, DOCK_ICON};
    int x = d.x + 8 + DOCK_PAD + i * (DOCK_ICON + DOCK_PAD) + (i >= dock_count() ? DOCK_DIVIDER : 0);
    return (struct rect){x, d.y + DOCK_PAD, DOCK_ICON, DOCK_ICON};
}

static int dock_window(int i) { /* the minimized window at item i, or -1 */
    int mins[MAX_WINDOWS], n = dock_minimized(mins), k = i - dock_count();
    return k >= 0 && k < n ? mins[k] : -1;
}

static int dock_item_of_window(int wi) {
    int mins[MAX_WINDOWS], n = dock_minimized(mins);
    for (int k = 0; k < n; k++)
        if (mins[k] == wi) return dock_count() + k;
    return -1;
}

static const struct program *dock_program(int k) {
    for (int i = 0, n = 0; program_at(i); i++)
        if (in_dock(program_at(i)) && n++ == k) return program_at(i);
    return 0;
}

/* Title bar lights: 0 close, 1 minimize, 2 zoom. */
static struct rect light_rect(const struct window *w, int k) {
    return (struct rect){w->x + LIGHT_X + k * LIGHT_GAP - LIGHT_GAP / 2, w->y + TITLE_H / 2 - LIGHT_GAP / 2, LIGHT_GAP, LIGHT_GAP};
}
static struct rect lights_rect(const struct window *w) {
    return (struct rect){w->x + LIGHT_X - LIGHT_GAP / 2, w->y + TITLE_H / 2 - LIGHT_GAP / 2, 3 * LIGHT_GAP, LIGHT_GAP};
}
static int light_at(const struct window *w, int x, int y) {
    for (int k = 0; k < 3; k++)
        if (rect_contains(light_rect(w, k), x, y)) return k;
    return -1;
}

/* ---- Drawing --------------------------------------------------------------------- */

/* The three lights. They show their symbols while the pointer is over any
 * of them; a window that can't be resized has its zoom light hollow. */
static void draw_lights(int wi) {
    struct window *w = &windows[wi];
    int focus = wi == focused, hot = hover_lights == wi;
    static const uint32_t colors[3] = {C_DANGER, C_AMBER, C_MINT};
    for (int k = 0; k < 3; k++) {
        int cx = w->x + LIGHT_X + k * LIGHT_GAP, cy = w->y + TITLE_H / 2;
        int enabled = k != 2 || (w->flags & WIN_FLAG_RESIZABLE);
        uint32_t c = (focus || hot) && enabled ? colors[k] : C_IDLE;
        if (!enabled) {
            gfx_circle(&screen, cx, cy, LIGHT_R, C_IDLE, 255);
            gfx_circle(&screen, cx, cy, LIGHT_R - 1, focus ? 0x1A2344 : 0x151B33, 255);
            continue;
        }
        gfx_circle(&screen, cx, cy, LIGHT_R, c, 255);
        gfx_circle(&screen, cx, cy - 2, LIGHT_R - 3, 0xFFFFFF, 30); /* a little gloss */
        if (!hot) continue;
        uint32_t ink = 0x2A0A14;
        if (k == 0) { /* x */
            for (int d = -3; d <= 3; d++) {
                gfx_blend(&screen, cx + d, cy + d, 1, 1, ink, 220);
                gfx_blend(&screen, cx + d, cy - d, 1, 1, ink, 220);
            }
        } else if (k == 1) { /* - */
            gfx_blend(&screen, cx - 3, cy, 7, 1, 0x3A2600, 230);
        } else { /* two arrowheads pointing apart, or together when zoomed */
            uint32_t g = 0x063A26;
            for (int j = 0; j < 4; j++) {
                int len = w->zoomed ? j + 1 : 4 - j;
                gfx_blend(&screen, cx - 3, cy - 3 + j, len, 1, g, 230);
                gfx_blend(&screen, cx + 4 - len, cy + j, len, 1, g, 230);
            }
        }
    }
}

static void draw_window(int wi) {
    struct window *w = &windows[wi];
    int focus = wi == focused;
    struct rect r = win_rect(w);
    if (rect_empty(rect_intersect(win_damage_rect(w), screen.clip))) return;

    gfx_shadow(&screen, r.x, r.y, r.w, r.h, WIN_RADIUS, focus ? 18 : 10, focus ? 150 : 90);
    gfx_round_rect(&screen, r.x, r.y, r.w, r.h, WIN_RADIUS, 0x10162A, 255);

    /* Glass title bar: tinted, with a thin accent line when focused. */
    struct rect save = screen.clip;
    gfx_set_clip(&screen, rect_intersect(save, (struct rect){r.x, r.y, r.w, TITLE_H}));
    gfx_round_rect(&screen, r.x, r.y, r.w, TITLE_H + WIN_RADIUS, WIN_RADIUS, focus ? 0x1A2344 : 0x151B33, 255);
    gfx_blend(&screen, r.x, r.y, r.w, TITLE_H / 2, 0xFFFFFF, 8);
    screen.clip = save;
    if (focus) {
        gfx_blend(&screen, r.x + 12, r.y + TITLE_H - 1, r.w - 24, 1, C_ACCENT, 90);
    }
    int tw = gfx_text_width(w->title, FONT_SMALL);
    gfx_text(&screen, r.x + (r.w - tw) / 2, r.y + 10, w->title, focus ? C_TEXT : C_TEXT_DIM, FONT_SMALL);
    draw_lights(wi);
    /* Kernel-drawn badges an app can't paint over: untrusted, and its data label. */
    int bx = r.x + LIGHT_X + 3 * LIGHT_GAP;
    if (w->owner->trust == TRUST_UNTRUSTED) {
        draw_chip(bx, r.y + 8, "untrusted", 0xFFB547);
        bx += chip_width("untrusted") + 6;
    }
    if (w->owner->taint > LABEL_PUBLIC) draw_chip(bx, r.y + 8, dlp_label_name((int)w->owner->taint), dlp_label_color((int)w->owner->taint));

    /* An app update was installed while this copy was running. The new
     * version starts next time; this window keeps running the old code. */
    if (w->owner->update_pending) {
        const char *note = "update pending";
        int nw = chip_width(note);
        if (r.w > nw + 80) draw_chip(r.x + r.w - 12 - nw, r.y + 8, note, C_AMBER);
    }

    /* Client area, with the bottom corners rounded off. */
    int cy = r.y + TITLE_H;
    for (int j = 0; j < w->ch; j++) {
        int inset = 0, from_bottom = w->ch - 1 - j;
        if (from_bottom < WIN_RADIUS) {
            int d = WIN_RADIUS - from_bottom;
            int k = 0;
            while (k < WIN_RADIUS && (WIN_RADIUS - k) * (WIN_RADIUS - k) + d * d > WIN_RADIUS * WIN_RADIUS) k++;
            inset = k;
        }
        gfx_blit(&screen, r.x + inset, cy + j, w->buf + (long)j * w->cw + inset, w->cw - 2 * inset, 1, w->cw);
    }
    gfx_round_outline(&screen, r.x, r.y, r.w, r.h, WIN_RADIUS, focus ? C_ACCENT : 0xFFFFFF, focus ? 110 : 28);
}

static void draw_shield(int x, int y, uint32_t color) {
    /* A tiny shield glyph: rectangle top narrowing to a point. */
    for (int j = 0; j < 14; j++) {
        int half = j < 7 ? 6 : 6 - (j - 6);
        gfx_blend(&screen, x + 6 - half, y + j, half * 2, 1, color, 230);
    }
    gfx_blend(&screen, x + 5, y + 3, 2, 7, 0x0B1020, 200);
    gfx_blend(&screen, x + 3, y + 5, 6, 2, 0x0B1020, 200);
}

/* ---- Menu bar ------------------------------------------------------------------------ */

/* Left: the æ menu (apps and the desktop), the focused app's menu, and Window.
 * Right: the security status and the clock. Every menu is kernel-drawn, so an
 * app can't fake one or see what is chosen from it. */
enum { ACT_NONE, ACT_LAUNCH, ACT_NEW, ACT_QUIT, ACT_MINIMIZE, ACT_ZOOM, ACT_CLOSE, ACT_SHOW_ALL, ACT_HIDE_ALL, ACT_FOCUS,
       ACT_LOCK, ACT_RESTART, ACT_SHUTDOWN };

struct menu_item {
    char text[WIN_TITLE_MAX + 16];
    const char *keys; /* shortcut hint, or 0 */
    int act, arg;     /* ACT_LAUNCH: index into launch_names; ACT_FOCUS: window id */
    int enabled, checked, separator;
};

static const char *const launch_names[] = {"settings", "installer", "explorer", "terminal"};

static const char *menu_label(int m) {
    if (m == MENU_SYSTEM) return "\xC3\xA6";
    if (m == MENU_APP) return focused >= 0 ? win_app_name(&windows[focused]) : "Desktop";
    return "Window";
}

static struct rect menu_title_rect(int m) {
    int x = 6;
    for (int k = MENU_SYSTEM; k < m; k++) x += gfx_text_width(menu_label(k), FONT_SMALL) + 20 + (k == MENU_APP);
    return (struct rect){x, 0, gfx_text_width(menu_label(m), FONT_SMALL) + 20 + (m == MENU_APP), TOPBAR_H};
}

static int add_item(struct menu_item *it, int n, const char *text, const char *keys, int act, int arg, int enabled) {
    if (n >= MENU_MAX) return n;
    memset(&it[n], 0, sizeof it[n]);
    strlcpy(it[n].text, text, sizeof it[n].text);
    it[n].keys = keys;
    it[n].act = act;
    it[n].arg = arg;
    it[n].enabled = enabled;
    return n + 1;
}

static int add_separator(struct menu_item *it, int n) {
    if (n >= MENU_MAX) return n;
    memset(&it[n], 0, sizeof it[n]);
    it[n].separator = 1;
    return n + 1;
}

static int any_minimized(void) {
    for (int i = 0; i < MAX_WINDOWS; i++)
        if (windows[i].used && windows[i].minimized) return 1;
    return 0;
}

static int build_menu(int m, struct menu_item *it) {
    int n = 0;
    char text[WIN_TITLE_MAX + 16];
    struct window *f = focused >= 0 ? &windows[focused] : 0;
    if (m == MENU_SYSTEM) {
        static const char *const labels[] = {"Settings...", "App Store...", "Files", "Terminal"};
        static const char *const keys[] = {0, 0, 0, "Ctrl+Alt+T"};
        for (int i = 0; i < (int)ARRAY_LEN(launch_names); i++)
            n = add_item(it, n, labels[i], keys[i], ACT_LAUNCH, i, program_find(launch_names[i]) != 0);
        n = add_separator(it, n);
        n = add_item(it, n, "Minimize All Windows", 0, ACT_HIDE_ALL, 0, top_visible() >= 0);
        n = add_item(it, n, "Show All Windows", 0, ACT_SHOW_ALL, 0, any_minimized());
        n = add_separator(it, n);
        n = add_item(it, n, "Lock Screen", "Super+L", ACT_LOCK, 0, !afs_temporary());
        n = add_item(it, n, "Restart...", 0, ACT_RESTART, 0, 1);
        n = add_item(it, n, "Shut Down...", 0, ACT_SHUTDOWN, 0, 1);
    } else if (m == MENU_APP) {
        if (!f) return add_item(it, n, "No app is active", 0, ACT_NONE, 0, 0);
        const struct program *p = win_program(f);
        snprintf(text, sizeof text, "New %s Window", win_app_name(f));
        n = add_item(it, n, text, 0, ACT_NEW, 0, p && in_dock(p));
        n = add_separator(it, n);
        snprintf(text, sizeof text, "Quit %s", win_app_name(f));
        n = add_item(it, n, text, "Super+Q", ACT_QUIT, 0, 1);
    } else {
        n = add_item(it, n, "Minimize", "Super+M", ACT_MINIMIZE, 0, f != 0);
        n = add_item(it, n, f && f->zoomed ? "Unzoom" : "Zoom", "Super+Up", ACT_ZOOM, 0,
                     f && (f->flags & WIN_FLAG_RESIZABLE));
        n = add_item(it, n, "Close", "Super+W", ACT_CLOSE, 0, f != 0);
        n = add_separator(it, n);
        n = add_item(it, n, "Show All Windows", 0, ACT_SHOW_ALL, 0, any_minimized());
        if (nz) n = add_separator(it, n);
        /* Every window, front to back; minimized ones are marked. */
        for (int pass = 0; pass < 2; pass++)
            for (int i = nz - 1; i >= 0; i--) {
                struct window *w = &windows[zorder[i]];
                if (w->minimized != pass) continue;
                n = add_item(it, n, w->title[0] ? w->title : win_app_name(w), pass ? "in dock" : 0, ACT_FOCUS, w->id, 1);
                if (n && zorder[i] == focused) it[n - 1].checked = 1;
            }
    }
    return n;
}

static int item_h(const struct menu_item *it) { return it->separator ? 9 : MENU_ITEM_H; }

static struct rect menu_rect(int m, const struct menu_item *it, int n) {
    int h = 12, w = MENU_W;
    for (int i = 0; i < n; i++) {
        h += item_h(&it[i]);
        int need = gfx_text_width(it[i].text, FONT_SMALL) + (it[i].keys ? gfx_text_width(it[i].keys, FONT_SMALL) + 24 : 0) + 48;
        w = MAX(w, need);
    }
    w = MIN(w, screen.w - 16);
    struct rect t = menu_title_rect(m);
    return (struct rect){MIN(t.x, screen.w - w - 8), TOPBAR_H + 2, w, h};
}

static int menu_item_at(int m, const struct menu_item *it, int n, int x, int y) {
    struct rect r = menu_rect(m, it, n);
    if (!rect_contains(r, x, y)) return -1;
    int yy = r.y + 6;
    for (int i = 0; i < n; i++) {
        if (y >= yy && y < yy + item_h(&it[i])) return it[i].separator ? -1 : i;
        yy += item_h(&it[i]);
    }
    return -1;
}

static void damage_menu(void) {
    if (!menu_open) return;
    struct menu_item it[MENU_MAX];
    int n = build_menu(menu_open, it);
    struct rect r = menu_rect(menu_open, it, n);
    damage((struct rect){r.x - 24, r.y - 8, r.w + 48, r.h + 40});
}

static void open_menu(int m) {
    damage_menu();
    damage_topbar();
    menu_open = m;
    menu_hover = -1;
    damage_menu();
}

static void close_menu(void) { open_menu(MENU_NONE); }

static void draw_menu(void) {
    if (!menu_open) return;
    struct menu_item it[MENU_MAX];
    int n = build_menu(menu_open, it);
    struct rect r = menu_rect(menu_open, it, n);
    gfx_shadow(&screen, r.x, r.y, r.w, r.h, 10, 14, 120);
    gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 10, 0x0C1226, 242);
    gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 10, 0xFFFFFF, 34);
    int y = r.y + 6;
    for (int i = 0; i < n; i++) {
        if (it[i].separator) {
            gfx_blend(&screen, r.x + 12, y + 4, r.w - 24, 1, 0xFFFFFF, 30);
        } else {
            int hot = i == menu_hover && it[i].enabled;
            if (hot) gfx_round_rect(&screen, r.x + 6, y, r.w - 12, MENU_ITEM_H, 6, C_ACCENT, 210);
            uint32_t ink = hot ? 0x060A18 : it[i].enabled ? C_TEXT : 0x5A6488;
            if (it[i].checked) gfx_circle(&screen, r.x + 18, y + MENU_ITEM_H / 2, 3, ink, 255);
            /* Keep long window titles inside the menu. */
            char text[WIN_TITLE_MAX + 16];
            strlcpy(text, it[i].text, sizeof text);
            int room = (r.w - 40 - (it[i].keys ? gfx_text_width(it[i].keys, FONT_SMALL) + 16 : 0)) / gfx_font_w(FONT_SMALL);
            if (room > 3 && (int)strlen(text) > room) strlcpy(text + room - 3, "...", 4);
            gfx_text(&screen, r.x + 30, y + 5, text, ink, FONT_SMALL);
            if (it[i].keys)
                gfx_text(&screen, r.x + r.w - 16 - gfx_text_width(it[i].keys, FONT_SMALL), y + 5, it[i].keys,
                         hot ? 0x0B2A3A : 0x6E7899, FONT_SMALL);
        }
        y += item_h(&it[i]);
    }
}

static void draw_shield(int x, int y, uint32_t color);
static void draw_bell(void);

static void draw_topbar(void) {
    gfx_blend(&screen, 0, 0, screen.w, TOPBAR_H, 0x070B18, 200);
    gfx_blend(&screen, 0, TOPBAR_H - 1, screen.w, 1, C_ACCENT, 50);
    for (int m = MENU_SYSTEM; m < MENU_COUNT; m++) {
        struct rect t = menu_title_rect(m);
        if (menu_open == m) gfx_round_rect(&screen, t.x + 2, 4, t.w - 4, TOPBAR_H - 8, 7, 0xFFFFFF, 34);
        const char *label = menu_label(m);
        if (m == MENU_SYSTEM) {
            gfx_text(&screen, t.x + 10, 7, label, C_ACCENT, FONT_SMALL);
        } else if (m == MENU_APP) { /* the active app's name, drawn heavier */
            gfx_text(&screen, t.x + 10, 7, label, 0xFFFFFF, FONT_SMALL);
            gfx_text(&screen, t.x + 11, 7, label, 0xFFFFFF, FONT_SMALL);
        } else {
            gfx_text(&screen, t.x + 10, 7, label, C_TEXT, FONT_SMALL);
        }
    }

    struct rtc_time t;
    rtc_now(&t);
    static const char *months[] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun",
                                   "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};
    char clock[48];
    snprintf(clock, sizeof clock, "%s %u   %02u:%02u", months[(t.month + 11) % 12], t.day, t.hour, t.minute);
    int cx = screen.w - 46 - gfx_text_width(clock, FONT_SMALL);
    gfx_text(&screen, cx, 7, clock, C_TEXT, FONT_SMALL);
    draw_bell();

    /* Security status pill, left of the clock: which protections are active.
     * Shortened when the screen is too narrow for all of it. */
    char sec[96];
    static const char *net[] = {"offline", "firewall on, connecting", "firewall on"};
    snprintf(sec, sizeof sec, "NX%s%s%s  sandboxed  %s  %s", (cpu_features & CPU_FEAT_SMEP) ? " SMEP" : "",
             (cpu_features & CPU_FEAT_SMAP) ? " SMAP" : "", (cpu_features & CPU_FEAT_UMIP) ? " UMIP" : "",
             afs_temporary() ? "temporary files" : "files encrypted", net[net_state()]);
    struct rect last = menu_title_rect(MENU_COUNT - 1);
    int room = cx - 24 - (last.x + last.w + 24) - 36;
    if (gfx_text_width(sec, FONT_SMALL) > room) {
        snprintf(sec, sizeof sec, "%s  %s", afs_temporary() ? "temporary files" : "encrypted", net[net_state()]);
        if (gfx_text_width(sec, FONT_SMALL) > room) sec[0] = 0;
    }
    int pw = gfx_text_width(sec, FONT_SMALL) + 36;
    int px = cx - 20 - pw;
    gfx_round_rect(&screen, px, 5, pw, 20, 10, 0x13324A, 200);
    draw_shield(px + 10, 8, C_MINT);
    gfx_text(&screen, px + 28, 7, sec, 0x9FF5D2, FONT_SMALL);
}

static int program_running(const char *name) {
    for (int i = 0; i < MAX_WINDOWS; i++)
        if (windows[i].used && !strcmp(windows[i].owner->name, name)) return 1;
    return 0;
}

static void draw_icon(const struct program *p, struct rect r, int hot) {
    if (!strcmp(p->name, "terminal")) {
        gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 14, 0x0D1324, 255);
        gfx_round_rect(&screen, r.x, r.y, r.w, r.h / 2, 14, 0x1B2440, 120);
        gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 14, C_ACCENT, hot ? 200 : 90);
        gfx_text(&screen, r.x + 10, r.y + 14, ">_", C_ACCENT, FONT_MEDIUM);
    } else if (!strcmp(p->name, "calculator")) {
        gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 14, 0x6D3BE8, 255);
        gfx_round_rect(&screen, r.x, r.y + r.h / 2, r.w, r.h / 2, 14, 0xD9367F, 140);
        gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 14, 0xFFFFFF, hot ? 200 : 70);
        gfx_text(&screen, r.x + 12, r.y + 8, "+", 0xFFFFFF, FONT_MEDIUM);
        gfx_text(&screen, r.x + 28, r.y + 8, "-", 0xFFFFFF, FONT_MEDIUM);
        gfx_text(&screen, r.x + 12, r.y + 24, "x", 0xFFFFFF, FONT_MEDIUM);
        gfx_text(&screen, r.x + 28, r.y + 24, "=", 0xFFFFFF, FONT_MEDIUM);
    } else if (!strcmp(p->name, "notes")) {
        gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 14, 0xF5B83D, 255);
        gfx_round_rect(&screen, r.x, r.y, r.w, 14, 14, 0xE0912B, 255);
        gfx_blend(&screen, r.x, r.y + 10, r.w, 4, 0xE0912B, 255);
        for (int i = 0; i < 4; i++) gfx_blend(&screen, r.x + 10, r.y + 22 + i * 7, i == 3 ? 18 : 32, 2, 0x7A4A12, 200);
        gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 14, 0xFFFFFF, hot ? 200 : 70);
    } else if (!strcmp(p->name, "explorer")) {
        /* Files: a folder on deep blue glass, with a sheet peeking out. */
        gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 14, 0x123A6B, 255);
        gfx_round_rect(&screen, r.x, r.y, r.w, r.h / 2, 14, 0xFFFFFF, 26);
        gfx_round_rect(&screen, r.x + 10, r.y + 13, 16, 8, 3, 0x2F8FE0, 255);
        gfx_round_rect(&screen, r.x + 14, r.y + 15, 24, 14, 2, 0xE6ECFF, 255);
        gfx_round_rect(&screen, r.x + 10, r.y + 19, 32, 21, 4, 0x4FB3FF, 255);
        gfx_blend(&screen, r.x + 12, r.y + 22, 28, 1, 0xFFFFFF, 90);
        gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 14, C_ACCENT, hot ? 200 : 80);
    } else if (p->icon && p->icon_w && p->icon_h) {
        /* A package icon: RGBA8888, drawn at the dock's 52x52. */
        gfx_image(&screen, r.x, r.y, r.w, r.h, p->icon, (int)p->icon_w, (int)p->icon_h);
        if (hot) gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 14, 0xFFFFFF, 140);
    } else {
        /* Installed apps: their colour and initial. */
        uint32_t color = p->icon_color ? p->icon_color : 0x334155;
        gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 14, color, 255);
        gfx_round_rect(&screen, r.x, r.y, r.w, r.h / 2, 14, 0xFFFFFF, 40);
        gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 14, 0xFFFFFF, hot ? 200 : 70);
        char c[2] = {(char)(p->title[0]), 0};
        gfx_text(&screen, r.x + (r.w - gfx_text_width(c, FONT_MEDIUM)) / 2, r.y + 14, c, 0xFFFFFF, FONT_MEDIUM);
    }
}

/* A scaled-down copy of a window's pixels (nearest neighbour), for the dock,
 * the window switcher and the minimize animation. */
static void draw_scaled(struct rect d, const uint32_t *src, int sw, int sh, unsigned alpha) {
    if (d.w <= 0 || d.h <= 0 || sw <= 0 || sh <= 0) return;
    struct rect c = rect_intersect(d, screen.clip);
    for (int y = c.y; y < c.y + c.h; y++) {
        const uint32_t *row = src + (long)((y - d.y) * sh / d.h) * sw;
        uint32_t *out = &screen.px[(long)y * screen.pitch];
        for (int x = c.x; x < c.x + c.w; x++) {
            uint32_t p = row[(long)(x - d.x) * sw / d.w] & 0xFFFFFF;
            out[x] = alpha >= 255 ? p : gfx_mix(out[x], p, alpha);
        }
    }
}

/* A window as a picture: a title bar strip over its scaled client area. */
static void draw_window_picture(int wi, struct rect r, unsigned alpha) {
    struct window *w = &windows[wi];
    if (r.w < 4 || r.h < 4) return;
    int th = MAX(2, r.h * TITLE_H / (w->ch + TITLE_H));
    gfx_blend(&screen, r.x, r.y, r.w, th, 0x1A2344, alpha);
    draw_scaled((struct rect){r.x, r.y + th, r.w, r.h - th}, w->buf, w->cw, w->ch, alpha);
}

/* A minimized window in the dock: its picture, fitted into the icon square,
 * with its app's icon in the corner so a window can't pass for another app. */
static void draw_dock_window(int wi, struct rect r, int hot) {
    struct window *w = &windows[wi];
    int ow = w->cw, oh = w->ch + TITLE_H;
    int pw = ow >= oh ? r.w : r.w * ow / oh, ph = ow >= oh ? r.h * oh / ow : r.h;
    struct rect p = {r.x + (r.w - pw) / 2, r.y + (r.h - ph) / 2, pw, ph};
    gfx_shadow(&screen, p.x, p.y, p.w, p.h, 4, 4, 80);
    draw_window_picture(wi, p, 255);
    gfx_round_outline(&screen, p.x, p.y, p.w, p.h, 4, 0xFFFFFF, hot ? 200 : 60);
    const struct program *prog = win_program(w);
    if (prog) {
        /* The badge is the app's colour and initial: small and kernel-drawn. */
        uint32_t color = prog->icon_color ? prog->icon_color : 0x334155;
        int bx = r.x + r.w - 18, by = r.y + r.h - 18;
        gfx_round_rect(&screen, bx, by, 20, 20, 6, color, 255);
        gfx_round_outline(&screen, bx, by, 20, 20, 6, 0xFFFFFF, 120);
        char c[2] = {prog->title ? prog->title[0] : w->owner->name[0], 0};
        gfx_text(&screen, bx + (20 - gfx_text_width(c, FONT_SMALL)) / 2, by + 2, c, 0xFFFFFF, FONT_SMALL);
    }
}

static void draw_tooltip(struct rect r, struct rect d, const char *text) {
    char t[WIN_TITLE_MAX + 4];
    strlcpy(t, text, sizeof t);
    int tw = gfx_text_width(t, FONT_SMALL) + 20;
    int tx = MAX(4, MIN(screen.w - tw - 4, r.x + r.w / 2 - tw / 2)), ty = d.y - 34;
    gfx_round_rect(&screen, tx, ty, tw, 26, 8, 0x0B1020, 230);
    gfx_text(&screen, tx + 10, ty + 5, t, C_TEXT, FONT_SMALL);
}

static void draw_dock(void) {
    struct rect d = dock_rect();
    gfx_shadow(&screen, d.x, d.y, d.w, d.h, 20, 12, 90);
    gfx_round_rect(&screen, d.x, d.y, d.w, d.h, 20, C_GLASS, 200);
    gfx_blend(&screen, d.x + 20, d.y, d.w - 40, 1, 0xFFFFFF, 40);
    gfx_round_outline(&screen, d.x, d.y, d.w, d.h, 20, 0xFFFFFF, 30);
    int napps = dock_count(), n = dock_items();
    if (n > napps) { /* divider between apps and minimized windows */
        struct rect first = dock_item_rect(napps);
        gfx_blend(&screen, first.x - DOCK_PAD / 2 - DOCK_DIVIDER / 2 - 1, d.y + 14, 1, d.h - 28, 0xFFFFFF, 60);
    }
    for (int k = 0; k < n; k++) {
        struct rect r = dock_item_rect(k);
        int hot = hover_dock == k;
        if (hot) r.y -= 4;
        if (k < napps) {
            const struct program *p = dock_program(k);
            draw_icon(p, r, hot);
            /* Running apps get a light under their icon. */
            if (program_running(p->name)) {
                gfx_circle(&screen, r.x + r.w / 2, d.y + d.h - 6 + (hot ? 0 : 0), 4, C_ACCENT, 60);
                gfx_circle(&screen, r.x + r.w / 2, d.y + d.h - 6, 2, C_ACCENT, 255);
            }
            if (hot) draw_tooltip(r, d, p->title);
        } else {
            int wi = dock_window(k);
            if (wi < 0) continue;
            draw_dock_window(wi, r, hot);
            if (hot) draw_tooltip(r, d, windows[wi].title[0] ? windows[wi].title : win_app_name(&windows[wi]));
        }
    }
}

/* The minimize / restore animation: the window's picture flies between its
 * place and the dock, easing out. */
static struct rect anim_rect(void) {
    uint64_t t = MIN(ticks - anim.start, (uint64_t)ANIM_MS);
    int k = (int)(t * 256 / ANIM_MS);
    k = 256 - (256 - k) * (256 - k) / 256; /* ease out */
    struct rect a = anim.from, b = anim.to;
    return (struct rect){a.x + (b.x - a.x) * k / 256, a.y + (b.y - a.y) * k / 256, a.w + (b.w - a.w) * k / 256,
                         a.h + (b.h - a.h) * k / 256};
}

static void draw_anim(void) {
    if (anim.wi < 0) return;
    draw_window_picture(anim.wi, anim.last, 230); /* the frame housekeeping() damaged */
}

static struct rect toast_rect(int i) {
    return (struct rect){screen.w - TOAST_W - 16, TOPBAR_H + 12 + i * (TOAST_H + 10), TOAST_W, TOAST_H};
}

/* The bell sits at the right end of the menu bar; the clock moves left to
 * make room for it. Clicking it (or Super+N) opens the history. */
static struct rect notif_button_rect(void) { return (struct rect){screen.w - 36, 4, 26, 22}; }
static struct rect notif_panel_rect(void) {
    int shown = MIN(nnotifs, 10);
    int h = 56 + shown * 42 + (shown ? 10 : 4);
    return (struct rect){screen.w - 396, TOPBAR_H + 8, 380, MIN(h, screen.h - TOPBAR_H - 16)};
}
static struct rect notif_clear_rect(void) {
    struct rect r = notif_panel_rect();
    return (struct rect){r.x + r.w - 74, r.y + 10, 62, 22};
}

static void toggle_notifications(void) {
    notif_open = !notif_open;
    damage_all();
}

static void draw_notifications(void) {
    if (!notif_open) return;
    struct rect r = notif_panel_rect();
    gfx_shadow(&screen, r.x, r.y, r.w, r.h, 12, 14, 120);
    gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 12, 0x0C1226, 246);
    gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 12, 0xFFFFFF, 36);
    gfx_text(&screen, r.x + 16, r.y + 12, "Notifications", 0xFFFFFF, FONT_SMALL);
    struct rect c = notif_clear_rect();
    if (nnotifs) {
        gfx_round_rect(&screen, c.x, c.y, c.w, c.h, 7, 0xFFFFFF, 22);
        gfx_text(&screen, c.x + 12, c.y + 5, "Clear", C_TEXT_DIM, FONT_SMALL);
    }
    if (!nnotifs) {
        gfx_text(&screen, r.x + 16, r.y + 44, "Nothing here yet.", C_TEXT_DIM, FONT_SMALL);
        return;
    }
    int y = r.y + 44;
    for (int k = 0; k < 10 && k < nnotifs; k++) {
        struct notification *n = &notifications[nnotifs - 1 - k];
        if (k) gfx_blend(&screen, r.x + 14, y - 5, r.w - 28, 1, 0xFFFFFF, 22);
        gfx_text(&screen, r.x + 16, y, n->app, C_ACCENT, FONT_SMALL);
        char title[40];
        strlcpy(title, n->title, sizeof title);
        int room = (r.w - 32) / gfx_font_w(FONT_SMALL);
        if (room > 3 && (int)strlen(title) > room) strlcpy(title + room - 3, "...", 4);
        gfx_text(&screen, r.x + 80, y, title, 0xFFFFFF, FONT_SMALL);
        char msg[112];
        strlcpy(msg, n->msg, sizeof msg);
        int mroom = (r.w - 32) / gfx_font_w(FONT_SMALL);
        if (mroom > 3 && (int)strlen(msg) > mroom) strlcpy(msg + mroom - 3, "...", 4);
        gfx_text(&screen, r.x + 16, y + 16, msg, C_TEXT_DIM, FONT_SMALL);
        y += 42;
    }
}

static void draw_bell(void) {
    struct rect b = notif_button_rect();
    gfx_round_rect(&screen, b.x, b.y, b.w, b.h, 7, 0xFFFFFF, notif_open ? 34 : 14);
    int cx = b.x + b.w / 2, cy = b.y + 9;
    gfx_circle(&screen, cx, cy, 5, notif_open ? C_ACCENT : C_TEXT_DIM, 90);
    gfx_circle(&screen, cx, cy + 1, 3, 0x0B1020, 220);
    gfx_blend(&screen, cx - 4, cy + 5, 8, 2, notif_open ? C_ACCENT : C_TEXT_DIM, 220);
    if (nnotifs) gfx_circle(&screen, b.x + b.w - 3, b.y + 3, 4, C_DANGER, 255);
}

static void draw_toasts(void) {
    for (int i = 0; i < ntoasts; i++) {
        struct rect r = toast_rect(i);
        gfx_shadow(&screen, r.x, r.y, r.w, r.h, 12, 10, 100);
        gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 12, 0x1A0F24, 235);
        gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 12, C_DANGER, 120);
        draw_shield(r.x + 16, r.y + 14, C_DANGER);
        gfx_text(&screen, r.x + 40, r.y + 12, toasts[i].title, 0xFFFFFF, FONT_SMALL);
        /* Wrap the message onto at most two lines. */
        char line[48];
        const char *m = toasts[i].msg;
        int maxc = (r.w - 56) / gfx_font_w(FONT_SMALL);
        for (int l = 0; l < 2 && *m; l++) {
            int n = (int)strnlen(m, (size_t)maxc);
            if (m[n] && n == maxc)
                for (int k = n; k > maxc / 2; k--)
                    if (m[k] == ' ') {
                        n = k;
                        break;
                    }
            memcpy(line, m, (size_t)MIN(n, (int)sizeof line - 1));
            line[MIN(n, (int)sizeof line - 1)] = 0;
            gfx_text(&screen, r.x + 40, r.y + 30 + l * 16, line, 0xFFC2D1, FONT_SMALL);
            m += n;
            while (*m == ' ') m++;
        }
    }
}

/* Cursor: 'o' outline, '#' fill. An original arrow design with a notched tail. */
static const char *cursor_art[] = {
    "o           ", "oo          ", "o#o         ", "o##o        ", "o###o       ", "o####o      ",
    "o#####o     ", "o######o    ", "o#######o   ", "o########o  ", "o#####ooooo ", "o##o##o     ",
    "o#o o##o    ", "oo  o##o    ", "o    o##o   ", "     o##o   ", "      oo    ",
};
#define CURSOR_W 12
#define CURSOR_H 17

static void draw_cursor(void) {
    for (int j = 0; j < CURSOR_H; j++)
        for (int i = 0; i < CURSOR_W; i++) {
            char c = cursor_art[j][i];
            if (c == 'o') gfx_blend(&screen, mx + i, my + j, 1, 1, 0x05070F, 230);
            else if (c == '#') gfx_blend(&screen, mx + i, my + j, 1, 1, j < 3 ? 0xFFFFFF : gfx_mix(0xFFFFFF, C_ACCENT, (unsigned)(j * 6)), 255);
        }
}

/* ---- Unlock screen ----------------------------------------------------------------- */

static struct {
    int mode; /* LOCK_* */
    void (*unlocked)(void);
    char pw[65], first[65];
    int len, confirming, busy;
    char error[80];
} lock;

void wm_set_lock(int mode, void (*unlocked)(void)) {
    lock.mode = mode;
    lock.unlocked = unlocked;
    damage_all();
}

static struct rect lock_rect(void) { return (struct rect){(screen.w - 480) / 2, (screen.h - 300) / 2, 480, 300}; }

static void draw_lock(void) {
    gfx_blend(&screen, 0, 0, screen.w, screen.h, 0x03050C, 120);
    struct rect r = lock_rect();
    gfx_shadow(&screen, r.x, r.y, r.w, r.h, 20, 24, 160);
    gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 20, C_GLASS, 235);
    gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 20, C_ACCENT, 90);
    const char *mark = "\xC3\xA6ther";
    gfx_text(&screen, r.x + (r.w - gfx_text_width(mark, FONT_LARGE)) / 2, r.y + 26, mark, C_TEXT, FONT_LARGE);
    const char *line1, *line2;
    if (lock.mode == LOCK_SETUP) {
        line1 = lock.confirming ? "Type the same passphrase again" : "Choose a passphrase for your files";
        line2 = "Everything on this disk will be encrypted with it.";
    } else if (lock.mode == LOCK_SESSION) {
        line1 = "Enter your passphrase to unlock the desktop";
        line2 = "Your files stay encrypted on disk while the screen is locked.";
    } else {
        line1 = "Enter your passphrase";
        line2 = "Your files are encrypted until you unlock them.";
    }
    gfx_text(&screen, r.x + (r.w - gfx_text_width(line1, FONT_SMALL)) / 2, r.y + 90, line1, C_TEXT, FONT_SMALL);
    gfx_text(&screen, r.x + (r.w - gfx_text_width(line2, FONT_SMALL)) / 2, r.y + 110, line2, C_TEXT_DIM, FONT_SMALL);

    /* Passphrase field: dots, never the text. */
    struct rect f = {r.x + 50, r.y + 146, r.w - 100, 44};
    gfx_round_rect(&screen, f.x, f.y, f.w, f.h, 12, 0x070B18, 255);
    gfx_round_outline(&screen, f.x, f.y, f.w, f.h, 12, lock.error[0] ? C_DANGER : C_ACCENT, 150);
    int shown = MIN(lock.len, (f.w - 40) / 14);
    for (int i = 0; i < shown; i++) gfx_circle(&screen, f.x + 22 + i * 14, f.y + f.h / 2, 4, C_TEXT, 255);
    if (!lock.busy) gfx_blend(&screen, f.x + 16 + shown * 14, f.y + 12, 2, f.h - 24, C_ACCENT, 255);

    const char *status = lock.busy ? (lock.mode == LOCK_SETUP ? "Encrypting the disk..." : "Unlocking...")
                         : lock.error[0] ? lock.error : "Press Enter to continue";
    uint32_t sc = lock.error[0] && !lock.busy ? C_DANGER : C_TEXT_DIM;
    gfx_text(&screen, r.x + (r.w - gfx_text_width(status, FONT_SMALL)) / 2, r.y + 206, status, sc, FONT_SMALL);
    char foot[80];
    snprintf(foot, sizeof foot, "%s  -  ChaCha20-Poly1305, PBKDF2-SHA256", storage_device());
    draw_shield(r.x + (r.w - gfx_text_width(foot, FONT_SMALL)) / 2 - 20, r.y + r.h - 40, 0x5CF2B0);
    gfx_text(&screen, r.x + (r.w - gfx_text_width(foot, FONT_SMALL)) / 2, r.y + r.h - 40, foot, 0x9FF5D2, FONT_SMALL);
}

static void lock_key(uint32_t key) {
    if (lock.busy) return;
    if (key == '\b') {
        if (lock.len) lock.pw[--lock.len] = 0;
    } else if (key == 27) {
        crypto_wipe(lock.pw, sizeof lock.pw);
        lock.len = 0;
    } else if (key == '\n') {
        if (lock.mode == LOCK_SETUP && !lock.confirming) {
            if (lock.len < 8) {
                strlcpy(lock.error, "Use at least 8 characters", sizeof lock.error);
                crypto_wipe(lock.pw, sizeof lock.pw);
                lock.len = 0;
            } else {
                memcpy(lock.first, lock.pw, sizeof lock.first);
                crypto_wipe(lock.pw, sizeof lock.pw);
                lock.len = 0;
                lock.confirming = 1;
                lock.error[0] = 0;
            }
        } else if (lock.mode == LOCK_SETUP && strcmp(lock.first, lock.pw)) {
            strlcpy(lock.error, "Those didn't match. Choose a passphrase again.", sizeof lock.error);
            crypto_wipe(lock.pw, sizeof lock.pw);
            crypto_wipe(lock.first, sizeof lock.first);
            lock.len = 0;
            lock.confirming = 0;
        } else {
            lock.busy = 1; /* the key derivation runs on the next loop, after this frame shows */
            lock.error[0] = 0;
        }
    } else if (key >= 32 && key < 127 && lock.len < 64) {
        lock.pw[lock.len++] = (char)key;
        lock.error[0] = 0;
    }
    damage(lock_rect());
}

static void lock_work(void) {
    if (!lock.busy) return;
    int err = lock.mode == LOCK_SESSION ? afs_verify(lock.pw, (size_t)lock.len)
                                        : storage_submit(lock.pw, (size_t)lock.len);
    crypto_wipe(lock.pw, sizeof lock.pw);
    crypto_wipe(lock.first, sizeof lock.first);
    lock.len = 0;
    lock.busy = 0;
    lock.confirming = 0;
    if (err) {
        kprintf("[storage] %s failed (error %d)\n", lock.mode == LOCK_SETUP ? "setup" : "unlock", -err);
        strlcpy(lock.error, err == -E_DENIED ? "Wrong passphrase" : "The disk couldn't be read", sizeof lock.error);
        damage(lock_rect());
        return;
    }
    int was_session = lock.mode == LOCK_SESSION;
    lock.mode = LOCK_NONE;
    damage_all();
    kprintf("[storage] unlocked\n");
    if (lock.unlocked && !was_session) lock.unlocked();
}

/* ---- System dialogs: file picker and label changes ------------------------------------ */

#define DLG_ROWS 9
#define DLG_ROW_H 30
enum { DLG_NONE, DLG_LABEL, DLG_PICK, DLG_NET, DLG_CONFIRM };

static struct {
    int kind;
    struct proc *p;
    struct thread *waiter; /* exactly the thread that asked; only it is woken */
    /* label */
    char file[NET_HOST_MAX + 1]; /* a file name, or a host name for the firewall */
    char title[80], detail[160];
    int from, to, focus; /* focus 0 = keep (the safe default), 1 = lower; network: 0 block, 1 once, 2 always */
    /* network: the host is in `file` */
    int port;
    /* picker */
    int mode;
    uint32_t dir, stack[16];
    int depth;
    char crumbs[16][NAME_MAX + 1];
    struct {
        char name[NAME_MAX + 1];
        uint8_t type, label;
        uint64_t size;
        uint32_t ino;
    } ents[96];
    int nents, sel, scroll;
    char name[NAME_MAX + 1];
    int namelen;
    char note[80];
    int replace_ok;     /* PICK_SAVE: the user confirmed replacing an existing file */
    uint64_t opened_at; /* input is ignored for a moment after a dialog appears */
} dlg;

/* A dialog an app opens can land under keys or a click the user meant for
 * something else (typing Enter in the app's window), so for this long after
 * it appears it ignores input: every answer it gets was given to it. */
#define DLG_INPUT_DELAY_MS 700
static int dlg_input_ready(void) { return ticks - dlg.opened_at >= DLG_INPUT_DELAY_MS; }

static struct rect dlg_rect(void) {
    int w = dlg.kind == DLG_LABEL ? 580 : dlg.kind == DLG_NET ? 660 : dlg.kind == DLG_CONFIRM ? 640 : 600;
    int h = dlg.kind == DLG_LABEL ? 230 : dlg.kind == DLG_NET ? 250 : dlg.kind == DLG_CONFIRM ? 240 : 480;
    return (struct rect){(screen.w - w) / 2, (screen.h - h) / 2, w, h};
}

static void dlg_damage(void) {
    struct rect r = dlg_rect();
    damage((struct rect){r.x - 40, r.y - 40, r.w + 80, r.h + 80});
}

/* Index into a process's per-type refusal counters. */
static int dlg_denial_slot(int kind) {
    return kind == DLG_LABEL ? 0 : kind == DLG_PICK ? 1 : kind == DLG_NET ? 2 : 3;
}

static void dlg_finish(int result, uint32_t ino, const char *name) {
    struct proc *p = dlg.p;
    struct thread *t = dlg.waiter;
    int kind = dlg.kind;
    damage_all(); /* the dimmed backdrop goes away */
    dlg.kind = DLG_NONE;
    dlg.p = 0;
    dlg.waiter = 0;
    if (!p) return;
    if (!result && p->state != PROC_ZOMBIE) p->dialog_denials[dlg_denial_slot(kind)]++;
    if (!t || t->proc != p || t->state == PROC_ZOMBIE) return;
    t->dialog_result = result;
    t->dialog_ino = ino;
    strlcpy(t->dialog_name, name ? name : "", sizeof t->dialog_name);
    t->dialog_done = 1;
    thread_wake(t); /* only the requester, never a sibling thread */
}

int wm_dialog_label(struct proc *p, const char *file, int from, int to) {
    if (dlg.kind || lock.mode) return -E_LIMIT;
    if (p->dialog_denials[0] >= 3) return -E_DENIED; /* stop an app from nagging */
    memset(&dlg, 0, sizeof dlg);
    dlg.kind = DLG_LABEL;
    dlg.p = p;
    dlg.waiter = cur_thread;
    dlg.opened_at = ticks;
    strlcpy(dlg.file, file, sizeof dlg.file);
    dlg.from = from;
    dlg.to = to;
    damage_all();
    return 0;
}

/* The firewall's question (net.c): the answer is 0 block, 1 allow once, 2 always allow. */
int wm_dialog_confirm(struct proc *p, const char *title, const char *detail) {
    if (dlg.kind || lock.mode) return -E_LIMIT;
    if (p->dialog_denials[3] >= 3) return -E_DENIED;
    memset(&dlg, 0, sizeof dlg);
    dlg.kind = DLG_CONFIRM;
    dlg.p = p;
    dlg.waiter = cur_thread;
    dlg.opened_at = ticks;
    strlcpy(dlg.title, title, sizeof dlg.title);
    strlcpy(dlg.detail, detail, sizeof dlg.detail);
    damage_all();
    return 0;
}

int wm_dialog_net(struct proc *p, const char *host, int port) {
    if (dlg.kind || lock.mode) return -E_LIMIT;
    if (p->dialog_denials[2] >= 3) return -E_DENIED;
    memset(&dlg, 0, sizeof dlg);
    dlg.kind = DLG_NET;
    dlg.p = p;
    dlg.waiter = cur_thread;
    dlg.opened_at = ticks;
    strlcpy(dlg.file, host, sizeof dlg.file);
    dlg.port = port;
    damage_all();
    return 0;
}

static void pick_load(void) {
    dlg.nents = 0;
    dlg.sel = 0;
    dlg.scroll = 0;
    dlg.replace_ok = 0; /* a warning in one folder must not authorize another */
    char name[NAME_MAX + 1];
    uint32_t ino;
    uint8_t type;
    /* Folders first, then files. */
    for (int pass = 0; pass < 2; pass++)
        for (uint32_t i = 0; dlg.nents < (int)ARRAY_LEN(dlg.ents) && afs_dir_entry(dlg.dir, i, name, &ino, &type) == 1; i++) {
            if ((pass == 0) != (type == AFS_DIR)) continue;
            struct afs_inode n;
            if (afs_iget(ino, &n)) continue;
            strlcpy(dlg.ents[dlg.nents].name, name, sizeof dlg.ents[0].name);
            dlg.ents[dlg.nents].type = type;
            dlg.ents[dlg.nents].label = n.label;
            dlg.ents[dlg.nents].size = n.size;
            dlg.ents[dlg.nents].ino = ino;
            dlg.nents++;
        }
    if (dlg.mode == PICK_OPEN)
        while (dlg.sel < dlg.nents - 1 && dlg.ents[dlg.sel].type == AFS_DIR) dlg.sel++;
    dlg_damage();
}

int wm_dialog_pick(struct proc *p, int mode, const char *suggested) {
    if (dlg.kind || lock.mode) return -E_LIMIT;
    if (p->dialog_denials[1] >= 3) return -E_DENIED;
    memset(&dlg, 0, sizeof dlg);
    dlg.kind = DLG_PICK;
    dlg.p = p;
    dlg.waiter = cur_thread;
    dlg.opened_at = ticks;
    dlg.mode = mode;
    dlg.dir = vfs_home();
    strlcpy(dlg.crumbs[0], "Home", sizeof dlg.crumbs[0]);
    if (mode == PICK_SAVE) {
        strlcpy(dlg.name, suggested[0] ? suggested : "Untitled.txt", sizeof dlg.name);
        dlg.namelen = (int)strlen(dlg.name);
    }
    pick_load();
    damage_all();
    return 0;
}

static void draw_chip(int x, int y, const char *text, uint32_t color) {
    int w = gfx_text_width(text, FONT_SMALL) + 16;
    gfx_round_rect(&screen, x, y, w, 20, 10, color, 40);
    gfx_round_outline(&screen, x, y, w, 20, 10, color, 160);
    gfx_text(&screen, x + 8, y + 2, text, color, FONT_SMALL);
}

static int chip_width(const char *text) { return gfx_text_width(text, FONT_SMALL) + 16; }

static struct rect dlg_button(int k) { /* 0 = left/cancel, 1 = right/confirm; the network dialog has 3 */
    struct rect r = dlg_rect();
    if (dlg.kind == DLG_NET) {
        int bw = 180;
        return (struct rect){r.x + r.w - 24 - (3 - k) * bw - (2 - k) * 12, r.y + r.h - 58, bw, 38};
    }
    int bw = dlg.kind == DLG_LABEL ? 210 : 140;
    return (struct rect){k ? r.x + r.w - 24 - bw : r.x + r.w - 24 - 2 * bw - 12, r.y + r.h - 58, bw, 38};
}

static void draw_button(struct rect b, const char *text, uint32_t color, int primary, int focus) {
    gfx_round_rect(&screen, b.x, b.y, b.w, b.h, 12, primary ? color : 0x1A2344, primary ? 230 : 255);
    gfx_round_outline(&screen, b.x, b.y, b.w, b.h, 12, focus ? 0xFFFFFF : color, focus ? 220 : 120);
    gfx_text(&screen, b.x + (b.w - gfx_text_width(text, FONT_SMALL)) / 2, b.y + 11, text, primary ? 0x060A18 : C_TEXT,
             FONT_SMALL);
}

static struct rect pick_row(int i) {
    struct rect r = dlg_rect();
    return (struct rect){r.x + 20, r.y + 116 + i * DLG_ROW_H, r.w - 40, DLG_ROW_H};
}

static void draw_dialog(void) {
    if (!dlg.kind) return;
    gfx_blend(&screen, 0, 0, screen.w, screen.h, 0x03050C, 110);
    struct rect r = dlg_rect();
    gfx_shadow(&screen, r.x, r.y, r.w, r.h, 18, 24, 170);
    gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 18, 0x0C1226, 250);
    gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 18, C_ACCENT, 110);
    draw_shield(r.x + 22, r.y + 22, 0x5CF2B0);
    gfx_text(&screen, r.x + 44, r.y + 22, "\xC3\xA6ther system dialog", 0x9FF5D2, FONT_SMALL);
    char line[120];

    if (dlg.kind == DLG_LABEL) {
        snprintf(line, sizeof line, "Lower the label of \"%s\"?", dlg.file);
        gfx_text(&screen, r.x + 24, r.y + 56, line, C_TEXT, FONT_MEDIUM);
        snprintf(line, sizeof line, "%s wants to change it from %s to %s.", dlg.p ? dlg.p->name : "An app",
                 dlp_label_name(dlg.from), dlp_label_name(dlg.to));
        gfx_text(&screen, r.x + 24, r.y + 96, line, C_TEXT, FONT_SMALL);
        gfx_text(&screen, r.x + 24, r.y + 118, "Data with a lower label gets fewer checks when it leaves.",
                 C_TEXT_DIM, FONT_SMALL);
        snprintf(line, sizeof line, "Keep %s", dlp_label_name(dlg.from));
        draw_button(dlg_button(0), line, C_ACCENT, 1, dlg.focus == 0);
        snprintf(line, sizeof line, "Lower to %s", dlp_label_name(dlg.to));
        draw_button(dlg_button(1), line, C_DANGER, 0, dlg.focus == 1);
        return;
    }

    if (dlg.kind == DLG_CONFIRM) {
        gfx_text(&screen, r.x + 24, r.y + 56, dlg.title, C_TEXT, FONT_MEDIUM);
        gfx_text(&screen, r.x + 24, r.y + 96, dlg.detail, 0xFFB547, FONT_SMALL);
        gfx_text(&screen, r.x + 24, r.y + 118, "This action is performed by the kernel, not by the app.",
                 C_TEXT_DIM, FONT_SMALL);
        draw_button(dlg_button(0), "Cancel", C_ACCENT, 0, dlg.focus == 0);
        draw_button(dlg_button(1), "Confirm", C_DANGER, 1, dlg.focus == 1);
        return;
    }

    if (dlg.kind == DLG_NET) {
        const char *app = dlg.p ? dlg.p->name : "An app";
        snprintf(line, sizeof line, "Let %s connect to %s?", app, dlg.file);
        gfx_text(&screen, r.x + 24, r.y + 56, line, C_TEXT, FONT_MEDIUM);
        const char *what = dlg.port == 443 ? " (secure web)" : dlg.port == 80 ? " (web, not encrypted)" : "";
        snprintf(line, sizeof line, "%s wants to connect on port %d%s.", app, dlg.port, what);
        gfx_text(&screen, r.x + 24, r.y + 96, line, C_TEXT, FONT_SMALL);
        gfx_text(&screen, r.x + 24, r.y + 118, "The firewall asks once per app and site. Nothing outside can connect in.",
                 C_TEXT_DIM, FONT_SMALL);
        draw_button(dlg_button(0), "Block", C_ACCENT, 1, dlg.focus == 0);
        draw_button(dlg_button(1), "Allow once", C_ACCENT, 0, dlg.focus == 1);
        draw_button(dlg_button(2), "Always allow", 0xFFB547, 0, dlg.focus == 2);
        return;
    }

    gfx_text(&screen, r.x + 24, r.y + 48, dlg.mode == PICK_SAVE ? "Save a file" : "Open a file", C_TEXT, FONT_MEDIUM);
    snprintf(line, sizeof line, "for %s. Only the file you choose is shared with it.", dlg.p ? dlg.p->name : "an app");
    gfx_text(&screen, r.x + 24, r.y + 76, line, 0xFFB547, FONT_SMALL);
    /* Breadcrumbs, with an up arrow when inside a folder. */
    int x = r.x + 24;
    if (dlg.depth) x = gfx_text(&screen, x, r.y + 96, "<  ", C_ACCENT, FONT_SMALL);
    for (int i = 0; i <= dlg.depth; i++) {
        x = gfx_text(&screen, x, r.y + 96, dlg.crumbs[i], i == dlg.depth ? C_TEXT : C_TEXT_DIM, FONT_SMALL);
        if (i < dlg.depth) x = gfx_text(&screen, x, r.y + 96, " / ", C_TEXT_DIM, FONT_SMALL);
    }
    struct rect list = {r.x + 20, r.y + 116, r.w - 40, DLG_ROWS * DLG_ROW_H};
    gfx_round_rect(&screen, list.x, list.y, list.w, list.h, 10, 0x070B18, 255);
    if (!dlg.nents) gfx_text(&screen, list.x + 16, list.y + 12, "This folder is empty", C_TEXT_DIM, FONT_SMALL);
    for (int i = 0; i < DLG_ROWS && dlg.scroll + i < dlg.nents; i++) {
        int k = dlg.scroll + i;
        struct rect row = pick_row(i);
        if (k == dlg.sel) gfx_round_rect(&screen, row.x + 2, row.y + 2, row.w - 4, row.h - 4, 8, C_ACCENT, 50);
        int dir = dlg.ents[k].type == AFS_DIR;
        if (dir) { /* folder glyph */
            gfx_round_rect(&screen, row.x + 12, row.y + 9, 18, 13, 3, 0x5C7CFA, 255);
            gfx_round_rect(&screen, row.x + 12, row.y + 7, 8, 4, 2, 0x5C7CFA, 255);
        } else { /* page glyph */
            gfx_round_rect(&screen, row.x + 14, row.y + 6, 13, 18, 2, 0xC8D2F0, 255);
            gfx_blend(&screen, row.x + 17, row.y + 12, 7, 1, 0x667099, 255);
            gfx_blend(&screen, row.x + 17, row.y + 16, 7, 1, 0x667099, 255);
        }
        gfx_text(&screen, row.x + 40, row.y + 7, dlg.ents[k].name, C_TEXT, FONT_SMALL);
        const char *ln = dlp_label_name(dlg.ents[k].label);
        draw_chip(row.x + row.w - 16 - chip_width(ln), row.y + 5, ln, dlp_label_color(dlg.ents[k].label));
        if (!dir) {
            char sz[24];
            uint64_t b = dlg.ents[k].size;
            if (b < 10240) snprintf(sz, sizeof sz, "%lu B", b);
            else snprintf(sz, sizeof sz, "%lu KB", b / 1024);
            gfx_text(&screen, row.x + row.w - 40 - chip_width(ln) - gfx_text_width(sz, FONT_SMALL), row.y + 7, sz, C_TEXT_DIM,
                     FONT_SMALL);
        }
    }
    if (dlg.mode == PICK_SAVE) {
        struct rect f = {r.x + 20, r.y + r.h - 106, r.w - 40, 36};
        gfx_text(&screen, f.x, f.y - 2, "Name:", C_TEXT_DIM, FONT_SMALL);
        gfx_round_rect(&screen, f.x + 56, f.y - 8, f.w - 56, f.h, 10, 0x070B18, 255);
        gfx_round_outline(&screen, f.x + 56, f.y - 8, f.w - 56, f.h, 10, C_ACCENT, 140);
        int ex = gfx_text(&screen, f.x + 68, f.y, dlg.name, C_TEXT, FONT_SMALL);
        gfx_blend(&screen, ex + 1, f.y - 1, 2, 18, C_ACCENT, 255);
    }
    if (dlg.note[0]) gfx_text(&screen, r.x + 24, r.y + r.h - 48, dlg.note, C_DANGER, FONT_SMALL);
    draw_button(dlg_button(0), "Cancel", C_ACCENT, 0, 0);
    draw_button(dlg_button(1), dlg.mode == PICK_SAVE ? "Save" : "Open", C_ACCENT, 1, 0);
}

static void pick_enter(int k) {
    if (dlg.depth + 1 >= (int)ARRAY_LEN(dlg.stack)) return;
    dlg.stack[dlg.depth] = dlg.dir;
    dlg.depth++;
    strlcpy(dlg.crumbs[dlg.depth], dlg.ents[k].name, sizeof dlg.crumbs[0]);
    dlg.dir = dlg.ents[k].ino;
    pick_load();
}

static void pick_up(void) {
    if (!dlg.depth) return;
    dlg.dir = dlg.stack[--dlg.depth];
    pick_load();
}

static void pick_confirm(void) {
    if (dlg.mode == PICK_SAVE) {
        if (!dlg.namelen) return;
        /* Ask the file system, not the visible (and possibly truncated)
         * listing: a name beyond the first 96 entries must still warn. */
        uint32_t existing;
        if (!afs_lookup(dlg.dir, dlg.name, &existing)) {
            struct afs_inode n;
            if (!afs_iget(existing, &n) && n.mode == AFS_DIR) {
                strlcpy(dlg.note, "That's a folder; choose another name.", sizeof dlg.note);
                dlg_damage();
                return;
            }
            /* Saving replaces the file, so it always takes a second, deliberate Save. */
            if (!dlg.replace_ok) {
                strlcpy(dlg.note, "That file exists. Save again to replace it.", sizeof dlg.note);
                dlg.replace_ok = 1;
                dlg_damage();
                return;
            }
        }
        dlg_finish(1, dlg.dir, dlg.name);
        return;
    }
    if (dlg.sel >= dlg.nents) return;
    if (dlg.ents[dlg.sel].type == AFS_DIR) {
        pick_enter(dlg.sel);
        return;
    }
    dlg_finish(1, dlg.ents[dlg.sel].ino, dlg.ents[dlg.sel].name);
}

static void dlg_key(uint32_t key, uint32_t mods) {
    dlg_damage();
    if (!dlg_input_ready()) return;
    if (mods & (MOD_CTRL | MOD_ALT)) return;
    if (key == 27) {
        dlg_finish(0, 0, 0);
        return;
    }
    if (dlg.kind == DLG_NET) {
        if (key == '\t' || key == KEY_RIGHT) dlg.focus = (dlg.focus + 1) % 3;
        else if (key == KEY_LEFT) dlg.focus = (dlg.focus + 2) % 3;
        else if (key == '\n') dlg_finish(dlg.focus, 0, 0);
        return;
    }
    if (dlg.kind == DLG_LABEL || dlg.kind == DLG_CONFIRM) {
        if (key == '\t' || key == KEY_LEFT || key == KEY_RIGHT) dlg.focus ^= 1;
        else if (key == '\n') dlg_finish(dlg.focus, 0, 0);
        return;
    }
    if (key == KEY_UP && dlg.sel > 0) dlg.sel--;
    else if (key == KEY_DOWN && dlg.sel < dlg.nents - 1) dlg.sel++;
    else if (key == '\n') pick_confirm();
    else if (dlg.mode == PICK_SAVE && key == '\b') {
        if (dlg.namelen) dlg.name[--dlg.namelen] = 0;
        dlg.note[0] = 0;
        dlg.replace_ok = 0;
    } else if (dlg.mode == PICK_OPEN && key == '\b') pick_up();
    else if (dlg.mode == PICK_SAVE && key >= 32 && key < 127 && key != '/' && dlg.namelen < NAME_MAX) {
        dlg.name[dlg.namelen++] = (char)key;
        dlg.name[dlg.namelen] = 0;
        dlg.note[0] = 0;
        dlg.replace_ok = 0;
    }
    if (dlg.sel < dlg.scroll) dlg.scroll = dlg.sel;
    if (dlg.sel >= dlg.scroll + DLG_ROWS) dlg.scroll = dlg.sel - DLG_ROWS + 1;
}

static void dlg_click(int x, int y) {
    dlg_damage();
    if (!dlg_input_ready()) return;
    if (dlg.kind == DLG_NET) {
        for (int k = 0; k < 3; k++)
            if (rect_contains(dlg_button(k), x, y)) {
                dlg_finish(k, 0, 0);
                return;
            }
        return;
    }
    if (rect_contains(dlg_button(0), x, y)) {
        dlg_finish(0, 0, 0);
        return;
    }
    if (rect_contains(dlg_button(1), x, y)) {
        if (dlg.kind == DLG_LABEL || dlg.kind == DLG_CONFIRM) dlg_finish(1, 0, 0);
        else pick_confirm();
        return;
    }
    if (dlg.kind != DLG_PICK) return;
    struct rect r = dlg_rect();
    if (dlg.depth && rect_contains((struct rect){r.x + 20, r.y + 92, 40, 22}, x, y)) {
        pick_up();
        return;
    }
    for (int i = 0; i < DLG_ROWS && dlg.scroll + i < dlg.nents; i++)
        if (rect_contains(pick_row(i), x, y)) {
            int k = dlg.scroll + i;
            if (dlg.ents[k].type == AFS_DIR) {
                pick_enter(k);
            } else if (dlg.sel == k && dlg.mode == PICK_OPEN) {
                pick_confirm(); /* second click on the same file opens it */
            } else {
                dlg.sel = k;
                if (dlg.mode == PICK_SAVE) {
                    strlcpy(dlg.name, dlg.ents[k].name, sizeof dlg.name);
                    dlg.replace_ok = 0;
                    dlg.namelen = (int)strlen(dlg.name);
                }
            }
            return;
        }
}

static void dlg_cancel_for(struct proc *p) {
    if (dlg.kind && dlg.p == p) {
        /* The owner is exiting: publish no result, just close the dialog. */
        dlg.p = 0;
        dlg.waiter = 0;
        dlg.kind = DLG_NONE;
        damage_all();
    }
}

void wm_proc_changed(struct proc *p) {
    for (int i = 0; i < MAX_WINDOWS; i++)
        if (windows[i].used && windows[i].owner == p) damage(win_damage_rect(&windows[i]));
}

/* ---- Window switcher ---------------------------------------------------------------- */

static int switch_tile(void) { return sw.n ? MIN(SWITCH_TILE, (screen.w - 80) / sw.n) : SWITCH_TILE; }

static struct rect switch_rect(void) {
    int t = switch_tile(), w = sw.n * t + 32, h = t + 64;
    return (struct rect){(screen.w - w) / 2, (screen.h - h) / 2, w, h};
}

static void damage_switch(void) {
    struct rect r = switch_rect();
    damage((struct rect){r.x - 30, r.y - 30, r.w + 60, r.h + 60});
}

static void switch_open(int backward) {
    if (!sw.open) {
        sw.n = 0;
        for (int pass = 0; pass < 2; pass++) /* on-screen windows front to back, then minimized ones */
            for (int i = nz - 1; i >= 0; i--)
                if (windows[zorder[i]].minimized == pass) sw.ids[sw.n++] = windows[zorder[i]].id;
        if (!sw.n) return;
        sw.open = 1;
        sw.sel = 0;
        if (menu_open) close_menu();
    }
    damage_switch();
    sw.sel = (sw.sel + (backward ? sw.n - 1 : 1)) % sw.n;
    damage_switch();
}

static void switch_close(int commit) {
    if (!sw.open) return;
    damage_switch();
    sw.open = 0;
    if (!commit) return;
    int wi = window_by_id(sw.ids[sw.sel]); /* it may have closed meanwhile */
    if (wi >= 0) restore_window(wi);
}

/* Lock the desktop without unmounting anything. Apps stay alive but receive
 * no input until the passphrase that unlocks the volume is entered again. */
static void wm_lock_now(void) {
    if (lock.mode || afs_temporary()) return;
    if (dlg.kind) dlg_finish(0, 0, 0); /* the app gets "denied", not a forever wait */
    close_menu();
    switch_close(0);
    lock.mode = LOCK_SESSION;
    lock.unlocked = 0; /* the boot-unlock callback must not run again */
    lock.len = 0;
    lock.error[0] = 0;
    lock.confirming = 0;
    crypto_wipe(lock.pw, sizeof lock.pw);
    crypto_wipe(lock.first, sizeof lock.first);
    last_input = ticks;
    damage_all();
    kprintf("[wm] desktop locked\n");
}

/* Ctrl+Alt+Esc: close the app behind a system dialog, or the focused app.
 * This is the way out of a picker an app keeps reopening. */
static void wm_force_close(void) {
    if (dlg.kind) {
        struct proc *p = dlg.p;
        dlg.p = 0;
        dlg.waiter = 0;
        dlg.kind = DLG_NONE;
        damage_all();
        if (p) proc_kill(p, EXIT_KILLED_BY_WM, 0);
    } else if (focused >= 0) {
        close_window(focused);
    }
}

int wm_lock_session(void) {
    if (lock.mode) return -E_BUSY;
    if (afs_temporary()) return -E_NOTSUP;
    wm_lock_now();
    return 0;
}

static int switch_at(int x, int y) {
    struct rect r = switch_rect();
    int t = switch_tile();
    for (int k = 0; k < sw.n; k++)
        if (rect_contains((struct rect){r.x + 16 + k * t, r.y + 12, t, t}, x, y)) return k;
    return -1;
}

static void draw_switcher(void) {
    if (!sw.open) return;
    struct rect r = switch_rect();
    int t = switch_tile();
    gfx_shadow(&screen, r.x, r.y, r.w, r.h, 18, 20, 150);
    gfx_round_rect(&screen, r.x, r.y, r.w, r.h, 18, 0x0C1226, 236);
    gfx_round_outline(&screen, r.x, r.y, r.w, r.h, 18, 0xFFFFFF, 40);
    for (int k = 0; k < sw.n; k++) {
        int wi = window_by_id(sw.ids[k]);
        struct rect tile = {r.x + 16 + k * t, r.y + 12, t, t};
        if (k == sw.sel) gfx_round_rect(&screen, tile.x + 2, tile.y, t - 4, t, 14, 0xFFFFFF, 40);
        if (wi < 0) continue;
        struct window *w = &windows[wi];
        struct rect icon = {tile.x + (t - DOCK_ICON) / 2, tile.y + (t - DOCK_ICON) / 2, DOCK_ICON, DOCK_ICON};
        const struct program *p = win_program(w);
        if (p) draw_icon(p, icon, k == sw.sel);
        if (w->minimized) gfx_blend(&screen, icon.x + DOCK_ICON / 2 - 8, icon.y + DOCK_ICON + 4, 16, 2, C_TEXT_DIM, 200);
    }
    int wi = window_by_id(sw.ids[sw.sel]);
    if (wi >= 0) {
        char label[WIN_TITLE_MAX + 40];
        struct window *w = &windows[wi];
        snprintf(label, sizeof label, "%s%s%s", win_app_name(w), w->title[0] && strcmp(w->title, win_app_name(w)) ? " - " : "",
                 w->title[0] && strcmp(w->title, win_app_name(w)) ? w->title : "");
        int maxc = (r.w - 24) / gfx_font_w(FONT_SMALL);
        if ((int)strlen(label) > maxc && maxc > 3) strlcpy(label + maxc - 3, "...", 4);
        gfx_text(&screen, r.x + (r.w - gfx_text_width(label, FONT_SMALL)) / 2, r.y + r.h - 30, label, C_TEXT, FONT_SMALL);
    }
}

/* The outline shown while a window is resized by an edge. */
static void draw_resize_outline(void) {
    if (rs_win < 0) return;
    struct rect r = rs_cur;
    gfx_round_rect(&screen, r.x, r.y, r.w, r.h, WIN_RADIUS, C_ACCENT, 26);
    gfx_round_outline(&screen, r.x, r.y, r.w, r.h, WIN_RADIUS, C_ACCENT, 200);
}

static void compose(void) {
    if (rect_empty(dirty)) return;
    gfx_set_clip(&screen, dirty);
    struct rect c = screen.clip;
    for (int j = 0; j < c.h; j++)
        memcpy(&screen.px[(long)(c.y + j) * screen.pitch + c.x], &wallpaper[(long)(c.y + j) * screen.w + c.x],
               (size_t)c.w * 4);
    if (lock.mode) {
        draw_lock();
    } else {
        for (int i = 0; i < nz; i++)
            if (!windows[zorder[i]].minimized && zorder[i] != anim.wi) draw_window(zorder[i]);
        draw_resize_outline();
        draw_topbar();
        draw_dock();
        draw_anim();
        draw_toasts();
        draw_notifications();
        draw_menu();
        draw_switcher();
        draw_dialog();
    }
    draw_cursor();

    /* Copy the dirty rectangle to the real framebuffer. */
    uint32_t *out = P2V(fb.base);
    int swap = fb.format == FB_FORMAT_RGBX;
    for (int j = 0; j < c.h; j++) {
        uint32_t *src = &screen.px[(long)(c.y + j) * screen.pitch + c.x];
        uint32_t *dst = &out[(long)(c.y + j) * fb.pitch + c.x];
        if (!swap) {
            memcpy(dst, src, (size_t)c.w * 4);
        } else {
            for (int i = 0; i < c.w; i++) {
                uint32_t p = src[i];
                dst[i] = ((p & 0xFF) << 16) | (p & 0xFF00) | ((p >> 16) & 0xFF);
            }
        }
    }
    dirty = (struct rect){0, 0, 0, 0};
}

/* ---- Input ------------------------------------------------------------------------- */

static int window_at(int x, int y) {
    for (int i = nz - 1; i >= 0; i--)
        if (!windows[zorder[i]].minimized && rect_contains(win_rect(&windows[zorder[i]]), x, y)) return zorder[i];
    return -1;
}

/* Which edges of a resizable window the point grabs (EDGE_*), looking from
 * the top window down; a window under the point that isn't resizable, or
 * the inside of any window, stops the search. */
static int edge_at(int x, int y, int *wi_out) {
    for (int i = nz - 1; i >= 0; i--) {
        struct window *w = &windows[zorder[i]];
        if (w->minimized) continue;
        struct rect r = win_rect(w);
        if (rect_contains(r, x, y)) return 0;
        if (!(w->flags & WIN_FLAG_RESIZABLE)) continue;
        if (!rect_contains((struct rect){r.x - EDGE, r.y - EDGE, r.w + 2 * EDGE, r.h + 2 * EDGE}, x, y)) continue;
        int e = (x < r.x ? EDGE_L : 0) | (x >= r.x + r.w ? EDGE_R : 0) | (y < r.y ? EDGE_T : 0) | (y >= r.y + r.h ? EDGE_B : 0);
        /* Near a corner, grab both edges. */
        if ((e & (EDGE_L | EDGE_R)) && !(e & (EDGE_T | EDGE_B))) e |= y < r.y + 16 ? EDGE_T : y >= r.y + r.h - 16 ? EDGE_B : 0;
        if ((e & (EDGE_T | EDGE_B)) && !(e & (EDGE_L | EDGE_R))) e |= x < r.x + 16 ? EDGE_L : x >= r.x + r.w - 16 ? EDGE_R : 0;
        *wi_out = zorder[i];
        return e;
    }
    return 0;
}

static int dock_at(int x, int y) {
    int n = dock_items();
    for (int k = 0; k < n; k++)
        if (rect_contains(dock_item_rect(k), x, y)) return k;
    return -1;
}

static void launch(const struct program *p) {
    if (!p) return;
    int pid = proc_spawn(p->name, p->caps, 0, 0);
    if (pid < 0) wm_notify(p->title, "could not be started (out of memory or too many apps)");
}

/* A dock icon: bring the app's frontmost window forward (from the dock if it
 * is minimized), or start the app when it has no window. */
static void activate_program(const struct program *p) {
    int best = -1;
    for (int i = nz - 1; i >= 0 && best < 0; i--)
        if (!windows[zorder[i]].minimized && !strcmp(windows[zorder[i]].owner->name, p->name)) best = zorder[i];
    if (best < 0) /* all minimized: the most recently minimized */
        for (int i = 0; i < MAX_WINDOWS; i++)
            if (windows[i].used && !strcmp(windows[i].owner->name, p->name) &&
                (best < 0 || windows[i].min_seq > windows[best].min_seq))
                best = i;
    if (best >= 0) restore_window(best);
    else launch(p);
}

static void menu_act(const struct menu_item *it) {
    int f = focused;
    switch (it->act) {
    case ACT_LAUNCH: launch(program_find(launch_names[it->arg])); break;
    case ACT_NEW: if (f >= 0) launch(win_program(&windows[f])); break;
    case ACT_QUIT:
    case ACT_CLOSE: if (f >= 0) close_window(f); break;
    case ACT_MINIMIZE: if (f >= 0) minimize_window(f); break;
    case ACT_ZOOM: if (f >= 0) zoom_window(f); break;
    case ACT_HIDE_ALL:
        for (int i = nz - 1; i >= 0; i--)
            if (!windows[zorder[i]].minimized) minimize_window(zorder[i]);
        break;
    case ACT_SHOW_ALL:
        for (int i = 0; i < MAX_WINDOWS; i++)
            if (windows[i].used && windows[i].minimized) restore_window(i);
        break;
    case ACT_FOCUS: {
        int wi = window_by_id(it->arg);
        if (wi >= 0) restore_window(wi);
        break;
    }
    case ACT_LOCK: wm_lock_now(); break;
    case ACT_RESTART: system_reboot(); break;
    case ACT_SHUTDOWN: system_shutdown(); break;
    }
}

/* Super+<key> belongs to the window manager and never reaches an app. */
static void super_key(uint32_t key, uint32_t mods) {
    int f = focused;
    if (key == '\t') switch_open(mods & MOD_SHIFT);
    else if (key == 'l' || key == 'L') wm_lock_now();
    else if (f < 0) return;
    else if (key == 'm' || key == 'M' || key == KEY_DOWN) minimize_window(f);
    else if (key == KEY_UP) zoom_window(f);
    else if (key == 'w' || key == 'W' || key == 'q' || key == 'Q') close_window(f);
}

static void handle_key(const struct input_event *e) {
    uint64_t t[2] = {e->key, rdtsc()};
    rng_mix(t, sizeof t); /* keystroke timing is good entropy */
    last_input = ticks;
    if (lock.mode) {
        lock_key(e->key);
        return;
    }
    /* These belong to the desktop even while a dialog is modal. */
    if ((e->mods & MOD_CTRL) && (e->mods & MOD_ALT) && e->key == 27) {
        wm_force_close();
        return;
    }
    if ((e->mods & MOD_SUPER) && (e->key == 'l' || e->key == 'L')) {
        wm_lock_now();
        return;
    }
    if (dlg.kind) {
        dlg_key(e->key, e->mods);
        return;
    }
    if (sw.open) {
        if (e->key == '\t' || e->key == KEY_RIGHT) switch_open((e->mods & MOD_SHIFT) && e->key == '\t');
        else if (e->key == KEY_LEFT) switch_open(1);
        else if (e->key == 27) switch_close(0);
        else if (e->key == '\n') switch_close(1);
        return; /* nothing reaches an app while the switcher is up */
    }
    if (menu_open) {
        if (e->key == 27) close_menu();
        return;
    }
    if ((e->mods & MOD_CTRL) && (e->mods & MOD_ALT) && (e->key == 't' || e->key == 'T')) {
        launch(program_find("terminal"));
        return;
    }
    if ((e->mods & MOD_SUPER) && (e->key == 'n' || e->key == 'N')) {
        toggle_notifications();
        return;
    }
    if (e->mods & MOD_SUPER) {
        super_key(e->key, e->mods);
        return;
    }
    if ((e->mods & MOD_ALT) && e->key == '\t') {
        switch_open(e->mods & MOD_SHIFT);
        return;
    }
    if (focused >= 0) send(focused, (struct event){.type = EV_KEY, .key = e->key, .mods = e->mods & ~MOD_SUPER});
}

/* Letting go of Alt / Super picks the window the switcher shows. */
static void handle_mods(const struct input_event *e) {
    if (sw.open && !(e->mods & (MOD_ALT | MOD_SUPER))) switch_close(1);
}

static void client_event(int wi, uint32_t type) {
    struct window *w = &windows[wi];
    send(wi, (struct event){.type = type, .x = mx - w->x, .y = my - w->y - TITLE_H, .buttons = (uint32_t)mbuttons});
}

/* The menu bar and an open menu. Returns 1 if it used the event. */
static int menu_mouse(int pressed) {
    if (pressed && rect_contains(notif_button_rect(), mx, my)) {
        if (menu_open) close_menu();
        toggle_notifications();
        return 1;
    }
    if (notif_open) {
        struct rect p = notif_panel_rect();
        if (pressed) {
            if (rect_contains(notif_clear_rect(), mx, my)) wm_notifications_clear();
            else if (!rect_contains(p, mx, my)) toggle_notifications();
        }
        return 1; /* the history panel is modal for the pointer */
    }
    int title = -1;
    if (my < TOPBAR_H)
        for (int m = MENU_SYSTEM; m < MENU_COUNT; m++)
            if (rect_contains(menu_title_rect(m), mx, my)) title = m;
    if (!menu_open) {
        if (pressed && title > 0) {
            open_menu(title);
            return 1;
        }
        return 0;
    }
    if (title > 0 && title != menu_open) { /* sliding along the bar switches menus */
        open_menu(title);
        return 1;
    }
    struct menu_item it[MENU_MAX];
    int n = build_menu(menu_open, it);
    int h = menu_item_at(menu_open, it, n, mx, my);
    if (h != menu_hover) {
        menu_hover = h;
        damage_menu();
    }
    if (pressed) {
        if (title == menu_open) close_menu();
        else if (h >= 0 && it[h].enabled) {
            close_menu();
            menu_act(&it[h]);
        } else if (h < 0) {
            close_menu(); /* a click outside only closes the menu */
        }
    }
    return 1;
}

static struct rect resized_rect(int dx, int dy) {
    struct rect r = rs_start;
    int minw = windows[rs_win].min_w, minh = windows[rs_win].min_h + TITLE_H;
    int maxw = max_client_w(), maxh = max_client_h() + TITLE_H;
    if (rs_edges & EDGE_R) r.w = MAX(minw, MIN(maxw, r.w + dx));
    if (rs_edges & EDGE_B) r.h = MAX(minh, MIN(maxh, r.h + dy));
    if (rs_edges & EDGE_L) {
        int w = MAX(minw, MIN(maxw, r.w - dx));
        r.x += r.w - w;
        r.w = w;
    }
    if (rs_edges & EDGE_T) {
        int h = MAX(minh, MIN(maxh, r.h - dy));
        if (r.y + r.h - h < TOPBAR_H) h = r.y + r.h - TOPBAR_H;
        r.y += r.h - h;
        r.h = h;
    }
    return r;
}

static void handle_mouse(const struct input_event *e) {
    last_input = ticks;
    struct rect old_cursor = {mx, my, CURSOR_W, CURSOR_H};
    mx = MAX(0, MIN(screen.w - 1, mx + e->dx));
    my = MAX(0, MIN(screen.h - 1, my + e->dy));
    int pressed = (e->buttons & 1) && !(mbuttons & 1);
    int released = !(e->buttons & 1) && (mbuttons & 1);
    int prev_buttons = mbuttons;
    mbuttons = e->buttons;
    damage(old_cursor);
    damage((struct rect){mx, my, CURSOR_W, CURSOR_H});
    if (lock.mode) return;
    if (dlg.kind) { /* a system dialog is modal: apps get no input meanwhile */
        if (pressed) dlg_click(mx, my);
        return;
    }
    if (sw.open) {
        int k = switch_at(mx, my);
        if (pressed && k >= 0) {
            sw.sel = k;
            switch_close(1);
        } else if (pressed) {
            switch_close(0);
        }
        return;
    }
    if (menu_mouse(pressed)) return;

    if (drag_win >= 0) {
        struct window *w = &windows[drag_win];
        damage(win_damage_rect(w));
        w->x = MAX(-w->cw + 80, MIN(screen.w - 80, mx - drag_dx));
        w->y = MAX(TOPBAR_H, MIN(screen.h - TITLE_H, my - drag_dy));
        damage(win_damage_rect(w));
        if (released) drag_win = -1;
        return;
    }
    if (rs_win >= 0) {
        struct rect r = resized_rect(mx - rs_mx, my - rs_my);
        damage((struct rect){rs_cur.x - 2, rs_cur.y - 2, rs_cur.w + 4, rs_cur.h + 4});
        rs_cur = r;
        damage((struct rect){r.x - 2, r.y - 2, r.w + 4, r.h + 4});
        if (released) {
            int wi = rs_win;
            rs_win = -1;
            damage((struct rect){r.x - 2, r.y - 2, r.w + 4, r.h + 4});
            if (r.w != rs_start.w || r.h != rs_start.h) propose(wi, (struct rect){r.x, r.y, r.w, r.h - TITLE_H}, 0);
        }
        return;
    }

    int wi = window_at(mx, my);
    int dk = (wi < 0) ? dock_at(mx, my) : -1;
    int hl = (wi >= 0 && rect_contains(lights_rect(&windows[wi]), mx, my)) ? wi : -1;
    if (hl != hover_lights) {
        if (hover_lights >= 0) damage(win_damage_rect(&windows[hover_lights]));
        if (hl >= 0) damage(win_damage_rect(&windows[hl]));
        hover_lights = hl;
    }
    if (dk != hover_dock) {
        hover_dock = dk;
        damage_dock();
    }

    if (pressed) {
        if (dk >= 0) {
            if (dk < dock_count()) activate_program(dock_program(dk));
            else if (dock_window(dk) >= 0) restore_window(dock_window(dk));
            return;
        }
        int ew = -1, edges = wi < 0 ? edge_at(mx, my, &ew) : 0;
        if (edges) {
            focus_window(ew);
            struct window *w = &windows[ew];
            rs_win = ew;
            rs_edges = edges;
            rs_mx = mx;
            rs_my = my;
            rs_start = rs_cur = win_rect(w);
            return;
        }
        if (wi < 0) return;
        focus_window(wi);
        struct window *w = &windows[wi];
        int light = light_at(w, mx, my);
        if (light == 0) {
            close_window(wi);
            return;
        }
        if (light == 1) {
            minimize_window(wi);
            return;
        }
        if (light == 2) {
            zoom_window(wi);
            return;
        }
        if (my < w->y + TITLE_H) {
            if (click_win == wi && ticks - click_at < DOUBLE_CLICK_MS) { /* double-click the title bar: zoom */
                click_win = -1;
                zoom_window(wi);
                return;
            }
            click_win = wi;
            click_at = ticks;
            drag_win = wi;
            drag_dx = mx - w->x;
            drag_dy = my - w->y;
            return;
        }
        client_event(wi, EV_MOUSE_DOWN);
        return;
    }
    if (focused >= 0 && rect_contains(win_rect(&windows[focused]), mx, my) && my >= windows[focused].y + TITLE_H) {
        if (e->dx || e->dy || released || e->buttons != prev_buttons)
            client_event(focused, released ? EV_MOUSE_UP : EV_MOUSE_MOVE);
        if (e->dz) {
            struct window *w = &windows[focused];
            send(focused, (struct event){.type = EV_SCROLL, .x = mx - w->x, .y = my - w->y - TITLE_H,
                                         .key = (uint32_t)(int32_t)e->dz, .buttons = (uint32_t)mbuttons});
        }
    } else if (released && focused >= 0) {
        client_event(focused, EV_MOUSE_UP);
    }
}

/* ---- Main loop --------------------------------------------------------------------------- */

void wm_init(const struct framebuffer *info) {
    fb = *info;
    uint64_t pages = ALIGN_UP((uint64_t)fb.width * fb.height * 4, PAGE_SIZE) / PAGE_SIZE;
    uint64_t bb = pmm_alloc_contig(pages), wp = pmm_alloc_contig(pages);
    if (!bb || !wp) panic("wm: not enough memory for the screen buffers");
    gfx_init_surface(&screen, P2V(bb), (int)fb.width, (int)fb.height, (int)fb.width);
    wallpaper = P2V(wp);
    render_wallpaper();
    mx = screen.w / 2;
    my = screen.h / 2;
    last_input = ticks;
    console_disable();
    damage_all();
    compose();
}

static void housekeeping(void) {
    struct rtc_time t;
    rtc_now(&t);
    static int last_net = -1;
    if (t.minute != last_minute || net_state() != last_net) {
        last_minute = t.minute;
        last_net = net_state();
        damage((struct rect){0, 0, screen.w, TOPBAR_H});
    }
    if (anim.wi >= 0) { /* next frame of the minimize / restore animation */
        damage(anim.last);
        if (ticks - anim.start >= ANIM_MS) {
            damage(win_damage_rect(&windows[anim.wi]));
            anim.wi = -1;
        } else {
            anim.last = anim_rect();
            damage(anim.last);
        }
    }
    if (!lock.mode && !afs_temporary() && afs_mounted() && ticks - last_input >= 60000) {
        static uint64_t checked_minute;
        uint64_t minute = ticks / 60000;
        if (minute != checked_minute) {
            checked_minute = minute;
            char v[SETTINGS_VALUE_MAX];
            int mins = 10; /* a sane default for an encrypted daily-driver install */
            if (settings_get("security.autolock", v, sizeof v) >= 0) {
                int n = 0;
                for (const char *p = v; *p >= '0' && *p <= '9'; p++) n = n * 10 + (*p - '0');
                mins = n;
            }
            if (mins > 0 && ticks - last_input >= (uint64_t)mins * 60000) wm_lock_now();
        }
    }
    if (ntoasts && ticks >= toasts[0].until) {
        damage((struct rect){screen.w - TOAST_W - 40, TOPBAR_H, TOAST_W + 40, (TOAST_H + 10) * MAX_TOASTS + 40});
        for (int i = 1; i < ntoasts; i++) toasts[i - 1] = toasts[i];
        ntoasts--;
    }
}

void wm_run(void) {
    uint64_t last_frame = 0;
    for (;;) {
        cli();
        usb_poll();
        i2c_hid_poll();
        struct input_event e;
        while (input_pop(&e)) {
            if (e.type == IN_KEY) handle_key(&e);
            else if (e.type == IN_MODS) handle_mods(&e);
            else handle_mouse(&e);
        }
        proc_reap();
        net_poll();
        housekeeping();
        if (lock.busy && ticks - last_frame >= 10 && rect_empty(dirty)) lock_work();
        /* Cap composition at ~100 fps; the dirty rect accumulates meanwhile. */
        if (!rect_empty(dirty) && ticks - last_frame >= 10) {
            compose();
            last_frame = ticks;
        }
        if (proc_others_ready()) {
            schedule(); /* let apps run; we come back on input, a timer slice, or when they block */
        } else {
            cur_thread->idle = 1;
            sti();
            hlt();
            cli();
            cur_thread->idle = 0;
        }
    }
}
