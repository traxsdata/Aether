/*
 * notes.c - a plain-text editor that runs as an untrusted app.
 *
 * It holds only CAP_WINDOW, CAP_PICK and CAP_STORAGE: it can't name any of
 * your files. Open and Save go through the system file picker, which hands
 * it exactly the one file you chose. Its unsaved draft lives in its own
 * private folder (/apps/notes), and the kernel's DLP labels follow the text:
 * open a Confidential file and the draft becomes Confidential too.
 *
 * Keys: Ctrl+O open, Ctrl+S save, Ctrl+N new, Ctrl+C/X/V clipboard,
 * arrows/Home/End to move. The clipboard is the kernel's label-aware one;
 * copying the whole document raises this app's taint to the file's label and
 * pasting raises it to the clipboard's label.
 */
#include "lib/ui.h"

#define W 760
#define H 500
#define PAD 16
#define BAR_H 44
#define STATUS_H 30
#define LINE_H 18
#define MAX_TEXT 65536
#define MIN(a, b) ((a) < (b) ? (a) : (b))

#define C_BG 0x0E1324
#define C_BAR 0x141B33
#define C_FG 0xE2E8FA
#define C_DIM 0x7C87A8
#define C_ACCENT 0x38E1F5
#define C_WARN 0xFFB547
#define C_ERR 0xFF5C7A

static struct window win;
static char text[MAX_TEXT + 1];
static int len, cur, top; /* top: first visible line */
static char filename[NAME_MAX + 2];
static int file_label = -1, dirty, draft_dirty;
static char status[96];
static uint32_t status_color = C_DIM;
static int hover = -1;
static uint64_t last_edit;

static const char *buttons[] = {"New", "Open", "Save", "Save as"};
#define NBUTTONS 4

static void set_status(uint32_t color, const char *msg) {
    strlcpy(status, msg, sizeof status);
    status_color = color;
}

static uint64_t now_ms(void) {
    struct time_info t;
    sys_time(&t);
    return t.uptime_ms;
}

/* ---- Files: the picker for your files, private storage for the draft ---- */

static void load_from(int h) {
    int64_t n = sys_read(h, text, MAX_TEXT);
    len = n > 0 ? (int)n : 0;
    text[len] = 0;
    cur = top = 0;
}

static void save_draft(void) {
    int h = (int)sys_open("draft.txt", O_WRITE | O_CREATE | O_TRUNC);
    if (h < 0) return;
    sys_write(h, text, (uint64_t)len);
    sys_close(h);
    draft_dirty = 0;
}

static void load_draft(void) {
    int h = (int)sys_open("draft.txt", O_READ);
    if (h < 0) return;
    load_from(h);
    sys_close(h);
    if (len) set_status(C_DIM, "Restored your unsaved draft");
}

static char pastebuf[MAX_TEXT + 1];

static void do_copy(int cut) {
    int label = file_label >= 0 ? file_label : 0;
    int64_t r = sys_clip_set(text, (uint64_t)len, label);
    if (r < 0) {
        set_status(C_ERR, r == -E_PERM ? "This Notes build has no clipboard access" : "Couldn't copy");
        return;
    }
    if (cut) {
        len = cur = top = 0;
        text[0] = 0;
        dirty = draft_dirty = 1;
        set_status(C_DIM, "Cut the whole document");
    } else {
        set_status(C_DIM, "Copied the whole document");
    }
}

static void do_paste(void) {
    struct clip_result ci;
    if (sys_clip_info(&ci) || !ci.len) {
        set_status(C_DIM, "The clipboard is empty");
        return;
    }
    if (ci.len > (uint64_t)(MAX_TEXT - len)) {
        set_status(C_ERR, "That clipboard text doesn't fit");
        return;
    }
    struct clip_result got;
    int64_t n = sys_clip_get(pastebuf, MAX_TEXT, &got);
    if (n < 0) {
        set_status(C_ERR, "Couldn't read the clipboard");
        return;
    }
    n = got.len;
    memmove(text + cur + n, text + cur, (size_t)(len - cur));
    memcpy(text + cur, pastebuf, (size_t)n);
    len += (int)n;
    cur += (int)n;
    text[len] = 0;
    dirty = draft_dirty = 1;
    char pasted[64];
    snprintf(pasted, sizeof pasted, "Pasted %ld bytes (%s)", (long)n, label_name((int)got.label));
    set_status(C_DIM, pasted);
}

