/*
 * explorer.c - Files, the æther file explorer.
 *
 * A trusted app with CAP_FILES: it names paths under /home and /apps like
 * the Terminal does, and every operation goes through the same kernel rules
 * (vfs.c, dlp.c). It never works around them:
 *   - Reading a file taints Files with the file's label, exactly as for any
 *     app. So the preview pane reads a file on its own only when it's
 *     Personal or lower, or no higher than what Files already holds;
 *     Confidential and Secret files wait until you ask, and it says what
 *     reading them will do.
 *   - Copies are made by reading and writing, so the copy keeps (at least)
 *     the original's label. Folders get their label raised to match.
 *   - Lowering a label is the kernel's question, not ours: SYS_SET_LABEL
 *     shows the system dialog and we just report the answer.
 *   - Refusals (E_DENIED, E_PERM on system folders, ...) become plain
 *     sentences in the status line.
 *
 * Keys: arrows/PgUp/PgDn/Home/End move, Enter opens, Backspace goes up,
 * Space previews, Del deletes, Ctrl+N new folder, Ctrl+R rename,
 * Ctrl+C/X/V copy/cut/paste, Ctrl+L label, letters jump to a name, Esc closes.
 */
#include "lib/ui.h"

#define W 980
#define H 560
#define PAD 14
#define TOOL_H 52
#define SIDE_W 168
#define PREV_W 250
#define STATUS_H 30
#define HEAD_H 30
#define ROW_H 28
#define MAX_ENTRIES 256
#define SNIPPET_MAX 2048
#define VIEW_MAX 65536
#define MAX_DEPTH 12 /* folders nested deeper than this aren't copied or deleted as a whole */
#define CHUNK 4096
#define DOUBLE_CLICK_MS 450
#define MIN(a, b) ((a) < (b) ? (a) : (b))
#define MAX(a, b) ((a) > (b) ? (a) : (b))

#define C_BG 0x0D1222
#define C_SIDE 0x0A0F1D
#define C_BAR 0x131A31
#define C_FIELD 0x0A0F1E
#define C_BUTTON 0x1D2647
#define C_BUTTON_HOT 0x2A3560
#define C_ROW_HOT 0x161E38
#define C_SEL 0x173356
#define C_FG 0xE2E8FA
#define C_DIM 0x7C87A8
#define C_FAINT 0x4A5578
#define C_ACCENT 0x38E1F5
#define C_FOLDER 0x4FB3FF
#define C_OK 0x5CF2B0
#define C_WARN 0xFFB547
#define C_ERR 0xFF5C7A

/* The kernel's colours for each label (dlp.c), so badges match the title bar. */
static const uint32_t label_colors[LABEL_COUNT] = {0x8C96B8, 0x5CF2B0, 0xFFB547, 0xFF5C7A};

struct entry {
    char name[NAME_MAX + 2];
    uint32_t type, label;
    uint64_t size, mtime;
};

static struct window win;
static int ww, wh; /* window size */

/* The folder being shown. */
static char cwd[PATH_MAX] = "/home";
static uint32_t cwd_label;
static struct entry ents[MAX_ENTRIES];
static int nents, sel = -1, top, truncated;

/* Preview pane: a snippet of the selected file, when we may read it. */
static char snippet[SNIPPET_MAX + 1];
static int snippet_len, snippet_state; /* SN_* */
enum { SN_NONE, SN_TEXT, SN_BINARY, SN_ASK, SN_ERROR };

/* Read-only viewer (double-click or Enter on a file). */
static char view_text[VIEW_MAX + 1];
static int view_len, view_open, view_top, view_label, view_binary, view_more;
static char view_name[NAME_MAX + 2];

/* Clipboard: one path, copied or cut. */
static char clip[PATH_MAX];
static int clip_cut;

/* In-window dialogs. */
enum { M_NONE, M_NEWDIR, M_RENAME, M_DELETE, M_LABEL };
static int modal, modal_choice;
static int modal_backdrop; /* the dimmed window is on screen, the pixels under the box saved (draw_modal) */
static char field[NAME_MAX + 1];
static int field_len;
static char modal_msg[2][120];

static char status[120];
static uint32_t status_color = C_DIM;
static struct fs_info fsi;

/* Pointer state. */
static int hot = -1;
static uint64_t last_click_ms;
static int last_click_row = -1;

static uint64_t now_ms(void) {
    struct time_info t;
    sys_time(&t);
    return t.uptime_ms;
}

static void draw(void);

static void say(uint32_t color, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(status, sizeof status, fmt, ap);
    va_end(ap);
    status_color = color;
}

/* Every refusal the kernel can give, as a sentence. */
static const char *why(int64_t e) {
    switch (-e) {
    case E_PERM: return "that's a system folder, so it can't be changed";
    case E_NOENT: return "it isn't there any more";
    case E_EXIST: return "something with that name is already there";
    case E_NOTDIR: return "that isn't a folder";
    case E_ISDIR: return "that's a folder";
    case E_NOSPACE: return "the disk is full";
    case E_IO: return "disk error, or the data failed its integrity check";
    case E_DENIED: return "refused by data protection, or by you";
    case E_LOCKED: return "your files are still locked";
    case E_NOTEMPTY: return "the folder isn't empty";
    case E_LIMIT: return "it's open in another app";
    case E_INVAL: return "that name isn't allowed";
    case E_NOMEM: return "out of memory";
    default: return "something went wrong";
    }
}

/* ---- Paths ----------------------------------------------------------------- */

/* dir + "/" + name; 0 if it doesn't fit. */
static int join(char *out, const char *dir, const char *name) {
    int n = snprintf(out, PATH_MAX, "%s%s%s", dir, strcmp(dir, "/") ? "/" : "", name);
    return n > 0 && n < PATH_MAX;
}

static const char *base_name(const char *path) {
    const char *b = path;
    for (const char *p = path; *p; p++)
        if (*p == '/' && p[1]) b = p + 1;
    return b;
}

static void parent_of(const char *path, char *out) {
    strlcpy(out, path, PATH_MAX);
    char *slash = 0;
    for (char *p = out; *p; p++)
        if (*p == '/') slash = p;
    if (!slash || slash == out) strlcpy(out, "/", PATH_MAX);
    else *slash = 0;
}

/* `inner` is `outer` or somewhere inside it. */
static int path_within(const char *inner, const char *outer) {
    size_t n = strlen(outer);
    return !strncmp(inner, outer, n) && (inner[n] == 0 || inner[n] == '/' || !strcmp(outer, "/"));
}

/* A name the file system accepts: not empty, not . or .., no '/'. */
static int valid_name(const char *s) {
    if (!*s || !strcmp(s, ".") || !strcmp(s, "..") || strlen(s) > NAME_MAX) return 0;
    for (; *s; s++)
        if (*s == '/' || (unsigned char)*s < 32) return 0;
    return 1;
}

static int lower(int c) { return c >= 'A' && c <= 'Z' ? c + 32 : c; }

static int name_cmp(const char *a, const char *b) {
    for (; *a && lower(*a) == lower(*b); a++, b++) {}
    if (lower(*a) != lower(*b)) return lower(*a) - lower(*b);
    return strcmp(a, b);
}

static int exists(const char *path) {
    struct file_stat st;
    return sys_stat(path, &st) == 0;
}

/* A free name in `dir` based on `name`: "report.txt", "report 2.txt", ... */
static int unique_path(char *out, const char *dir, const char *name) {
    if (!join(out, dir, name)) return 0;
    if (!exists(out)) return 1;
    const char *dot = 0;
    for (const char *p = name + 1; *p; p++)
        if (*p == '.') dot = p;
    int stem = dot ? (int)(dot - name) : (int)strlen(name);
    for (int i = 2; i < 100; i++) {
        char cand[NAME_MAX + 16];
        snprintf(cand, sizeof cand, "%.*s %d%s", stem, name, i, dot ? dot : "");
        if (strlen(cand) > NAME_MAX || !join(out, dir, cand)) return 0;
        if (!exists(out)) return 1;
    }
    return 0;
}

/* ---- Formatting ---------------------------------------------------------------- */

