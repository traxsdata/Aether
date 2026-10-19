/*
 * calculator.c - the æther calculator. Mouse or keyboard:
 *   digits . + - * / % Enter(=) Backspace Esc(C)
 * Runs with a single capability: CAP_WINDOW.
 */
#include "lib/ui.h"

#define W 312
#define H 452
#define DISPLAY_H 128
#define GAP 10
#define COLS 4
#define ROWS 5

#define C_BG 0x0D1222
#define C_KEY 0x1A2240
#define C_KEY_HOT 0x253058
#define C_OP 0x6D3BE8
#define C_OP_HOT 0x8457FF
#define C_FN 0x152039
#define C_EQ 0x14B8A6
#define C_TEXT 0xEEF2FF
#define C_DIM 0x7C87A8

struct key {
    const char *label;
    char code; /* what the key does, as a keyboard character */
    int col, row, span;
    uint32_t color, hot;
};

static const struct key keys[] = {
    {"C", 'c', 0, 0, 1, C_FN, C_KEY_HOT},   {"+/-", 'n', 1, 0, 1, C_FN, C_KEY_HOT},
    {"%", '%', 2, 0, 1, C_FN, C_KEY_HOT},   {"/", '/', 3, 0, 1, C_OP, C_OP_HOT},
    {"7", '7', 0, 1, 1, C_KEY, C_KEY_HOT},  {"8", '8', 1, 1, 1, C_KEY, C_KEY_HOT},
    {"9", '9', 2, 1, 1, C_KEY, C_KEY_HOT},  {"x", '*', 3, 1, 1, C_OP, C_OP_HOT},
    {"4", '4', 0, 2, 1, C_KEY, C_KEY_HOT},  {"5", '5', 1, 2, 1, C_KEY, C_KEY_HOT},
    {"6", '6', 2, 2, 1, C_KEY, C_KEY_HOT},  {"-", '-', 3, 2, 1, C_OP, C_OP_HOT},
    {"1", '1', 0, 3, 1, C_KEY, C_KEY_HOT},  {"2", '2', 1, 3, 1, C_KEY, C_KEY_HOT},
    {"3", '3', 2, 3, 1, C_KEY, C_KEY_HOT},  {"+", '+', 3, 3, 1, C_OP, C_OP_HOT},
    {"0", '0', 0, 4, 2, C_KEY, C_KEY_HOT},  {".", '.', 2, 4, 1, C_KEY, C_KEY_HOT},
    {"=", '=', 3, 4, 1, C_EQ, 0x2DD4BF},
};
#define NKEYS (int)(sizeof keys / sizeof keys[0])

static struct window win;
static int hover = -1, pressed = -1;

/* Calculator state: an entry being typed, an accumulator and a pending operator. */
static char entry[24] = "0";
static double acc;
static char op;          /* pending operator, 0 if none */
static int fresh = 1;    /* next digit starts a new entry */
static int error;
static char history[48]; /* the expression shown above the result */

/* ---- Number formatting (no libc here: done by hand) ---------------------- */

static void format_number(double v, char *out, int size) {
    if (v != v || v > 1e300 || v < -1e300) {
        strlcpy(out, "Error", (size_t)size);
        return;
    }
    char buf[40];
    int n = 0;
    if (v < 0) buf[n++] = '-', v = -v;
    if (v != 0 && (v >= 1e12 || v < 1e-6)) { /* scientific */
        int e = 0;
        while (v >= 10) v /= 10, e++;
        while (v < 1) v *= 10, e--;
        uint64_t m = (uint64_t)(v * 1e6 + 0.5); /* 7 significant digits */
        if (m >= 10000000) m /= 10, e++;
        buf[n++] = (char)('0' + m / 1000000);
        buf[n++] = '.';
        for (uint64_t d = 100000; d; d /= 10) buf[n++] = (char)('0' + (m / d) % 10);
        while (buf[n - 1] == '0') n--;
        if (buf[n - 1] == '.') n--;
        n += snprintf(buf + n, sizeof buf - (size_t)n, "e%d", e);
        buf[n] = 0;
        strlcpy(out, buf, (size_t)size);
        return;
    }
    /* Fixed point with up to 12 significant digits. */
    uint64_t ip = (uint64_t)v;
    int idigits = 1;
    for (uint64_t t = ip; t >= 10; t /= 10) idigits++;
    int fdigits = 12 - idigits;
    if (fdigits < 0) fdigits = 0;
    uint64_t scale = 1;
    for (int i = 0; i < fdigits; i++) scale *= 10;
    uint64_t frac = (uint64_t)((v - (double)ip) * (double)scale + 0.5);
    if (frac >= scale) ip++, frac -= scale;
    n += snprintf(buf + n, sizeof buf - (size_t)n, "%lu", ip);
    if (frac && fdigits) {
        buf[n++] = '.';
        for (uint64_t d = scale / 10; d; d /= 10) buf[n++] = (char)('0' + (frac / d) % 10);
        while (buf[n - 1] == '0') n--;
    }
    buf[n] = 0;
    strlcpy(out, buf, (size_t)size);
}

