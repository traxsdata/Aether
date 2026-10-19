/* gfx.c - see gfx.h. */
#include "gfx.h"
#include "../include/font_data.h"
#include "string.h"


static inline int imin(int a, int b) { return a < b ? a : b; }
static inline int imax(int a, int b) { return a > b ? a : b; }

int rect_empty(struct rect r) { return r.w <= 0 || r.h <= 0; }

int rect_contains(struct rect r, int x, int y) {
    return x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h;
}

struct rect rect_intersect(struct rect a, struct rect b) {
    int x0 = imax(a.x, b.x), y0 = imax(a.y, b.y);
    int x1 = imin(a.x + a.w, b.x + b.w), y1 = imin(a.y + a.h, b.y + b.h);
    struct rect r = {x0, y0, x1 - x0, y1 - y0};
    if (rect_empty(r)) r.w = r.h = 0;
    return r;
}

struct rect rect_union(struct rect a, struct rect b) {
    if (rect_empty(a)) return b;
    if (rect_empty(b)) return a;
    int x0 = imin(a.x, b.x), y0 = imin(a.y, b.y);
    int x1 = imax(a.x + a.w, b.x + b.w), y1 = imax(a.y + a.h, b.y + b.h);
    return (struct rect){x0, y0, x1 - x0, y1 - y0};
}

void gfx_init_surface(struct surface *s, uint32_t *px, int w, int h, int pitch) {
    s->px = px;
    s->w = w;
    s->h = h;
    s->pitch = pitch;
    s->swap = 0;
    s->clip = (struct rect){0, 0, w, h};
}

void gfx_set_clip(struct surface *s, struct rect r) {
    s->clip = rect_intersect(r, (struct rect){0, 0, s->w, s->h});
}

static inline uint32_t conv(const struct surface *s, uint32_t c) {
    return s->swap ? ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF) : c;
}

uint32_t gfx_mix(uint32_t d, uint32_t c, unsigned a) {
    if (a >= 255) return c;
    if (a == 0) return d;
    unsigned ia = 255 - a;
    /* Blend red+blue and green in parallel. */
    uint32_t rb = (((c & 0xFF00FF) * a + (d & 0xFF00FF) * ia + 0x800080) >> 8) & 0xFF00FF;
    uint32_t g = (((c & 0x00FF00) * a + (d & 0x00FF00) * ia + 0x008000) >> 8) & 0x00FF00;
    return rb | g;
}

/* Clip a rectangle; returns 0 if nothing is left. */
static int clip(const struct surface *s, int *x, int *y, int *w, int *h) {
    struct rect r = rect_intersect((struct rect){*x, *y, *w, *h}, s->clip);
    *x = r.x, *y = r.y, *w = r.w, *h = r.h;
    return !rect_empty(r);
}

static inline void put(struct surface *s, int x, int y, uint32_t c, unsigned a) {
    if (!rect_contains(s->clip, x, y)) return;
    uint32_t *p = &s->px[(long)y * s->pitch + x];
    *p = gfx_mix(*p, conv(s, c), a);
}

void gfx_fill(struct surface *s, int x, int y, int w, int h, uint32_t color) {
    if (!clip(s, &x, &y, &w, &h)) return;
    uint32_t c = conv(s, color);
    for (int j = 0; j < h; j++) {
        uint32_t *row = &s->px[(long)(y + j) * s->pitch + x];
        for (int i = 0; i < w; i++) row[i] = c;
    }
}

void gfx_blend(struct surface *s, int x, int y, int w, int h, uint32_t color, unsigned alpha) {
    if (alpha >= 255) {
        gfx_fill(s, x, y, w, h, color);
        return;
    }
    if (!clip(s, &x, &y, &w, &h)) return;
    uint32_t c = conv(s, color);
    for (int j = 0; j < h; j++) {
        uint32_t *row = &s->px[(long)(y + j) * s->pitch + x];
        for (int i = 0; i < w; i++) row[i] = gfx_mix(row[i], c, alpha);
    }
}

void gfx_gradient_v(struct surface *s, int x, int y, int w, int h, uint32_t top, uint32_t bottom) {
    for (int j = 0; j < h; j++) {
        if (y + j < s->clip.y || y + j >= s->clip.y + s->clip.h) continue;
        gfx_fill(s, x, y + j, w, 1, gfx_mix(top, bottom, h > 1 ? (unsigned)(j * 255 / (h - 1)) : 0));
    }
}

