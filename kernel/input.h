/* input.h - raw input from PS/2 and USB HID, queued for the window manager. */
#pragma once
#include <stdint.h>

enum { IN_KEY = 1, IN_MOUSE = 2, IN_MODS = 3 /* a modifier key went down or up; mods says which are held */ };

struct input_event {
    uint8_t type;
    uint8_t buttons; /* IN_MOUSE: bit0 left, bit1 right, bit2 middle */
    uint16_t mods;   /* IN_KEY, IN_MODS: MOD_* */
    uint32_t key;    /* IN_KEY: abi.h key code */
    int16_t dx, dy;  /* IN_MOUSE: movement, y grows downward */
    int8_t dz;       /* IN_MOUSE: wheel notches, positive = toward the user (scroll down) */
};

void ps2_init(void);
void ps2_keyboard_irq(void);
void ps2_mouse_irq(void);

/* input.c: the shared queue. Drivers on the polled path call input_push. */
void input_push(struct input_event e);
int input_pop(struct input_event *out); /* 1 if an event was returned */
extern volatile int input_pending;
