/* ui.h - a window with a drawing surface, for æther apps. */
#pragma once
#include "../../common/gfx.h"
#include "sys.h"

struct window {
    int handle;
    struct surface s;
};

/* Returns 0 on success, a negative error otherwise. */
int window_open(struct window *w, int width, int height, const char *title);
void window_present(struct window *w);
void window_set_title(struct window *w, const char *title);
/* Opt in to being zoomed and resized by the user. The app then gets EV_RESIZE
 * and calls window_resize, which updates w->s; then it redraws. */
void window_set_resizable(struct window *w, int on, int min_w, int min_h);
int window_resize(struct window *w);