static double parse_number(const char *s) {
    double v = 0, scale = 1;
    int neg = *s == '-', frac = 0;
    if (neg) s++;
    for (; *s; s++) {
        if (*s == '.') {
            frac = 1;
        } else if (*s == 'e' || *s == 'E') {
            /* Results are shown in scientific notation; a following operation
             * has to read that form back (1e13 is not "1 times junk"). */
            int eneg = 0, exp = 0;
            s++;
            if (*s == '+' || *s == '-') eneg = *s++ == '-';
            while (*s >= '0' && *s <= '9') {
                if (exp < 100000) exp = exp * 10 + (*s - '0');
                s++;
            }
            for (int i = 0; i < exp && i < 400; i++) v = eneg ? v / 10 : v * 10;
            break;
        } else if (*s >= '0' && *s <= '9') {
            if (frac) scale /= 10, v += (*s - '0') * scale;
            else v = v * 10 + (*s - '0');
        }
    }
    return neg ? -v : v;
}

/* ---- Logic ----------------------------------------------------------------- */

static char op_symbol(char o) { return o == '*' ? 'x' : o; }

static double apply(double a, char o, double b) {
    switch (o) {
    case '+': return a + b;
    case '-': return a - b;
    case '*': return a * b;
    case '/':
        if (b == 0) error = 1;
        return b == 0 ? 0 : a / b;
    }
    return b;
}

static void clear_all(void) {
    strlcpy(entry, "0", sizeof entry);
    acc = 0;
    op = 0;
    fresh = 1;
    error = 0;
    history[0] = 0;
}

static void press(char c) {
    if (error && c != 'c') clear_all();
    int len = (int)strlen(entry);
    if (c >= '0' && c <= '9') {
        if (fresh || !strcmp(entry, "0")) entry[0] = 0, len = 0, fresh = 0;
        if (len < 15) entry[len] = c, entry[len + 1] = 0;
    } else if (c == '.') {
        if (fresh) strlcpy(entry, "0", sizeof entry), fresh = 0, len = 1;
        int has_dot = 0;
        for (int i = 0; entry[i]; i++) has_dot |= entry[i] == '.';
        if (!has_dot && len < 15) entry[len] = '.', entry[len + 1] = 0;
    } else if (c == '\b') {
        if (!fresh && len > 1) entry[len - 1] = 0;
        else strlcpy(entry, "0", sizeof entry);
    } else if (c == 'c') {
        clear_all();
    } else if (c == 'n') {
        double v = -parse_number(entry);
        format_number(v, entry, sizeof entry);
    } else if (c == '%') {
        double v = parse_number(entry) / 100;
        if (op == '+' || op == '-') v *= acc; /* 200 + 10% = 220 */
        format_number(v, entry, sizeof entry);
        fresh = 1;
    } else if (c == '+' || c == '-' || c == '*' || c == '/') {
        double v = parse_number(entry);
        if (op && !fresh) acc = apply(acc, op, v);
        else if (!op) acc = v;
        op = c;
        fresh = 1;
        char a[24];
        format_number(acc, a, sizeof a);
        format_number(acc, entry, sizeof entry);
        snprintf(history, sizeof history, "%s %c", a, op_symbol(op));
    } else if (c == '=' || c == '\n') {
        if (!op) return;
        double v = parse_number(entry);
        char a[24], b[24];
        format_number(acc, a, sizeof a);
        format_number(v, b, sizeof b);
        double r = apply(acc, op, v);
        snprintf(history, sizeof history, "%s %c %s =", a, op_symbol(op), b);
        if (error) {
            strlcpy(entry, "Can't divide by 0", sizeof entry);
        } else {
            format_number(r, entry, sizeof entry);
        }
        acc = r;
        op = 0;
        fresh = 1;
    }
}