static unsigned isqrt(unsigned v) {
    unsigned r = 0, bit = 1u << 30;
    while (bit > v) bit >>= 2;
    while (bit) {
        if (v >= r + bit) {
            v -= r + bit;
            r = (r >> 1) + bit;
        } else {
            r >>= 1;
        }
        bit >>= 2;
    }
    return r;
}

/*
 * Coverage (0..255) of pixel (px, py) by a disc of radius r centred at
 * (cx, cy), all in 1/16 pixel units: a cheap anti-aliased circle edge.
 */
static unsigned disc_cover(int px, int py, int cx, int cy, int r) {
    int dx = px - cx, dy = py - cy;
    int d = (int)isqrt((unsigned)(dx * dx + dy * dy));
    int edge = r - d; /* 1/16 px inside the edge */
    if (edge >= 8) return 255;
    if (edge <= -8) return 0;
    return (unsigned)((edge + 8) * 255 / 16);
}

void gfx_round_rect(struct surface *s, int x, int y, int w, int h, int r, uint32_t color, unsigned alpha) {
    if (r * 2 > w) r = w / 2;
    if (r * 2 > h) r = h / 2;
    if (r <= 0) {
        gfx_blend(s, x, y, w, h, color, alpha);
        return;
    }
    /* Middle band and the two side bands between the corners. */
    gfx_blend(s, x, y + r, w, h - 2 * r, color, alpha);
    gfx_blend(s, x + r, y, w - 2 * r, r, color, alpha);
    gfx_blend(s, x + r, y + h - r, w - 2 * r, r, color, alpha);
    /* Four anti-aliased corners. */
    for (int j = 0; j < r; j++) {
        for (int i = 0; i < r; i++) {
            unsigned cov = disc_cover(i * 16 + 8, j * 16 + 8, r * 16, r * 16, r * 16);
            if (!cov) continue;
            unsigned a = cov * alpha / 255;
            put(s, x + i, y + j, color, a);
            put(s, x + w - 1 - i, y + j, color, a);
            put(s, x + i, y + h - 1 - j, color, a);
            put(s, x + w - 1 - i, y + h - 1 - j, color, a);
        }
    }
}

void gfx_round_outline(struct surface *s, int x, int y, int w, int h, int r, uint32_t color, unsigned alpha) {
    if (r * 2 > w) r = w / 2;
    if (r * 2 > h) r = h / 2;
    gfx_blend(s, x + r, y, w - 2 * r, 1, color, alpha);
    gfx_blend(s, x + r, y + h - 1, w - 2 * r, 1, color, alpha);
    gfx_blend(s, x, y + r, 1, h - 2 * r, color, alpha);
    gfx_blend(s, x + w - 1, y + r, 1, h - 2 * r, color, alpha);
    for (int j = 0; j < r; j++) {
        for (int i = 0; i < r; i++) {
            int px = i * 16 + 8, py = j * 16 + 8, c = r * 16;
            unsigned outer = disc_cover(px, py, c, c, c);
            unsigned inner = disc_cover(px, py, c, c, c - 16);
            unsigned cov = outer > inner ? outer - inner : 0;
            if (!cov) continue;
            unsigned a = cov * alpha / 255;
            put(s, x + i, y + j, color, a);
            put(s, x + w - 1 - i, y + j, color, a);
            put(s, x + i, y + h - 1 - j, color, a);
            put(s, x + w - 1 - i, y + h - 1 - j, color, a);
        }
    }
}

void gfx_circle(struct surface *s, int cx, int cy, int r, uint32_t color, unsigned alpha) {
    for (int j = -r - 1; j <= r; j++) {
        for (int i = -r - 1; i <= r; i++) {
            unsigned cov = disc_cover(i * 16 + 8, j * 16 + 8, 0, 0, r * 16);
            if (cov) put(s, cx + i, cy + j, color, cov * alpha / 255);
        }
    }
}

void gfx_shadow(struct surface *s, int x, int y, int w, int h, int r, int spread, unsigned alpha) {
    /* Stacked translucent rounded rects approximate a blurred shadow. */
    for (int i = spread; i > 0; i--) {
        unsigned a = alpha * (unsigned)(spread - i + 1) / (unsigned)(spread * spread / 2 + 1);
        gfx_round_rect(s, x - i, y - i + spread / 3, w + 2 * i, h + 2 * i, r + i, 0x000000, a);
    }
}

