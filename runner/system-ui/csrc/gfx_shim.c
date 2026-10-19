/*
 * gfx_shim.c - the small C entry points a Rust system app uses to draw with
 * the kernel apps' own software graphics and Facet font.
 */
#include <stdint.h>
#include "gfx.h"

void aether_gfx_init(struct surface *s, uint32_t *px, int w, int h, int pitch) {
    gfx_init_surface(s, px, w, h, pitch);
}
void aether_gfx_fill(struct surface *s, int x, int y, int w, int h, uint32_t c) {
    gfx_fill(s, x, y, w, h, c);
}
void aether_gfx_blend(struct surface *s, int x, int y, int w, int h, uint32_t c, unsigned a) {
    gfx_blend(s, x, y, w, h, c, a);
}
void aether_gfx_round_rect(struct surface *s, int x, int y, int w, int h, int r, uint32_t c, unsigned a) {
    gfx_round_rect(s, x, y, w, h, r, c, a);
}
void aether_gfx_round_outline(struct surface *s, int x, int y, int w, int h, int r, uint32_t c, unsigned a) {
    gfx_round_outline(s, x, y, w, h, r, c, a);
}
void aether_gfx_circle(struct surface *s, int cx, int cy, int r, uint32_t c, unsigned a) {
    gfx_circle(s, cx, cy, r, c, a);
}
void aether_gfx_image(struct surface *s, int x, int y, int w, int h, const uint8_t *rgba, int iw, int ih) {
    gfx_image(s, x, y, w, h, rgba, iw, ih);
}
int aether_gfx_text(struct surface *s, int x, int y, const char *text, uint32_t c, int font) {
    return gfx_text(s, x, y, text, c, (enum font_size)font);
}
int aether_gfx_text_width(const char *text, int font) {
    return gfx_text_width(text, (enum font_size)font);
}
int aether_gfx_font_h(int font) {
    return gfx_font_h((enum font_size)font);
}
int aether_gfx_font_w(int font) {
    return gfx_font_w((enum font_size)font);
}