static void format_size(uint64_t n, char *out, size_t len) {
    if (n < 1024) snprintf(out, len, "%lu B", n);
    else if (n < 1024 * 1024) snprintf(out, len, "%lu.%lu KB", n / 1024, n % 1024 * 10 / 1024);
    else snprintf(out, len, "%lu.%lu MB", n >> 20, (n & 0xFFFFF) * 10 >> 20);
}

static int leap(unsigned y) { return (y % 4 == 0 && y % 100 != 0) || y % 400 == 0; }

/* Seconds since 1970 as "Today 14:03", "Oct 18 14:03" or "Oct 18 2025". */
static void format_time(uint64_t t, char *out, size_t len) {
    static const char *months[] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun",
                                   "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};
    static const uint8_t mdays[] = {31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31};
    if (!t) {
        strlcpy(out, "-", len);
        return;
    }
    uint64_t days = t / 86400, secs = t % 86400;
    unsigned year = 1970, month = 0;
    while (days >= (uint64_t)(leap(year) ? 366 : 365)) days -= leap(year) ? 366 : 365, year++;
    while (days >= (uint64_t)(mdays[month] + (month == 1 && leap(year)))) days -= mdays[month] + (month == 1 && leap(year)), month++;
    unsigned day = (unsigned)days + 1;
    struct time_info now;
    sys_time(&now);
    if (now.year == year && now.month == month + 1 && now.day == day)
        snprintf(out, len, "Today %02lu:%02lu", secs / 3600, secs / 60 % 60);
    else if (now.year == year)
        snprintf(out, len, "%s %u %02lu:%02lu", months[month], day, secs / 3600, secs / 60 % 60);
    else
        snprintf(out, len, "%s %u %u", months[month], day, year);
}

/* Copies `in` into `out`, shortened with ".." to fit `px` pixels of small text. */
static void fit(char *out, size_t len, const char *in, int px) {
    strlcpy(out, in, len);
    if (gfx_text_width(out, FONT_SMALL) <= px) return;
    size_t n = strlen(out);
    while (n > 0) {
        n--;
        while (n > 0 && ((unsigned char)out[n] & 0xC0) == 0x80) n--; /* whole UTF-8 characters */
        if (n + 3 > len) continue;
        out[n] = '.', out[n + 1] = '.', out[n + 2] = 0;
        if (gfx_text_width(out, FONT_SMALL) <= px) return;
    }
}

/* ---- The folder listing -------------------------------------------------------- */

static int is_text(const char *buf, int n) {
    int odd = 0;
    for (int i = 0; i < n; i++) {
        unsigned char c = (unsigned char)buf[i];
        if (c == 0) return 0;
        if (c < 32 && c != '\n' && c != '\r' && c != '\t') odd++;
    }
    return odd * 20 <= n;
}

static void refresh_fs_info(void) {
    if (sys_fs_info(&fsi) < 0) memset(&fsi, 0, sizeof fsi);
}

/* Reads up to `max` bytes of `path` into buf; returns bytes or -E_*. This is the read that taints. */
static int64_t read_head(const char *path, char *buf, int max) {
    int h = (int)sys_open(path, O_READ);
    if (h < 0) return h;
    int64_t n = sys_read(h, buf, (uint64_t)max);
    sys_close(h);
    refresh_fs_info(); /* our label may have just gone up */
    return n;
}

static struct entry *selected(void) { return sel >= 0 && sel < nents ? &ents[sel] : 0; }

static void load_snippet(int force) {
    struct entry *e = selected();
    snippet_len = 0;
    snippet_state = SN_NONE;
    if (!e || e->type != FT_FILE) return;
    /* Previews are read on their own only up to Personal, the label your
     * Home starts with (it changes nothing about where data may go), or up
     * to what Files already holds. Anything higher waits to be asked. */
    if (e->label > MAX(fsi.taint, (uint32_t)LABEL_PERSONAL) && !force) {
        snippet_state = SN_ASK;
        return;
    }
    char path[PATH_MAX];
    if (!join(path, cwd, e->name)) return;
    int64_t n = read_head(path, snippet, SNIPPET_MAX);
    if (n < 0) {
        snippet_state = SN_ERROR;
        say(C_ERR, "Can't preview %s: %s", e->name, why(n));
        return;
    }
    snippet_len = (int)n;
    snippet[snippet_len] = 0;
    snippet_state = is_text(snippet, snippet_len) ? SN_TEXT : SN_BINARY;
}

static int visible_rows(void) { return MAX(1, (wh - TOOL_H - STATUS_H - HEAD_H - 8) / ROW_H); }

static void select_row(int i) {
    if (!nents) {
        sel = -1;
        load_snippet(0);
        return;
    }
    sel = MAX(0, MIN(i, nents - 1));
    int rows = visible_rows();
    if (sel < top) top = sel;
    if (sel >= top + rows) top = sel - rows + 1;
    load_snippet(0);
}

static int entry_before(const struct entry *a, const struct entry *b) {
    if (a->type != b->type) return a->type == FT_DIR; /* folders first */
    return name_cmp(a->name, b->name) < 0;
}

/* Lists cwd, folders first, by name; keeps `keep` selected if it's there. */
static int64_t load_dir(const char *keep) {
    int h = (int)sys_open(cwd, O_DIR);
    if (h < 0) return h;
    nents = truncated = 0;
    struct dir_entry de;
    for (uint32_t i = 0; sys_readdir(h, i, &de) == 1; i++) {
        if (nents == MAX_ENTRIES) {
            truncated = 1;
            break;
        }
        struct entry *e = &ents[nents++];
        strlcpy(e->name, de.name, sizeof e->name);
        e->type = de.type;
        e->label = de.label;
        e->size = de.size;
        e->mtime = 0;
        char path[PATH_MAX];
        struct file_stat st;
        if (join(path, cwd, de.name) && sys_stat(path, &st) == 0) e->mtime = st.mtime;
    }
    sys_close(h);
    for (int i = 1; i < nents; i++) { /* insertion sort: folders are small */
        struct entry e = ents[i];
        int j = i;
        for (; j > 0 && entry_before(&e, &ents[j - 1]); j--) ents[j] = ents[j - 1];
        ents[j] = e;
    }
    struct file_stat st;
    cwd_label = sys_stat(cwd, &st) == 0 ? st.label : 0;
    refresh_fs_info();
    int want = 0;
    for (int i = 0; keep && i < nents; i++)
        if (!strcmp(ents[i].name, keep)) want = i;
    top = 0;
    select_row(want);
    return 0;
}

static void go(const char *path, const char *keep) {
    char old[PATH_MAX];
    strlcpy(old, cwd, sizeof old);
    strlcpy(cwd, path, sizeof cwd);
    int64_t r = load_dir(keep);
    if (r < 0) {
        say(C_ERR, "Can't open %s: %s", path, why(r));
        strlcpy(cwd, old, sizeof cwd);
        if (load_dir(0) < 0) { /* the old folder went away too */
            strlcpy(cwd, "/home", sizeof cwd);
            load_dir(0);
        }
    }
}

static void go_up(void) {
    if (!strcmp(cwd, "/")) return;
    char up[PATH_MAX];
    parent_of(cwd, up);
    char from[NAME_MAX + 2];
    strlcpy(from, base_name(cwd), sizeof from);
    go(up, from);
}

static void reload(void) {
    char keep[NAME_MAX + 2] = "";
    if (selected()) strlcpy(keep, selected()->name, sizeof keep);
    go(cwd, keep);
}

/* ---- Opening files ------------------------------------------------------------- */

static void open_viewer(void) {
    struct entry *e = selected();
    char path[PATH_MAX];
    if (!e || !join(path, cwd, e->name)) return;
    int64_t n = read_head(path, view_text, VIEW_MAX);
    if (n < 0) {
        say(C_ERR, "Can't open %s: %s", e->name, why(n));
        return;
    }
    view_len = (int)n;
    view_text[view_len] = 0;
    view_binary = !is_text(view_text, view_len);
    view_more = e->size > (uint64_t)view_len;
    view_label = (int)e->label;
    view_top = 0;
    view_open = 1;
    strlcpy(view_name, e->name, sizeof view_name);
    say(C_DIM, "Read-only. Esc closes.");
}

