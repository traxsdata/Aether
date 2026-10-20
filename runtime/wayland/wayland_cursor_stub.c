// Minimal libwayland-cursor.so.0 fallback for the Aether Gate 4 host spike.
//
// The pinned GridCraft binary does not reference these symbols, but a future
// winit/SCTK build may dlopen libwayland-cursor.  Reporting "no theme" makes
// callers fall back to no cursor instead of failing to load the library.

#include <stdint.h>
#include <stdlib.h>

#define EXPORT __attribute__((visibility("default")))

struct wl_cursor_image {
    uint32_t width;
    uint32_t height;
    uint32_t hotspot_x;
    uint32_t hotspot_y;
    uint32_t delay;
};

struct wl_cursor {
    unsigned int image_count;
    struct wl_cursor_image **images;
    char *name;
};

EXPORT struct wl_cursor_theme *wl_cursor_theme_load(const char *name, int size,
                                                    void *shm) {
    (void)name;
    (void)size;
    (void)shm;
    return NULL;
}

EXPORT void wl_cursor_theme_destroy(struct wl_cursor_theme *theme) {
    (void)theme;
}

EXPORT struct wl_cursor *wl_cursor_theme_get_cursor(
    struct wl_cursor_theme *theme, const char *name) {
    (void)theme;
    (void)name;
    return NULL;
}

EXPORT void *wl_cursor_image_get_buffer(struct wl_cursor_image *image) {
    (void)image;
    return NULL;
}

EXPORT int wl_cursor_frame(struct wl_cursor *cursor, uint32_t time) {
    (void)cursor;
    (void)time;
    return 0;
}

EXPORT int wl_cursor_frame_and_duration(struct wl_cursor *cursor,
                                        uint32_t time, uint32_t *duration) {
    (void)cursor;
    (void)time;
    if (duration)
        *duration = 0;
    return 0;
}