static void do_open(void) {
    struct pick_req req = {PICK_OPEN, 0, {0}};
    struct pick_result res;
    int h = (int)sys_pick_file(&req, &res);
    if (h < 0) {
        set_status(C_DIM, h == -E_DENIED ? "Open cancelled" : "Couldn't open that file");
        return;
    }
    load_from(h);
    sys_close(h);
    strlcpy(filename, res.name, sizeof filename);
    file_label = (int)res.label;
    dirty = 0;
    draft_dirty = 1;
    set_status(C_DIM, "Opened");
}

static void do_save(int ask) {
    struct pick_req req = {PICK_SAVE, 0, {0}};
    strlcpy(req.suggested, filename[0] ? filename : "Untitled.txt", sizeof req.suggested);
    /* Without a picker grant there's no way back to a file we saved before,
     * so every save to your files goes through the picker: you see where it goes. */
    (void)ask;
    struct pick_result res;
    int h = (int)sys_pick_file(&req, &res);
    if (h < 0) {
        set_status(C_DIM, h == -E_DENIED ? "Save cancelled" : "Couldn't save there");
        return;
    }
    int64_t n = sys_write(h, text, (uint64_t)len);
    /* The picker deliberately does not truncate: a failed save must not
     * destroy the previous contents. Drop the old tail only after the new
     * text is safely written. */
    int64_t cut = n == len ? sys_truncate(h, (uint64_t)len) : -1;
    struct file_stat st;
    if (!sys_fstat(h, &st)) file_label = (int)st.label;
    sys_close(h);
    strlcpy(filename, res.name, sizeof filename);
    if (n == len && cut == 0) {
        dirty = 0;
        set_status(C_DIM, "Saved");
    } else {
        set_status(C_ERR, "Saving failed (disk full?)");
    }
}

static void do_new(void) {
    len = cur = top = 0;
    text[0] = 0;
    filename[0] = 0;
    file_label = -1;
    dirty = 0;
    draft_dirty = 1;
    set_status(C_DIM, "New note");
}

/* ---- Editing -------------------------------------------------------------- */

static int line_start(int pos) {
    while (pos > 0 && text[pos - 1] != '\n') pos--;
    return pos;
}

static int line_end(int pos) {
    while (pos < len && text[pos] != '\n') pos++;
    return pos;
}

static int line_of(int pos) {
    int n = 0;
    for (int i = 0; i < pos; i++) n += text[i] == '\n';
    return n;
}

static void insert(char c) {
    if (len >= MAX_TEXT) {
        set_status(C_ERR, "This note is full (64 KB)");
        return;
    }
    memmove(text + cur + 1, text + cur, (size_t)(len - cur));
    text[cur++] = c;
    text[++len] = 0;
    dirty = draft_dirty = 1;
    last_edit = now_ms();
}

static void erase(void) {
    if (!cur) return;
    memmove(text + cur - 1, text + cur, (size_t)(len - cur));
    cur--;
    text[--len] = 0;
    dirty = draft_dirty = 1;
    last_edit = now_ms();
}

static void move_vertical(int dir) {
    int col = cur - line_start(cur);
    if (dir < 0) {
        int ls = line_start(cur);
        if (!ls) return;
        int prev = line_start(ls - 1);
        cur = MIN(prev + col, ls - 1);
    } else {
        int le = line_end(cur);
        if (le >= len) return;
        int next = le + 1;
        cur = MIN(next + col, line_end(next));
    }
}

static void key(uint32_t k, uint32_t mods) {
    if (mods & MOD_CTRL) {
        if (k == 'o' || k == 'O') do_open();
        else if (k == 's' || k == 'S') do_save(0);
        else if (k == 'n' || k == 'N') do_new();
        else if (k == 'c' || k == 'C') do_copy(0);
        else if (k == 'x' || k == 'X') do_copy(1);
        else if (k == 'v' || k == 'V') do_paste();
        return;
    }
    switch (k) {
    case '\b': erase(); break;
    case KEY_DELETE:
        if (cur < len) cur++, erase();
        break;
    case KEY_LEFT: if (cur) cur--; break;
    case KEY_RIGHT: if (cur < len) cur++; break;
    case KEY_UP: move_vertical(-1); break;
    case KEY_DOWN: move_vertical(1); break;
    case KEY_HOME: cur = line_start(cur); break;
    case KEY_END: cur = line_end(cur); break;
    case '\t': for (int i = 0; i < 4; i++) insert(' '); break;
    default:
        if (k == '\n' || (k >= 32 && k < 127)) insert((char)k);
        break;
    }
}