static void open_selected(void) {
    struct entry *e = selected();
    if (!e) return;
    if (e->type == FT_DIR) {
        char path[PATH_MAX];
        if (!join(path, cwd, e->name)) return say(C_ERR, "That path is too long");
        go(path, 0);
    } else {
        open_viewer();
    }
}

/* ---- Copy, move, delete ------------------------------------------------------- */

static char chunk[CHUNK];

/* After a copy, give `dst` at least the original's label (raising is free). */
static void match_label(const char *dst, uint32_t label) {
    struct file_stat st;
    if (sys_stat(dst, &st) == 0 && st.label < label) sys_set_label(dst, (int)label);
}

static int64_t copy_tree(const char *src, const char *dst, int depth) {
    struct file_stat st;
    int64_t r = sys_stat(src, &st);
    if (r < 0) return r;
    if (depth > MAX_DEPTH) return -E_LIMIT;
    if (st.type == FT_DIR) {
        if ((r = sys_mkdir(dst)) < 0) return r;
        int h = (int)sys_open(src, O_DIR);
        if (h < 0) return h;
        struct dir_entry de;
        for (uint32_t i = 0; r >= 0 && sys_readdir(h, i, &de) == 1; i++) {
            char a[PATH_MAX], b[PATH_MAX];
            if (!join(a, src, de.name) || !join(b, dst, de.name)) r = -E_INVAL;
            else r = copy_tree(a, b, depth + 1);
        }
        sys_close(h);
    } else {
        int in = (int)sys_open(src, O_READ);
        if (in < 0) return in;
        int out = (int)sys_open(dst, O_WRITE | O_CREATE | O_EXCL);
        if (out < 0) {
            sys_close(in);
            return out;
        }
        for (;;) {
            int64_t n = sys_read(in, chunk, CHUNK);
            if (n <= 0) {
                r = n;
                break;
            }
            int64_t put = sys_write(out, chunk, (uint64_t)n);
            if (put != n) {
                r = put < 0 ? put : -E_NOSPACE;
                break;
            }
        }
        sys_close(in);
        sys_close(out);
    }
    match_label(dst, st.label);
    return r < 0 ? r : 0;
}

/* Files and folders inside `path` (not counting itself), up to `cap`. */
static int count_tree(const char *path, int depth, int cap) {
    int h = (int)sys_open(path, O_DIR);
    if (h < 0) return 0;
    int n = 0;
    struct dir_entry de;
    for (uint32_t i = 0; n < cap && sys_readdir(h, i, &de) == 1; i++) {
        n++;
        char sub[PATH_MAX];
        if (de.type == FT_DIR && depth < MAX_DEPTH && join(sub, path, de.name)) n += count_tree(sub, depth + 1, cap - n);
    }
    sys_close(h);
    return n;
}

static int64_t remove_tree(const char *path, int depth) {
    struct file_stat st;
    int64_t r = sys_stat(path, &st);
    if (r < 0) return r;
    if (st.type == FT_DIR) {
        if (depth > MAX_DEPTH) return -E_LIMIT;
        /* Always take the first entry: removing shifts the rest down. */
        for (;;) {
            int h = (int)sys_open(path, O_DIR);
            if (h < 0) return h;
            struct dir_entry de;
            int more = sys_readdir(h, 0, &de) == 1;
            sys_close(h);
            if (!more) break;
            char sub[PATH_MAX];
            if (!join(sub, path, de.name)) return -E_INVAL;
            if ((r = remove_tree(sub, depth + 1)) < 0) return r;
        }
    }
    return sys_remove(path);
}

static void do_copy(int cut) {
    struct entry *e = selected();
    if (!e || !join(clip, cwd, e->name)) return;
    clip_cut = cut;
    say(C_DIM, "%s %s: open a folder and paste (Ctrl+V)", cut ? "Cut" : "Copied", e->name);
}

static void do_paste(void) {
    if (!clip[0]) return say(C_DIM, "Nothing to paste: copy or cut something first");
    char dst[PATH_MAX], from_dir[PATH_MAX];
    const char *name = base_name(clip);
    if (path_within(cwd, clip)) return say(C_ERR, "Can't put a folder inside itself");
    parent_of(clip, from_dir);
    if (clip_cut) {
        if (!strcmp(from_dir, cwd)) return say(C_DIM, "%s is already here", name);
        if (!join(dst, cwd, name)) return say(C_ERR, "That path is too long");
        int64_t r = sys_rename(clip, dst);
        if (r < 0) return say(C_ERR, "Can't move %s: %s", name, why(r));
        say(C_OK, "Moved %s here", name);
        clip[0] = 0;
    } else {
        if (!unique_path(dst, cwd, name)) return say(C_ERR, "Can't find a free name for the copy");
        say(C_DIM, "Copying %s...", name);
        int64_t r = copy_tree(clip, dst, 0);
        refresh_fs_info();
        if (r < 0) {
            say(C_ERR, "Copying %s stopped: %s", name, why(r));
        } else if (!strcmp(name, base_name(dst))) {
            say(C_OK, "Copied %s here", name);
        } else {
            say(C_OK, "Copied %s here as %s", name, base_name(dst));
        }
    }
    char keep[NAME_MAX + 2];
    strlcpy(keep, base_name(dst), sizeof keep);
    go(cwd, keep);
}

/* ---- Dialogs ---------------------------------------------------------------------- */

static void open_modal(int kind) {
    struct entry *e = selected();
    if (kind != M_NEWDIR && !e) return say(C_DIM, "Select something first");
    modal = kind;
    modal_backdrop = 0;
    modal_choice = 0;
    modal_msg[0][0] = modal_msg[1][0] = 0;
    if (kind == M_NEWDIR) {
        char path[PATH_MAX];
        strlcpy(field, unique_path(path, cwd, "New folder") ? base_name(path) : "New folder", sizeof field);
        uint32_t label = MAX(cwd_label, fsi.taint);
        snprintf(modal_msg[0], sizeof modal_msg[0], "It will be labelled %s.", label_name(label));
        if (fsi.taint > cwd_label)
            snprintf(modal_msg[1], sizeof modal_msg[1], "(Files holds %s data, and new things carry that label.)",
                     label_name(fsi.taint));
    } else if (kind == M_RENAME) {
        strlcpy(field, e->name, sizeof field);
    } else if (kind == M_DELETE) {
        char path[PATH_MAX];
        int n = e->type == FT_DIR && join(path, cwd, e->name) ? count_tree(path, 0, 10000) : 0;
        if (e->type == FT_FILE) {
            snprintf(modal_msg[0], sizeof modal_msg[0], "\"%s\" will be gone for good.", e->name);
        } else if (!n) {
            snprintf(modal_msg[0], sizeof modal_msg[0], "The folder \"%s\" is empty.", e->name);
        } else {
            snprintf(modal_msg[0], sizeof modal_msg[0], "The folder \"%s\" isn't empty:", e->name);
            snprintf(modal_msg[1], sizeof modal_msg[1], "it holds %d item%s, and they'll be deleted too.", n, n == 1 ? "" : "s");
        }
    } else if (kind == M_LABEL) {
        modal_choice = (int)e->label;
    }
    field_len = (int)strlen(field);
}

static void close_modal(void) { modal = M_NONE; }

