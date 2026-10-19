/*
 * gfx.h - 2D drawing on 32-bit pixel surfaces. Shared by the kernel's window
 * manager and by applications (linked into both), so it is integer-only and
 * depends on nothing but memcpy.
 *
 * Colors are 0xRRGGBB. A surface with `swap` set stores pixels as 0xBBGGRR
 * (used when drawing straight onto an RGBX framebuffer).
 */
#pragma once
#include <stdint.h>

struct rect {
    int x, y, w, h;
};

struct surface {
    uint32_t *px;
    int w, h, pitch; /* pitch in pixels */
    int swap;
    struct rect clip; /* drawing outside this is ignored */
};

enum font_size { FONT_SMALL, FONT_MEDIUM, FONT_LARGE };

void gfx_init_surface(struct surface *s, uint32_t *px, int w, int h, int pitch);
void gfx_set_clip(struct surface *s, struct rect r); /* intersected with the surface bounds */
struct rect rect_intersect(struct rect a, struct rect b);
struct rect rect_union(struct rect a, struct rect b); /* empty rects are ignored */
int rect_empty(struct rect r);
int rect_contains(struct rect r, int x, int y);

uint32_t gfx_mix(uint32_t dst, uint32_t src, unsigned alpha); /* alpha 0..255 */
void gfx_fill(struct surface *s, int x, int y, int w, int h, uint32_t color);
void gfx_blend(struct surface *s, int x, int y, int w, int h, uint32_t color, unsigned alpha);
void gfx_gradient_v(struct surface *s, int x, int y, int w, int h, uint32_t top, uint32_t bottom);
/* Rounded rectangle with anti-aliased corners. */
void gfx_round_rect(struct surface *s, int x, int y, int w, int h, int r, uint32_t color, unsigned alpha);
/* 1-pixel rounded outline. */
void gfx_round_outline(struct surface *s, int x, int y, int w, int h, int r, uint32_t color, unsigned alpha);
void gfx_circle(struct surface *s, int cx, int cy, int r, uint32_t color, unsigned alpha);
/* Soft drop shadow around a rounded rectangle. */
void gfx_shadow(struct surface *s, int x, int y, int w, int h, int r, int spread, unsigned alpha);
/* Copy a block of pixels, optionally rounding the bottom corners. */
void gfx_blit(struct surface *s, int dx, int dy, const uint32_t *src, int sw, int sh, int spitch);
/* Draw an RGBA8888 image, scaled to w x h with nearest-neighbour sampling. */
void gfx_image(struct surface *s, int x, int y, int w, int h, const uint8_t *rgba, int iw, int ih);

int gfx_font_w(enum font_size f);
int gfx_font_h(enum font_size f);
int gfx_text_width(const char *str, enum font_size f);
/* Draws text with its top-left at (x, y); returns the x after the last glyph. */
int gfx_text(struct surface *s, int x, int y, const char *str, uint32_t color, enum font_size f);
int gfx_char(struct surface *s, int x, int y, char c, uint32_t color, enum font_size f);
