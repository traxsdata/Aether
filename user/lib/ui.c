#include "ui.h"

int window_open(struct window *w, int width, int height, const char *title) {
    struct win_req req = {(uint32_t)width, (uint32_t)height, {0}};
    strlcpy(req.title, title, sizeof req.title);
    struct win_info info;
    int64_t h = syscall2(SYS_WIN_CREATE, &req, &info);
    if (h < 0) return (int)h;
    w->handle = (int)h;
    gfx_init_surface(&w->s, (uint32_t *)info.pixels, (int)info.width, (int)info.height, (int)info.pitch);
    return 0;
}

void window_present(struct window *w) { syscall1(SYS_WIN_PRESENT, w->handle); }

void window_set_resizable(struct window *w, int on, int min_w, int min_h) {
    syscall4(SYS_WIN_FLAGS, w->handle, on ? WIN_FLAG_RESIZABLE : 0, min_w, min_h);
}

int window_resize(struct window *w) {
    struct win_info info;
    int64_t r = syscall2(SYS_WIN_RESIZE, w->handle, &info);
    if (r < 0) return (int)r;
    gfx_init_surface(&w->s, (uint32_t *)info.pixels, (int)info.width, (int)info.height, (int)info.pitch);
    return 0;
}

void window_set_title(struct window *w, const char *title) {
    syscall3(SYS_WIN_TITLE, w->handle, title, strlen(title));
}