/* ---- Drawing ---------------------------------------------------------------- */

static struct rect key_rect(const struct key *k) {
    int kw = (W - GAP * (COLS + 1)) / COLS;
    int kh = (H - DISPLAY_H - GAP * (ROWS + 1)) / ROWS;
    return (struct rect){GAP + k->col * (kw + GAP), DISPLAY_H + GAP + k->row * (kh + GAP), kw * k->span + GAP * (k->span - 1), kh};
}

static void draw(void) {
    struct surface *s = &win.s;
    gfx_fill(s, 0, 0, s->w, s->h, C_BG);

    /* Display: expression on top, value large and right-aligned. */
    gfx_round_rect(s, GAP, GAP, W - 2 * GAP, DISPLAY_H - GAP, 14, 0x070A16, 255);
    gfx_round_outline(s, GAP, GAP, W - 2 * GAP, DISPLAY_H - GAP, 14, 0x38E1F5, 50);
    gfx_text(s, W - 2 * GAP - 8 - gfx_text_width(history, FONT_SMALL), GAP + 14, history, C_DIM, FONT_SMALL);
    enum font_size f = gfx_text_width(entry, FONT_LARGE) <= W - 4 * GAP ? FONT_LARGE : FONT_MEDIUM;
    int tw = gfx_text_width(entry, f);
    int ty = f == FONT_LARGE ? GAP + 54 : GAP + 66;
    gfx_text(s, W - 2 * GAP - 8 - tw, ty, entry, error ? 0xFF5C7A : C_TEXT, f);

    for (int i = 0; i < NKEYS; i++) {
        const struct key *k = &keys[i];
        struct rect r = key_rect(k);
        uint32_t c = i == pressed ? gfx_mix(k->hot, 0xFFFFFF, 40) : i == hover ? k->hot : k->color;
        gfx_round_rect(s, r.x, r.y, r.w, r.h, 14, c, 255);
        gfx_blend(s, r.x + 10, r.y + 1, r.w - 20, 1, 0xFFFFFF, 30); /* top highlight */
        if (op && k->code == op && fresh) gfx_round_outline(s, r.x, r.y, r.w, r.h, 14, 0xFFFFFF, 160);
        int lw = gfx_text_width(k->label, FONT_MEDIUM);
        gfx_text(s, r.x + (r.w - lw) / 2, r.y + (r.h - gfx_font_h(FONT_MEDIUM)) / 2, k->label, C_TEXT, FONT_MEDIUM);
    }
    window_present(&win);
}

static int key_at(int x, int y) {
    for (int i = 0; i < NKEYS; i++)
        if (rect_contains(key_rect(&keys[i]), x, y)) return i;
    return -1;
}

int main(void) {
    if (window_open(&win, W, H, "Calculator") < 0) return 1;
    draw();
    for (;;) {
        struct event ev;
        sys_event_wait(&ev, -1);
        switch (ev.type) {
        case EV_MOUSE_MOVE: {
            int h = key_at(ev.x, ev.y);
            if (h == hover) continue;
            hover = h;
            break;
        }
        case EV_MOUSE_DOWN:
            pressed = key_at(ev.x, ev.y);
            break;
        case EV_MOUSE_UP:
            if (pressed >= 0 && pressed == key_at(ev.x, ev.y)) press(keys[pressed].code);
            pressed = -1;
            break;
        case EV_KEY: {
            uint32_t k = ev.key;
            if (k == 27) k = 'c';
            if (k == 'x' || k == 'X') k = '*';
            if (k == 'C') k = 'c';
            if (k < 128) press((char)k);
            break;
        }
        case EV_CLOSE:
            return 0;
        default:
            continue;
        }
        draw();
    }
}