static void confirm_modal(void) {
    struct entry *e = selected();
    char path[PATH_MAX], dst[PATH_MAX];
    int kind = modal;
    close_modal();
    if (kind == M_NEWDIR) {
        if (!valid_name(field)) return say(C_ERR, "That name isn't allowed (no / and not . or ..)");
        if (!join(path, cwd, field)) return say(C_ERR, "That path is too long");
        int64_t r = sys_mkdir(path);
        if (r < 0) return say(C_ERR, "Can't make %s: %s", field, why(r));
        say(C_OK, "Made the folder %s", field);
        go(cwd, field);
    } else if (kind == M_RENAME && e) {
        if (!strcmp(field, e->name)) return;
        if (!valid_name(field)) return say(C_ERR, "That name isn't allowed (no / and not . or ..)");
        if (!join(path, cwd, e->name) || !join(dst, cwd, field)) return say(C_ERR, "That path is too long");
        int64_t r = sys_rename(path, dst);
        if (r < 0) return say(C_ERR, "Can't rename %s: %s", e->name, why(r));
        if (!strcmp(clip, path)) strlcpy(clip, dst, sizeof clip);
        say(C_OK, "Renamed to %s", field);
        go(cwd, field);
    } else if (kind == M_DELETE && e) {
        char name[NAME_MAX + 2];
        strlcpy(name, e->name, sizeof name);
        if (!join(path, cwd, name)) return;
        int64_t r = remove_tree(path, 0);
        if (r < 0) say(C_ERR, "Can't delete %s: %s", name, why(r));
        else say(C_OK, "Deleted %s", name);
        if (path_within(clip, path)) clip[0] = 0;
        int keep = sel;
        go(cwd, 0);
        select_row(keep);
    } else if (kind == M_LABEL && e) {
        int to = modal_choice;
        if (to == (int)e->label || !join(path, cwd, e->name)) return;
        if (to < (int)e->label) {
            /* The kernel asks you in its own dialog; we wait for the answer. */
            say(C_WARN, "Lowering a label needs your OK: answer the system dialog");
            draw();
        }
        int64_t r = sys_set_label(path, to);
        if (r == -E_DENIED) say(C_WARN, "%s stays %s", e->name, label_name(e->label));
        else if (r < 0) say(C_ERR, "Can't change the label: %s", why(r));
        else say(C_OK, "%s is now %s", e->name, label_name((uint32_t)to));
        reload();
    }
}

/* ---- Layout ------------------------------------------------------------------------- */

enum { B_UP, B_NEWDIR, B_RENAME, B_DELETE, B_COPY, B_CUT, B_PASTE, B_LABEL, NBUTTONS };
static const char *button_text[NBUTTONS] = {"Up", "New folder", "Rename", "Delete", "Copy", "Cut", "Paste", "Label"};

/* Hit-test ids: what the pointer is over. */
enum { HIT_BUTTON = 0, HIT_PLACE = 100, HIT_CRUMB = 200, HIT_ROW = 1000, HIT_PREVIEW = 5000, HIT_MODAL = 6000,
       HIT_VIEW_CLOSE = 7000 };

static const struct {
    const char *name, *path;
} places[] = {{"Home", "/home"}, {"Apps", "/apps"}, {"All files", "/"}};
#define NPLACES 3

static struct rect button_rect(int i) {
    if (i == B_UP) return (struct rect){PAD, 12, 44, 28};
    int x = ww - PAD;
    for (int k = NBUTTONS - 1; k >= i; k--) {
        int w = gfx_text_width(button_text[k], FONT_SMALL) + 24;
        x -= w + (k == B_COPY || k == B_LABEL ? 14 : 6); /* small gaps between groups */
        if (k == i) return (struct rect){x, 12, w, 28};
    }
    return (struct rect){0, 0, 0, 0};
}

static int button_enabled(int i) {
    if (view_open) return i == B_UP;
    if (i == B_UP) return strcmp(cwd, "/") != 0;
    if (i == B_NEWDIR) return 1;
    if (i == B_PASTE) return clip[0] != 0;
    return selected() != 0;
}

static struct rect path_rect(void) {
    int x = PAD + 44 + 8;
    return (struct rect){x, 12, button_rect(B_NEWDIR).x - 14 - x, 28};
}

/* Breadcrumbs: the path's folders, root first, as clickable pieces. */
#define MAX_CRUMBS 24
static struct {
    char path[PATH_MAX];
    char name[NAME_MAX + 2];
    struct rect r;
} crumbs[MAX_CRUMBS];
static int ncrumbs, crumbs_cut;

static const char *nice_name(const char *path) {
    if (!strcmp(path, "/")) return "All files";
    if (!strcmp(path, "/home")) return "Home";
    if (!strcmp(path, "/apps")) return "Apps";
    return base_name(path);
}

static void layout_crumbs(void) {
    ncrumbs = 0;
    char p[PATH_MAX] = "/";
    const char *s = cwd;
    for (;;) {
        if (ncrumbs < MAX_CRUMBS) {
            strlcpy(crumbs[ncrumbs].path, p, PATH_MAX);
            strlcpy(crumbs[ncrumbs].name, nice_name(p), sizeof crumbs[0].name);
            ncrumbs++;
        }
        while (*s == '/') s++;
        if (!*s) break;
        const char *e = s;
        while (*e && *e != '/') e++;
        size_t n = strlen(p);
        if (n > 1) p[n++] = '/';
        memcpy(p + n, s, (size_t)(e - s));
        p[n + (size_t)(e - s)] = 0;
        s = e;
    }
    /* Drop crumbs from the left until the rest fit, keeping the last one. */
    struct rect pr = path_rect();
    int sep = 22, avail = pr.w - 20;
    crumbs_cut = 0;
    for (;;) {
        int w = crumbs_cut ? gfx_text_width("..", FONT_SMALL) + sep : 0;
        for (int i = crumbs_cut; i < ncrumbs; i++) w += gfx_text_width(crumbs[i].name, FONT_SMALL) + (i > crumbs_cut ? sep : 0);
        if (w <= avail || crumbs_cut >= ncrumbs - 1) break;
        crumbs_cut++;
    }
    int x = pr.x + 10 + (crumbs_cut ? gfx_text_width("..", FONT_SMALL) + sep : 0);
    for (int i = 0; i < ncrumbs; i++) {
        if (i < crumbs_cut) {
            crumbs[i].r = (struct rect){0, 0, 0, 0};
            continue;
        }
        int w = gfx_text_width(crumbs[i].name, FONT_SMALL);
        crumbs[i].r = (struct rect){x - 4, pr.y + 3, w + 8, pr.h - 6};
        x += w + sep;
    }
}

static struct rect place_rect(int i) { return (struct rect){10, TOOL_H + 36 + i * 32, SIDE_W - 20, 28}; }
static struct rect list_rect(void) { return (struct rect){SIDE_W, TOOL_H, ww - SIDE_W - PREV_W, wh - TOOL_H - STATUS_H}; }
static struct rect prev_rect(void) { return (struct rect){ww - PREV_W, TOOL_H, PREV_W, wh - TOOL_H - STATUS_H}; }
static struct rect view_rect(void) { return (struct rect){SIDE_W, TOOL_H, ww - SIDE_W, wh - TOOL_H - STATUS_H}; }

static struct rect row_rect(int i) {
    struct rect l = list_rect();
    return (struct rect){l.x + 8, l.y + HEAD_H + 4 + (i - top) * ROW_H, l.w - 16, ROW_H};
}

static struct rect preview_button_rect(void) {
    struct rect p = prev_rect();
    return (struct rect){p.x + 16, p.y + 240, p.w - 32, 30}; /* under a file's facts */
}

static struct rect view_close_rect(void) {
    struct rect v = view_rect();
    return (struct rect){v.x + v.w - 16 - 80, v.y + 10, 80, 28};
}

static struct rect modal_rect(void) {
    int mw = 460, mh = modal == M_LABEL ? 210 : 190;
    return (struct rect){(ww - mw) / 2, (wh - mh) / 2, mw, mh};
}

/* Dialog buttons: 0 cancel, 1 confirm; for M_LABEL, 10 + label are the choices. */
static struct rect modal_button(int k) {
    struct rect m = modal_rect();
    if (k >= 10) {
        int bw = (m.w - 48 - 3 * 8) / 4;
        return (struct rect){m.x + 24 + (k - 10) * (bw + 8), m.y + 92, bw, 34};
    }
    return (struct rect){k ? m.x + m.w - 24 - 120 : m.x + m.w - 24 - 2 * 120 - 10, m.y + m.h - 52, 120, 34};
}