void gfx_blit(struct surface *s, int dx, int dy, const uint32_t *src, int sw, int sh, int spitch) {
    int x = dx, y = dy, w = sw, h = sh;
    if (!clip(s, &x, &y, &w, &h)) return;
    for (int j = 0; j < h; j++) {
        const uint32_t *in = src + (long)(y - dy + j) * spitch + (x - dx);
        uint32_t *out = &s->px[(long)(y + j) * s->pitch + x];
        if (!s->swap) {
            memcpy(out, in, (unsigned long)w * 4);
        } else {
            for (int i = 0; i < w; i++) out[i] = conv(s, in[i]);
        }
    }
}

void gfx_image(struct surface *s, int x, int y, int w, int h, const uint8_t *rgba, int iw, int ih) {
    if (!rgba || iw <= 0 || ih <= 0 || w <= 0 || h <= 0) return;
    int x0 = x, y0 = y, w0 = w, h0 = h;
    if (!clip(s, &x0, &y0, &w0, &h0)) return;
    for (int j = 0; j < h0; j++) {
        int sy = (int)((long)(y0 - y + j) * ih / h);
        if (sy >= ih) sy = ih - 1;
        const uint8_t *row = rgba + (long)sy * iw * 4;
        for (int i = 0; i < w0; i++) {
            int sx = (int)((long)(x0 - x + i) * iw / w);
            if (sx >= iw) sx = iw - 1;
            const uint8_t *p = row + (long)sx * 4;
            if (p[3] == 0) continue;
            put(s, x0 + i, y0 + j, ((uint32_t)p[0] << 16) | ((uint32_t)p[1] << 8) | p[2], p[3]);
        }
    }
}

/* ---- Text -------------------------------------------------------------- */

int gfx_font_w(enum font_size f) {
    return f == FONT_LARGE ? FONT_LARGE_W : f == FONT_MEDIUM ? FONT_MEDIUM_W : FONT_SMALL_W;
}

int gfx_font_h(enum font_size f) {
    return f == FONT_LARGE ? FONT_LARGE_H : f == FONT_MEDIUM ? FONT_MEDIUM_H : FONT_SMALL_H;
}

/* Text is UTF-8; besides ASCII the font has the two letters of "æther". */
static int next_glyph(const char **p) {
    const unsigned char *s = (const unsigned char *)*p;
    if (s[0] == 0xC3 && (s[1] == 0xA6 || s[1] == 0x86)) {
        *p += 2;
        return s[1] == 0xA6 ? FONT_GLYPH_AE_LOWER : FONT_GLYPH_AE_UPPER;
    }
    *p += 1;
    if (s[0] < FONT_FIRST || s[0] > FONT_LAST) return '?' - FONT_FIRST;
    return s[0] - FONT_FIRST;
}

int gfx_text_width(const char *str, enum font_size f) {
    int n = 0;
    while (*str) next_glyph(&str), n++;
    return n * gfx_font_w(f);
}

static int draw_glyph(struct surface *s, int x, int y, int idx, uint32_t color, enum font_size f) {
    int w = gfx_font_w(f), h = gfx_font_h(f);
    const uint8_t *g = f == FONT_LARGE    ? &font_large[idx][0][0]
                       : f == FONT_MEDIUM ? &font_medium[idx][0][0]
                                          : &font_small[idx][0][0];
    struct rect box = rect_intersect((struct rect){x, y, w, h}, s->clip);
    if (rect_empty(box)) return x + w;
    uint32_t col = conv(s, color);
    for (int j = box.y - y; j < box.y - y + box.h; j++) {
        uint32_t *row = &s->px[(long)(y + j) * s->pitch];
        for (int i = box.x - x; i < box.x - x + box.w; i++) {
            unsigned a = g[j * w + i];
            if (a) row[x + i] = gfx_mix(row[x + i], col, a);
        }
    }
    return x + w;
}

int gfx_char(struct surface *s, int x, int y, char c, uint32_t color, enum font_size f) {
    int idx = (c < FONT_FIRST || c > FONT_LAST) ? '?' - FONT_FIRST : c - FONT_FIRST;
    return draw_glyph(s, x, y, idx, color, f);
}

int gfx_text(struct surface *s, int x, int y, const char *str, uint32_t color, enum font_size f) {
    while (*str) x = draw_glyph(s, x, y, next_glyph(&str), color, f);
    return x;
}
