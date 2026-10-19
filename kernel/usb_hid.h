/* usb_hid.h - USB HID boot keyboard and mouse decoding. */
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "input.h"

struct usb_hid_kbd {
    uint8_t prev[6];
    uint8_t mods;
    uint8_t caps;
};

struct usb_hid_mouse {
    uint8_t buttons;
};

/* Pure report decoders, shared with the kernel unit tests. They return the
 * number of events written (0..max). */
int usb_hid_keyboard_report(struct usb_hid_kbd *st, const uint8_t *report, struct input_event *out, int max);
int usb_hid_mouse_report(struct usb_hid_mouse *st, const uint8_t *report, size_t len, struct input_event *out);