static int hit_test(int x, int y) {
    if (modal) {
        for (int k = 0; k < 2; k++)
            if (rect_contains(modal_button(k), x, y)) return HIT_MODAL + k;
        if (modal == M_LABEL)
            for (int k = 0; k < LABEL_COUNT; k++)
                if (rect_contains(modal_button(10 + k), x, y)) return HIT_MODAL + 10 + k;
        return -1;
    }
    for (int i = 0; i < NBUTTONS; i++)
        if (rect_contains(button_rect(i), x, y)) return HIT_BUTTON + i;
    for (int i = crumbs_cut; i < ncrumbs; i++)
        if (rect_contains(crumbs[i].r, x, y)) return HIT_CRUMB + i;
    for (int i = 0; i < NPLACES; i++)
        if (rect_contains(place_rect(i), x, y)) return HIT_PLACE + i;
    if (view_open) return rect_contains(view_close_rect(), x, y) ? HIT_VIEW_CLOSE : -1;
    for (int i = top; i < nents && i < top + visible_rows(); i++)
        if (rect_contains(row_rect(i), x, y)) return HIT_ROW + i;
    if (snippet_state == SN_ASK && rect_contains(preview_button_rect(), x, y)) return HIT_PREVIEW;
    return -1;
}

/* ---- Drawing ---------------------------------------------------------------------- */

static int chip(int x, int y, const char *text, uint32_t color) {
    int w = gfx_text_width(text, FONT_SMALL) + 16;
    gfx_round_rect(&win.s, x, y, w, 20, 10, color, 40);
    gfx_round_outline(&win.s, x, y, w, 20, 10, color, 160);
    gfx_text(&win.s, x + 8, y + 2, text, color, FONT_SMALL);
    return w;
}

static void button(struct rect r, const char *text, int is_hot, int enabled, uint32_t accent) {
    struct surface *s = &win.s;
    gfx_round_rect(s, r.x, r.y, r.w, r.h, 8, is_hot && enabled ? C_BUTTON_HOT : C_BUTTON, 255);
    gfx_blend(s, r.x + 8, r.y, r.w - 16, 1, 0xFFFFFF, enabled ? 24 : 8); /* glass highlight */
    if (accent) gfx_round_outline(s, r.x, r.y, r.w, r.h, 8, accent, is_hot ? 200 : 110);
    gfx_text(s, r.x + (r.w - gfx_text_width(text, FONT_SMALL)) / 2, r.y + (r.h - 16) / 2, text,
             enabled ? (accent ? accent : C_FG) : C_FAINT, FONT_SMALL);
}

static void icon_folder(int x, int y, uint32_t color) {
    struct surface *s = &win.s;
    gfx_round_rect(s, x, y + 1, 9, 6, 2, gfx_mix(color, 0x000000, 60), 255); /* tab */
    gfx_round_rect(s, x, y + 4, 18, 13, 3, color, 255);
    gfx_blend(s, x + 2, y + 6, 14, 1, 0xFFFFFF, 70);
}

static void icon_file(int x, int y, int text, uint32_t color) {
    struct surface *s = &win.s;
    gfx_round_rect(s, x + 2, y, 14, 18, 2, 0xC9D3F0, 255);
    gfx_fill(s, x + 11, y, 5, 5, C_BG); /* folded corner */
    gfx_fill(s, x + 11, y + 4, 4, 1, 0x8C96B8);
    gfx_fill(s, x + 11, y, 1, 4, 0x8C96B8);
    if (text)
        for (int i = 0; i < 3; i++) gfx_fill(s, x + 5, y + 7 + i * 3, i == 2 ? 5 : 8, 1, color);
    else
        gfx_fill(s, x + 5, y + 8, 8, 6, color);
}

static int name_is_text(const char *name) {
    size_t n = strlen(name);
    return (n > 4 && !strcmp(name + n - 4, ".txt")) || (n > 3 && !strcmp(name + n - 3, ".md")) ||
           (n > 4 && !strcmp(name + n - 4, ".log")) || (n > 4 && !strcmp(name + n - 4, ".csv"));
}

static void draw_toolbar(void) {
    struct surface *s = &win.s;
    gfx_gradient_v(s, 0, 0, ww, TOOL_H, 0x182040, C_BAR);
    gfx_blend(s, 0, TOOL_H - 1, ww, 1, C_ACCENT, 50);
    for (int i = 0; i < NBUTTONS; i++) {
        struct rect r = button_rect(i);
        int on = button_enabled(i);
        button(r, button_text[i], hot == HIT_BUTTON + i, on, i == B_DELETE && on ? C_ERR : 0);
    }
    /* The path bar. */
    struct rect pr = path_rect();
    gfx_round_rect(s, pr.x, pr.y, pr.w, pr.h, 8, C_FIELD, 255);
    gfx_round_outline(s, pr.x, pr.y, pr.w, pr.h, 8, C_ACCENT, 50);
    layout_crumbs();
    int sep_x = 0;
    if (crumbs_cut) {
        gfx_text(s, pr.x + 10, pr.y + 6, "..", C_DIM, FONT_SMALL);
        sep_x = pr.x + 10 + gfx_text_width("..", FONT_SMALL);
    }
    for (int i = crumbs_cut; i < ncrumbs; i++) {
        struct rect r = crumbs[i].r;
        if (sep_x) gfx_text(s, sep_x + 7, pr.y + 6, ">", C_FAINT, FONT_SMALL);
        int last = i == ncrumbs - 1;
        if (hot == HIT_CRUMB + i) gfx_round_rect(s, r.x, r.y, r.w, r.h, 6, C_BUTTON_HOT, 255);
        gfx_text(s, r.x + 4, pr.y + 6, crumbs[i].name, last ? C_FG : C_DIM, FONT_SMALL);
        sep_x = r.x + r.w - 4;
    }
}

static void draw_sidebar(void) {
    struct surface *s = &win.s;
    int y0 = TOOL_H, h = wh - TOOL_H - STATUS_H;
    gfx_fill(s, 0, y0, SIDE_W, h, C_SIDE);
    gfx_blend(s, SIDE_W - 1, y0, 1, h, 0xFFFFFF, 14);
    gfx_text(s, 20, y0 + 12, "PLACES", C_FAINT, FONT_SMALL);
    for (int i = 0; i < NPLACES; i++) {
        struct rect r = place_rect(i);
        /* The place we're in: the longest one that contains cwd. */
        int here = path_within(cwd, places[i].path) && (i == 2 ? !path_within(cwd, "/home") && !path_within(cwd, "/apps") : 1);
        if (here) gfx_round_rect(s, r.x, r.y, r.w, r.h, 8, C_SEL, 255);
        else if (hot == HIT_PLACE + i) gfx_round_rect(s, r.x, r.y, r.w, r.h, 8, C_ROW_HOT, 255);
        if (here) gfx_fill(s, r.x, r.y + 6, 3, r.h - 12, C_ACCENT);
        icon_folder(r.x + 12, r.y + 5, i == 0 ? C_FOLDER : i == 1 ? 0xB794FF : 0x8C96B8);
        gfx_text(s, r.x + 40, r.y + 6, places[i].name, here ? C_FG : C_DIM, FONT_SMALL);
    }

    /* Storage, then what Files itself holds: small and at the bottom. */
    int y = y0 + h - 132;
    gfx_text(s, 20, y, "STORAGE", C_FAINT, FONT_SMALL);
    const char *kind = fsi.kind == FS_ENCRYPTED ? "Encrypted" : fsi.kind == FS_TEMPORARY ? "Temporary" : "Locked";
    gfx_text(s, 20, y + 20, kind, fsi.kind == FS_ENCRYPTED ? C_OK : C_WARN, FONT_SMALL);
    if (fsi.total_blocks) {
        char free_s[24], line[40];
        format_size(fsi.free_blocks * fsi.block_size, free_s, sizeof free_s);
        snprintf(line, sizeof line, "%s free", free_s);
        gfx_text(s, 20, y + 38, line, C_DIM, FONT_SMALL);
        int bw = SIDE_W - 40, used = (int)((fsi.total_blocks - fsi.free_blocks) * (uint64_t)bw / fsi.total_blocks);
        gfx_round_rect(s, 20, y + 58, bw, 4, 2, 0x1D2647, 255);
        gfx_round_rect(s, 20, y + 58, MAX(used, 4), 4, 2, C_ACCENT, 200);
    }
    gfx_text(s, 20, y + 76, "FILES HOLDS", C_FAINT, FONT_SMALL);
    chip(20, y + 96, label_name(fsi.taint), label_colors[fsi.taint & 3]);
}