/* ---- Drawing -------------------------------------------------------------- */

static struct rect button_rect(int i) { return (struct rect){PAD + i * 92, 8, 84, 28}; }

static void draw(void) {
    struct surface *s = &win.s;
    gfx_fill(s, 0, 0, s->w, s->h, C_BG);
    gfx_fill(s, 0, 0, s->w, BAR_H, C_BAR);
    gfx_blend(s, 0, BAR_H - 1, s->w, 1, C_ACCENT, 50);
    for (int i = 0; i < NBUTTONS; i++) {
        struct rect r = button_rect(i);
        gfx_round_rect(s, r.x, r.y, r.w, r.h, 8, hover == i ? 0x2A3560 : 0x1D2647, 255);
        gfx_text(s, r.x + (r.w - gfx_text_width(buttons[i], FONT_SMALL)) / 2, r.y + 6, buttons[i], C_FG, FONT_SMALL);
    }
    char name[80];
    snprintf(name, sizeof name, "%s%s", filename[0] ? filename : "Untitled", dirty ? " *" : "");
    gfx_text(s, s->w - PAD - gfx_text_width(name, FONT_SMALL), 14, name, C_DIM, FONT_SMALL);

    /* Text, with the view following the cursor. */
    int rows = (s->h - BAR_H - STATUS_H - 2 * 8) / LINE_H;
    int cl = line_of(cur);
    if (cl < top) top = cl;
    if (cl >= top + rows) top = cl - rows + 1;
    int cols = (s->w - 2 * PAD) / gfx_font_w(FONT_SMALL);
    int line = 0, pos = 0;
    while (pos <= len && line < top + rows) {
        int end = line_end(pos);
        if (line >= top) {
            int y = BAR_H + 8 + (line - top) * LINE_H;
            char buf[160];
            int n = MIN(end - pos, MIN(cols, (int)sizeof buf - 1));
            memcpy(buf, text + pos, (size_t)n);
            buf[n] = 0;
            gfx_text(s, PAD, y, buf, C_FG, FONT_SMALL);
            if (cur >= pos && cur <= end) {
                int cx = PAD + MIN(cur - pos, cols) * gfx_font_w(FONT_SMALL);
                gfx_fill(s, cx, y, 2, 16, C_ACCENT);
            }
        }
        line++;
        pos = end + 1;
    }

    /* Status bar: what the app is allowed to do, and the file's label. */
    int sy = s->h - STATUS_H;
    gfx_fill(s, 0, sy, s->w, STATUS_H, C_BAR);
    int x = gfx_text(s, PAD, sy + 7, status[0] ? status : "Untrusted app: it only sees the files you pick.",
                     status[0] ? status_color : C_WARN, FONT_SMALL);
    (void)x;
    if (file_label >= 0) {
        char lbl[32];
        snprintf(lbl, sizeof lbl, "Label: %s", label_name((uint32_t)file_label));
        gfx_text(s, s->w - PAD - gfx_text_width(lbl, FONT_SMALL), sy + 7, lbl, C_DIM, FONT_SMALL);
    }
    window_present(&win);
}

int main(void) {
    if (window_open(&win, W, H, "Notes")) return 1;
    window_set_resizable(&win, 1, 420, 200);
    load_draft();
    draw();
    for (;;) {
        struct event ev;
        int64_t got = sys_event_wait(&ev, draft_dirty ? 1500 : -1);
        if (got == 0) { /* quiet for a moment: keep the draft safe */
            if (draft_dirty && now_ms() - last_edit >= 1000) save_draft();
            continue;
        }
        if (ev.type == EV_KEY) {
            status[0] = 0;
            key(ev.key, ev.mods);
        } else if (ev.type == EV_MOUSE_MOVE || ev.type == EV_MOUSE_DOWN) {
            int h = -1;
            for (int i = 0; i < NBUTTONS; i++)
                if (rect_contains(button_rect(i), ev.x, ev.y)) h = i;
            int changed = h != hover;
            hover = h;
            if (ev.type == EV_MOUSE_DOWN && h >= 0) {
                if (h == 0) do_new();
                else if (h == 1) do_open();
                else do_save(h == 3);
            } else if (!changed) {
                continue;
            }
        } else if (ev.type == EV_CLOSE) {
            save_draft();
            return 0;
        } else if (ev.type == EV_RESIZE) {
            if (window_resize(&win)) continue;
        }
        draw();
    }
}