static void draw_list(void) {
    struct surface *s = &win.s;
    struct rect l = list_rect();
    gfx_fill(s, l.x, l.y, l.w, l.h, C_BG);
    /* Column header. */
    int xs = l.x + l.w - 16 - 116 - 110 - 72, xm = l.x + l.w - 16 - 116 - 110 + 12, xl = l.x + l.w - 16 - 116;
    int ty = l.y + 8;
    gfx_text(s, l.x + 46, ty, "Name", C_FAINT, FONT_SMALL);
    gfx_text(s, xs + 72 - gfx_text_width("Size", FONT_SMALL), ty, "Size", C_FAINT, FONT_SMALL);
    gfx_text(s, xm, ty, "Modified", C_FAINT, FONT_SMALL);
    gfx_text(s, xl, ty, "Label", C_FAINT, FONT_SMALL);
    gfx_blend(s, l.x + 12, l.y + HEAD_H, l.w - 24, 1, 0xFFFFFF, 18);

    struct rect save = s->clip;
    gfx_set_clip(s, (struct rect){l.x, l.y + HEAD_H + 1, l.w, l.h - HEAD_H - 1});
    if (!nents) {
        const char *msg = "This folder is empty";
        gfx_text(s, l.x + (l.w - gfx_text_width(msg, FONT_SMALL)) / 2, l.y + 90, msg, C_DIM, FONT_SMALL);
        msg = "Ctrl+N makes a folder; Ctrl+V pastes";
        gfx_text(s, l.x + (l.w - gfx_text_width(msg, FONT_SMALL)) / 2, l.y + 112, msg, C_FAINT, FONT_SMALL);
    }
    for (int i = top; i < nents && i < top + visible_rows() + 1; i++) {
        struct entry *e = &ents[i];
        struct rect r = row_rect(i);
        if (i == sel) {
            gfx_round_rect(s, r.x, r.y + 1, r.w, r.h - 2, 8, C_SEL, 255);
            gfx_round_outline(s, r.x, r.y + 1, r.w, r.h - 2, 8, C_ACCENT, 90);
        } else if (hot == HIT_ROW + i) {
            gfx_round_rect(s, r.x, r.y + 1, r.w, r.h - 2, 8, C_ROW_HOT, 255);
        }
        int cut = clip[0] && clip_cut && !strcmp(base_name(clip), e->name) && path_within(clip, cwd) &&
                  strlen(clip) == strlen(cwd) + (strcmp(cwd, "/") ? 1 : 0) + strlen(e->name);
        uint32_t fg = cut ? C_DIM : C_FG;
        if (e->type == FT_DIR) icon_folder(r.x + 12, r.y + 5, cut ? C_FAINT : C_FOLDER);
        else icon_file(r.x + 12, r.y + 5, name_is_text(e->name), label_colors[e->label & 3]);
        char buf[NAME_MAX + 8];
        fit(buf, sizeof buf, e->name, xs - (r.x + 38) - 8);
        gfx_text(s, r.x + 38, r.y + 6, buf, fg, FONT_SMALL);
        if (e->type == FT_FILE) {
            format_size(e->size, buf, sizeof buf);
            gfx_text(s, xs + 72 - gfx_text_width(buf, FONT_SMALL), r.y + 6, buf, C_DIM, FONT_SMALL);
        } else {
            gfx_text(s, xs + 72 - gfx_text_width("--", FONT_SMALL), r.y + 6, "--", C_FAINT, FONT_SMALL);
        }
        format_time(e->mtime, buf, sizeof buf);
        gfx_text(s, xm, r.y + 6, buf, C_DIM, FONT_SMALL);
        chip(xl, r.y + 4, label_name(e->label), label_colors[e->label & 3]);
    }
    s->clip = save;
    /* Scroll position, when the folder is longer than the view. */
    int rows = visible_rows();
    if (nents > rows) {
        int th = l.h - HEAD_H - 12, bh = MAX(24, th * rows / nents);
        int by = l.y + HEAD_H + 6 + (th - bh) * top / MAX(1, nents - rows);
        gfx_round_rect(s, l.x + l.w - 6, by, 3, bh, 1, C_ACCENT, 90);
    }
}

/* Draws `text` wrapped into the box; skips the first `skip` wrapped lines. Returns the number of lines. */
static int draw_wrapped(const char *text, int len, struct rect box, int skip, uint32_t color) {
    struct surface *s = &win.s;
    int cols = MAX(1, box.w / gfx_font_w(FONT_SMALL)), rows = box.h / 18, line = 0;
    char buf[256];
    for (int pos = 0; pos < len;) {
        int n = 0;
        while (pos + n < len && text[pos + n] != '\n' && n < cols && n < (int)sizeof buf - 1) n++;
        if (line >= skip && line < skip + rows) {
            for (int i = 0; i < n; i++) {
                char c = text[pos + i];
                buf[i] = c == '\t' ? ' ' : ((unsigned char)c < 32 ? ' ' : c);
            }
            buf[n] = 0;
            gfx_text(s, box.x, box.y + (line - skip) * 18, buf, color, FONT_SMALL);
        }
        line++;
        pos += n;
        if (pos < len && text[pos] == '\n') pos++;
    }
    return line;
}

static void draw_preview(void) {
    struct surface *s = &win.s;
    struct rect p = prev_rect();
    gfx_fill(s, p.x, p.y, p.w, p.h, C_SIDE);
    gfx_blend(s, p.x, p.y, 1, p.h, 0xFFFFFF, 14);
    struct entry *e = selected();
    if (!e) {
        gfx_text(s, p.x + 16, p.y + 20, "Nothing selected", C_FAINT, FONT_SMALL);
        return;
    }
    /* A big icon, the name and the facts. */
    int x = p.x + 16, y = p.y + 18;
    gfx_round_rect(s, x, y, 48, 48, 12, 0x141C36, 255);
    gfx_round_outline(s, x, y, 48, 48, 12, label_colors[e->label & 3], 120);
    if (e->type == FT_DIR) icon_folder(x + 15, y + 15, C_FOLDER);
    else icon_file(x + 15, y + 15, name_is_text(e->name), label_colors[e->label & 3]);
    char buf[NAME_MAX + 8];
    fit(buf, sizeof buf, e->name, p.w - 32 - 60);
    gfx_text(s, x + 60, y + 6, buf, C_FG, FONT_SMALL);
    gfx_text(s, x + 60, y + 26, e->type == FT_DIR ? "Folder" : name_is_text(e->name) ? "Text file" : "File", C_DIM,
             FONT_SMALL);
    y += 66;
    gfx_text(s, x, y, "Label", C_FAINT, FONT_SMALL);
    chip(x + 80, y - 2, label_name(e->label), label_colors[e->label & 3]);
    y += 26;
    if (e->type == FT_FILE) {
        format_size(e->size, buf, sizeof buf);
        gfx_text(s, x, y, "Size", C_FAINT, FONT_SMALL);
        gfx_text(s, x + 80, y, buf, C_FG, FONT_SMALL);
        y += 22;
    }
    format_time(e->mtime, buf, sizeof buf);
    gfx_text(s, x, y, "Modified", C_FAINT, FONT_SMALL);
    gfx_text(s, x + 80, y, buf, C_FG, FONT_SMALL);
    y += 30;
    gfx_blend(s, x, y, p.w - 32, 1, 0xFFFFFF, 18);
    y += 12;

    if (e->type == FT_DIR) {
        gfx_text(s, x, y, "Enter or double-click", C_DIM, FONT_SMALL);
        gfx_text(s, x, y + 18, "to open it.", C_DIM, FONT_SMALL);
        return;
    }
    if (snippet_state == SN_ASK) {
        /* Reading it would raise our own label: say so and let the user decide. */
        snprintf(buf, sizeof buf, "This file is %s.", label_name(e->label));
        gfx_text(s, x, y, buf, label_colors[e->label & 3], FONT_SMALL);
        gfx_text(s, x, y + 20, "Reading it marks Files as", C_DIM, FONT_SMALL);
        snprintf(buf, sizeof buf, "%s until it closes.", label_name(e->label));
        gfx_text(s, x, y + 38, buf, C_DIM, FONT_SMALL);
        button(preview_button_rect(), "Show preview (Space)", hot == HIT_PREVIEW, 1, C_ACCENT);
    } else if (snippet_state == SN_TEXT) {
        struct rect box = {x, y, p.w - 32, p.y + p.h - y - 10};
        gfx_round_rect(s, box.x - 6, box.y - 6, box.w + 12, box.h + 6, 8, C_FIELD, 255);
        struct rect save = s->clip;
        gfx_set_clip(s, box);
        draw_wrapped(snippet, snippet_len, box, 0, 0xB8C2E0);
        s->clip = save;
    } else if (snippet_state == SN_BINARY) {
        gfx_text(s, x, y, "Not a text file:", C_DIM, FONT_SMALL);
        gfx_text(s, x, y + 18, "no preview.", C_DIM, FONT_SMALL);
    } else if (snippet_state == SN_ERROR) {
        gfx_text(s, x, y, "Couldn't read it.", C_ERR, FONT_SMALL);
    } else if (!e->size) {
        gfx_text(s, x, y, "Empty file.", C_DIM, FONT_SMALL);
    }
}

static void draw_viewer(void) {
    struct surface *s = &win.s;
    struct rect v = view_rect();
    gfx_fill(s, v.x, v.y, v.w, v.h, C_BG);
    gfx_fill(s, v.x, v.y, v.w, 48, C_SIDE);
    gfx_blend(s, v.x, v.y + 47, v.w, 1, 0xFFFFFF, 18);
    icon_file(v.x + 16, v.y + 15, 1, label_colors[view_label & 3]);
    int x = gfx_text(s, v.x + 44, v.y + 16, view_name, C_FG, FONT_SMALL) + 12;
    x += chip(x, v.y + 14, label_name((uint32_t)view_label), label_colors[view_label & 3]) + 6;
    chip(x, v.y + 14, "read-only", C_ACCENT);
    struct rect c = view_close_rect();
    button(c, "Close", hot == HIT_VIEW_CLOSE, 1, 0);
    struct rect box = {v.x + 20, v.y + 62, v.w - 40, v.h - 62 - 12};
    struct rect save = s->clip;
    gfx_set_clip(s, box);
    if (view_binary) {
        gfx_text(s, box.x, box.y, "This file isn't text, so there's nothing to show.", C_DIM, FONT_SMALL);
    } else {
        int lines = draw_wrapped(view_text, view_len, box, view_top, C_FG);
        int rows = box.h / 18;
        if (view_top > MAX(0, lines - rows)) view_top = MAX(0, lines - rows);
        if (view_more && view_top + rows >= lines)
            gfx_text(s, box.x, box.y + (lines - view_top) * 18 + 6, "(showing the first 64 KB)", C_DIM, FONT_SMALL);
    }
    s->clip = save;
}

/*
 * Dialogs are redrawn on every keystroke, and a full window with a soft
 * shadow is slow to paint without a GPU. So the dimmed window and the shadow
 * are painted once, the pixels under the box are kept, and later frames put
 * them back and draw only the box.
 */
#define MODAL_MAX_W 460
#define MODAL_MAX_H 210
static uint32_t modal_under[MODAL_MAX_W * MODAL_MAX_H];

static void draw_modal(void) {
    struct surface *s = &win.s;
    struct rect m = modal_rect();
    if (!modal_backdrop) {
        gfx_blend(s, 0, 0, ww, wh, 0x03050C, 140);
        gfx_shadow(s, m.x, m.y, m.w, m.h, 16, 16, 150);
        for (int j = 0; j < m.h; j++) memcpy(&modal_under[j * m.w], &s->px[(long)(m.y + j) * s->pitch + m.x], (size_t)m.w * 4);
        modal_backdrop = 1;
    } else {
        for (int j = 0; j < m.h; j++) memcpy(&s->px[(long)(m.y + j) * s->pitch + m.x], &modal_under[j * m.w], (size_t)m.w * 4);
    }
    gfx_round_rect(s, m.x, m.y, m.w, m.h, 16, 0x0F1630, 250);
    gfx_blend(s, m.x + 16, m.y, m.w - 32, 1, 0xFFFFFF, 40);
    gfx_round_outline(s, m.x, m.y, m.w, m.h, 16, modal == M_DELETE ? C_ERR : C_ACCENT, 110);
    struct entry *e = selected();
    char title[NAME_MAX + 32];
    const char *ok = "OK";
    if (modal == M_NEWDIR) strlcpy(title, "New folder", sizeof title), ok = "Create";
    else if (modal == M_RENAME) strlcpy(title, "Rename", sizeof title), ok = "Rename";
    else if (modal == M_DELETE) snprintf(title, sizeof title, "Delete %s?", e && e->type == FT_DIR ? "this folder" : "this file"), ok = "Delete";
    else snprintf(title, sizeof title, "Label for %s", e ? e->name : "");
    char t[64];
    fit(t, sizeof t, title, m.w - 48);
    gfx_text(s, m.x + 24, m.y + 22, t, C_FG, FONT_MEDIUM);

    if (modal == M_NEWDIR || modal == M_RENAME) {
        struct rect f = {m.x + 24, m.y + 66, m.w - 48, 34};
        gfx_round_rect(s, f.x, f.y, f.w, f.h, 8, C_FIELD, 255);
        gfx_round_outline(s, f.x, f.y, f.w, f.h, 8, C_ACCENT, 160);
        int cx = gfx_text(s, f.x + 10, f.y + 9, field, C_FG, FONT_SMALL);
        gfx_fill(s, cx + 1, f.y + 8, 2, 18, C_ACCENT);
        if (modal_msg[0][0]) gfx_text(s, m.x + 24, m.y + 108, modal_msg[0], C_DIM, FONT_SMALL);
    } else if (modal == M_DELETE) {
        gfx_text(s, m.x + 24, m.y + 64, modal_msg[0], C_FG, FONT_SMALL);
        gfx_text(s, m.x + 24, m.y + 84, modal_msg[1][0] ? modal_msg[1] : "This can't be undone.", modal_msg[1][0] ? C_WARN : C_DIM,
                 FONT_SMALL);
    } else if (modal == M_LABEL && e) {
        for (int k = 0; k < LABEL_COUNT; k++) {
            struct rect b = modal_button(10 + k);
            int on = modal_choice == k;
            uint32_t c = label_colors[k];
            gfx_round_rect(s, b.x, b.y, b.w, b.h, 10, c, on ? 70 : hot == HIT_MODAL + 10 + k ? 40 : 18);
            gfx_round_outline(s, b.x, b.y, b.w, b.h, 10, c, on ? 230 : 110);
            gfx_text(s, b.x + (b.w - gfx_text_width(label_name((uint32_t)k), FONT_SMALL)) / 2, b.y + 9,
                     label_name((uint32_t)k), c, FONT_SMALL);
        }
        const char *hint = modal_choice < (int)e->label ? "Lowering asks you in a system dialog first."
                         : modal_choice > (int)e->label ? "Raising a label is always allowed."
                                                        : "Pick a label: arrows or 1-4, then Enter.";
        gfx_text(s, m.x + 24, m.y + 64, "Raising is free; lowering asks you.", C_DIM, FONT_SMALL);
        gfx_text(s, m.x + 24, m.y + 138, hint, modal_choice < (int)e->label ? C_WARN : C_DIM, FONT_SMALL);
    }
    if (modal == M_NEWDIR && modal_msg[1][0]) {
        char line[64];
        fit(line, sizeof line, modal_msg[1], m.w - 48);
        gfx_text(s, m.x + 24, m.y + 126, line, C_WARN, FONT_SMALL);
    }
    button(modal_button(0), "Cancel", hot == HIT_MODAL, 1, 0);
    button(modal_button(1), modal == M_LABEL ? "Apply" : ok, hot == HIT_MODAL + 1, 1, modal == M_DELETE ? C_ERR : C_ACCENT);
}

static void draw(void) {
    struct surface *s = &win.s;
    s->clip = (struct rect){0, 0, ww, wh};
    if (modal && modal_backdrop) { /* only the dialog box changes */
        draw_modal();
        window_present(&win);
        return;
    }
    draw_toolbar();
    draw_sidebar();
    if (view_open) {
        draw_viewer();
    } else {
        draw_list();
        draw_preview();
    }
    /* Status line: the last thing that happened, the item count, the clipboard. */
    int sy = wh - STATUS_H;
    gfx_fill(s, 0, sy, ww, STATUS_H, C_BAR);
    gfx_blend(s, 0, sy, ww, 1, 0xFFFFFF, 18);
    char right[96];
    int n = snprintf(right, sizeof right, "%d item%s%s", nents, nents == 1 ? "" : "s", truncated ? " (first 256)" : "");
    if (clip[0]) snprintf(right + n, sizeof right - (size_t)n, "   %s: %s", clip_cut ? "cut" : "copied", base_name(clip));
    char fitted[96];
    fit(fitted, sizeof fitted, right, 300);
    int rw = gfx_text_width(fitted, FONT_SMALL);
    gfx_text(s, ww - PAD - rw, sy + 7, fitted, C_DIM, FONT_SMALL);
    char msg[128];
    fit(msg, sizeof msg, status[0] ? status : "Enter opens   Backspace goes up   Del deletes   Ctrl+C/X/V copy, cut, paste",
        ww - 2 * PAD - rw - 20);
    gfx_text(s, PAD, sy + 7, msg, status[0] ? status_color : C_FAINT, FONT_SMALL);
    if (modal) draw_modal();
    window_present(&win);
}

/* ---- Input ------------------------------------------------------------------------- */

static void press_button(int i) {
    if (!button_enabled(i)) return;
    switch (i) {
    case B_UP: go_up(); break;
    case B_NEWDIR: open_modal(M_NEWDIR); break;
    case B_RENAME: open_modal(M_RENAME); break;
    case B_DELETE: open_modal(M_DELETE); break;
    case B_COPY: do_copy(0); break;
    case B_CUT: do_copy(1); break;
    case B_PASTE: do_paste(); break;
    case B_LABEL: open_modal(M_LABEL); break;
    }
}

static void modal_key(uint32_t k) {
    if (k == 27) return close_modal();
    if (k == '\n') return confirm_modal();
    if (modal == M_LABEL) {
        if (k == KEY_LEFT || k == KEY_UP) modal_choice = MAX(0, modal_choice - 1);
        else if (k == KEY_RIGHT || k == KEY_DOWN) modal_choice = MIN(LABEL_COUNT - 1, modal_choice + 1);
        else if (k >= '1' && k <= '4') modal_choice = (int)(k - '1');
        return;
    }
    if (modal == M_NEWDIR || modal == M_RENAME) {
        if (k == '\b' && field_len) field[--field_len] = 0;
        else if (k >= 32 && k < 127 && k != '/' && field_len < NAME_MAX) field[field_len++] = (char)k, field[field_len] = 0;
    }
}

static void key(uint32_t k, uint32_t mods) {
    if (modal) return modal_key(k);
    if (view_open) {
        if (k == 27 || k == '\b' || k == 'q') view_open = 0, status[0] = 0;
        else if (k == KEY_DOWN) view_top++;
        else if (k == KEY_UP) view_top = MAX(0, view_top - 1);
        else if (k == KEY_PGDN || k == ' ') view_top += 10;
        else if (k == KEY_PGUP) view_top = MAX(0, view_top - 10);
        else if (k == KEY_HOME) view_top = 0;
        return;
    }
    if (mods & MOD_CTRL) {
        switch (lower((int)k)) {
        case 'n': open_modal(M_NEWDIR); break;
        case 'r': press_button(B_RENAME); break;
        case 'c': press_button(B_COPY); break;
        case 'x': press_button(B_CUT); break;
        case 'v': do_paste(); break;
        case 'l': press_button(B_LABEL); break;
        }
        return;
    }
    int rows = visible_rows();
    switch (k) {
    case KEY_UP: select_row(sel - 1); break;
    case KEY_DOWN: select_row(sel + 1); break;
    case KEY_PGUP: select_row(sel - rows); break;
    case KEY_PGDN: select_row(sel + rows); break;
    case KEY_HOME: select_row(0); break;
    case KEY_END: select_row(nents - 1); break;
    case '\n': open_selected(); break;
    case '\b': go_up(); break;
    case KEY_DELETE: press_button(B_DELETE); break;
    case ' ':
        if (snippet_state == SN_ASK) load_snippet(1);
        break;
    default:
        /* Type-ahead: jump to the next name starting with that letter. */
        if (k > ' ' && k < 127)
            for (int i = 1; i <= nents; i++) {
                int j = (sel + i) % nents;
                if (lower(ents[j].name[0]) == lower((int)k)) {
                    select_row(j);
                    break;
                }
            }
        break;
    }
}

static void click(int x, int y) {
    int h = hit_test(x, y);
    uint64_t t = now_ms();
    if (modal) {
        if (h == HIT_MODAL) close_modal();
        else if (h == HIT_MODAL + 1) confirm_modal();
        else if (h >= HIT_MODAL + 10) modal_choice = h - HIT_MODAL - 10;
        return;
    }
    if (h >= HIT_BUTTON && h < HIT_BUTTON + NBUTTONS) {
        if (view_open && h == HIT_BUTTON + B_UP) view_open = 0; /* Up leaves the viewer first */
        else press_button(h - HIT_BUTTON);
    } else if (h >= HIT_CRUMB && h < HIT_CRUMB + MAX_CRUMBS) {
        view_open = 0;
        char path[PATH_MAX];
        strlcpy(path, crumbs[h - HIT_CRUMB].path, sizeof path);
        if (strcmp(path, cwd)) go(path, 0);
    } else if (h >= HIT_PLACE && h < HIT_PLACE + NPLACES) {
        view_open = 0;
        go(places[h - HIT_PLACE].path, 0);
    } else if (h == HIT_VIEW_CLOSE) {
        view_open = 0;
    } else if (h == HIT_PREVIEW) {
        load_snippet(1);
    } else if (h >= HIT_ROW && h < HIT_PREVIEW) {
        int row = h - HIT_ROW;
        int dbl = row == last_click_row && t - last_click_ms < DOUBLE_CLICK_MS;
        if (row != sel) select_row(row);
        if (dbl) {
            open_selected();
            last_click_row = -1;
            return;
        }
        last_click_row = row;
        last_click_ms = t;
    }
}

/* Handles one event; returns 1 if the window needs drawing. */
static int handle(const struct event *ev) {
    if (ev->type == EV_KEY) {
        status[0] = 0;
        key(ev->key, ev->mods);
    } else if (ev->type == EV_MOUSE_MOVE) {
        int h = hit_test(ev->x, ev->y);
        if (h == hot) return 0;
        hot = h;
    } else if (ev->type == EV_MOUSE_DOWN) {
        if (!(ev->buttons & 1)) return 0;
        status[0] = 0;
        click(ev->x, ev->y);
        hot = hit_test(ev->x, ev->y);
    } else if (ev->type == EV_SCROLL) {
        int notches = (int32_t)ev->key;
        if (view_open) view_top = MAX(0, view_top + notches * 3);
        else top = MAX(0, MIN(top + notches * 3, nents - visible_rows()));
        hot = hit_test(ev->x, ev->y);
    } else if (ev->type == EV_CLOSE) {
        sys_exit(0);
    } else if (ev->type == EV_RESIZE) {
        if (window_resize(&win)) return 0;
        ww = win.s.w, wh = win.s.h;
        top = MAX(0, MIN(top, nents - visible_rows()));
    } else if (ev->type != EV_FOCUS) {
        return 0;
    }
    return 1;
}

int main(void) {
    struct display_info di;
    int w = W, h = H;
    if (sys_display_info(&di) == 0) w = MIN(w, (int)di.max_client_w), h = MIN(h, (int)di.max_client_h);
    if (window_open(&win, w, h, "Files") < 0) return 1;
    ww = win.s.w, wh = win.s.h;
    window_set_resizable(&win, 1, SIDE_W + PREV_W + 240, 320);
    refresh_fs_info();
    go("/home", 0);
    draw();
    for (;;) {
        struct event ev;
        if (sys_event_wait(&ev, -1) <= 0) continue;
        int dirty = handle(&ev);
        /* Take everything already queued (fast typing, mouse moves) before drawing once. */
        while (sys_event_wait(&ev, 0) > 0) dirty |= handle(&ev);
        if (dirty) draw();
    }
}
