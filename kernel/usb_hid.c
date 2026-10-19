/*
 * usb_hid.c - boot-protocol USB keyboards and mice.
 *
 * HID boot reports are fixed-size and need no report-descriptor parser:
 * a keyboard report is eight bytes (modifiers, reserved, six key usages) and
 * a mouse report is three or four bytes (buttons, dx, dy, wheel). The devices
 * are polled through one interrupt-IN transfer at a time.
 *
 * Only newly pressed keys are emitted. The window manager has no key-release
 * event, so releases only update the held-key set. LEDs and keyboard repeat
 * are left to the device/firmware; the ABI repeats nothing today.
 */
#include "../include/abi.h"
#include "kernel.h"
#include "usb.h"
#include "usb_hid.h"

#define MAX_HID 4

static struct {
    int used;
    struct usb_device *dev;
    struct usb_endpoint *ep;
    uint8_t proto; /* 1 keyboard, 2 mouse */
    struct usb_hid_kbd kbd;
    struct usb_hid_mouse mouse;
    int seen_input;
} hid[MAX_HID];

static const char unshifted[128] = {
    [0x04] = 'a', [0x05] = 'b', [0x06] = 'c', [0x07] = 'd', [0x08] = 'e', [0x09] = 'f', [0x0A] = 'g',
    [0x0B] = 'h', [0x0C] = 'i', [0x0D] = 'j', [0x0E] = 'k', [0x0F] = 'l', [0x10] = 'm', [0x11] = 'n',
    [0x12] = 'o', [0x13] = 'p', [0x14] = 'q', [0x15] = 'r', [0x16] = 's', [0x17] = 't', [0x18] = 'u',
    [0x19] = 'v', [0x1A] = 'w', [0x1B] = 'x', [0x1C] = 'y', [0x1D] = 'z', [0x1E] = '1', [0x1F] = '2',
    [0x20] = '3', [0x21] = '4', [0x22] = '5', [0x23] = '6', [0x24] = '7', [0x25] = '8', [0x26] = '9',
    [0x27] = '0', [0x28] = '\n', [0x29] = 27, [0x2A] = '\b', [0x2B] = '\t', [0x2C] = ' ', [0x2D] = '-',
    [0x2E] = '=', [0x2F] = '[', [0x30] = ']', [0x31] = '\\', [0x32] = ';', [0x33] = '\'', [0x34] = '`',
    [0x35] = ',', [0x36] = '.', [0x37] = '/',
    [0x54] = '/', [0x55] = '*', [0x56] = '-', [0x57] = '+', [0x58] = '\n', [0x59] = '1', [0x5A] = '2',
    [0x5B] = '3', [0x5C] = '4', [0x5D] = '5', [0x5E] = '6', [0x5F] = '7', [0x60] = '8', [0x61] = '9',
    [0x62] = '0', [0x63] = '.',
};

static const char shifted[128] = {
    [0x04] = 'A', [0x05] = 'B', [0x06] = 'C', [0x07] = 'D', [0x08] = 'E', [0x09] = 'F', [0x0A] = 'G',
    [0x0B] = 'H', [0x0C] = 'I', [0x0D] = 'J', [0x0E] = 'K', [0x0F] = 'L', [0x10] = 'M', [0x11] = 'N',
    [0x12] = 'O', [0x13] = 'P', [0x14] = 'Q', [0x15] = 'R', [0x16] = 'S', [0x17] = 'T', [0x18] = 'U',
    [0x19] = 'V', [0x1A] = 'W', [0x1B] = 'X', [0x1C] = 'Y', [0x1D] = 'Z', [0x1E] = '!', [0x1F] = '@',
    [0x20] = '#', [0x21] = '$', [0x22] = '%', [0x23] = '^', [0x24] = '&', [0x25] = '*', [0x26] = '(',
    [0x27] = ')', [0x28] = '\n', [0x29] = 27, [0x2A] = '\b', [0x2B] = '\t', [0x2C] = ' ', [0x2D] = '_',
    [0x2E] = '+', [0x2F] = '{', [0x30] = '}', [0x31] = '|', [0x32] = ':', [0x33] = '"', [0x34] = '~',
    [0x35] = '<', [0x36] = '>', [0x37] = '?',
    [0x54] = '/', [0x55] = '*', [0x56] = '-', [0x57] = '+', [0x58] = '\n', [0x59] = '1', [0x5A] = '2',
    [0x5B] = '3', [0x5C] = '4', [0x5D] = '5', [0x5E] = '6', [0x5F] = '7', [0x60] = '8', [0x61] = '9',
    [0x62] = '0', [0x63] = '.',
};

static int held(const uint8_t *keys, uint8_t usage) {
    for (int i = 0; i < 6; i++)
        if (keys[i] == usage) return 1;
    return 0;
}

static uint16_t hid_mods(uint8_t b) {
    uint16_t m = 0;
    if (b & (1u << 0) || b & (1u << 4)) m |= MOD_CTRL;
    if (b & (1u << 1) || b & (1u << 5)) m |= MOD_SHIFT;
    if (b & (1u << 2) || b & (1u << 6)) m |= MOD_ALT;
    return m;
}

int usb_hid_keyboard_report(struct usb_hid_kbd *st, const uint8_t *r, struct input_event *out, int max) {
    if (!st || !r || !out || max <= 0) return 0;
    uint16_t mods = hid_mods(r[0]);
    int n = 0;
    for (int i = 2; i < 8 && n < max; i++) {
        uint8_t u = r[i];
        if (!u || u == 1 || held(st->prev, u)) continue;
        if (u == 0x39) { /* Caps Lock */
            st->caps ^= 1;
            continue;
        }
        if (u >= 0xE0 && u <= 0xE7) continue; /* pure modifier */
        uint32_t key = 0;
        if (u >= 0x49 && u <= 0x4E) {
            switch (u) {
            case 0x49: key = 0; break; /* Insert: no ABI key */
            case 0x4A: key = KEY_HOME; break;
            case 0x4B: key = KEY_PGUP; break;
            case 0x4C: key = KEY_DELETE; break;
            case 0x4D: key = KEY_END; break;
            case 0x4E: key = KEY_PGDN; break;
            }
        } else if (u >= 0x4F && u <= 0x52) {
            key = u == 0x4F ? KEY_RIGHT : u == 0x50 ? KEY_LEFT : u == 0x51 ? KEY_DOWN : KEY_UP;
        } else if (u < 128) {
            char c = (mods & MOD_SHIFT) ? shifted[u] : unshifted[u];
            if ((st->caps & 1) && c >= 'a' && c <= 'z') c -= 32;
            else if ((st->caps & 1) && c >= 'A' && c <= 'Z') c += 32;
            if (c) key = (uint8_t)c;
        }
        if (key) out[n++] = (struct input_event){.type = IN_KEY, .key = key, .mods = mods};
    }
    memcpy(st->prev, r + 2, 6);
    st->mods = (uint8_t)mods;
    return n;
}

int usb_hid_mouse_report(struct usb_hid_mouse *st, const uint8_t *r, size_t len, struct input_event *out) {
    if (!st || !r || !out || len < 3) return 0;
    uint8_t buttons = r[0] & 7;
    int dx = (int8_t)r[1];
    int dy = (int8_t)r[2];
    int dz = len >= 4 ? (int8_t)r[3] : 0;
    if (dx == 0 && dy == 0 && dz == 0 && buttons == st->buttons) return 0;
    st->buttons = buttons;
    out[0] = (struct input_event){.type = IN_MOUSE, .buttons = buttons, .dx = (int16_t)dx, .dy = (int16_t)dy,
                                  .dz = (int8_t)dz};
    return 1;
}

void usb_hid_probe(struct usb_device *d, uint8_t proto, struct usb_endpoint *ep) {
    for (int i = 0; i < MAX_HID; i++) {
        if (hid[i].used) continue;
        memset(&hid[i], 0, sizeof hid[i]);
        hid[i].used = 1;
        hid[i].dev = d;
        hid[i].ep = ep;
        hid[i].proto = proto;
        /* Boot protocol and no idle reports: the device sends only changes. */
        usb_control(d, 0x21, 0x0B, 0, d->iface, 0, 0);
        usb_control(d, 0x21, 0x0A, 0, d->iface, 0, 0);
        kprintf("[usb] slot %u: HID %s ready\n", d->slot, proto == 1 ? "keyboard" : "mouse");
        return;
    }
    kprintf("[usb] slot %u: too many HID devices\n", d->slot);
}

static void hid_poll_one(int i) {
    struct usb_endpoint *ep = hid[i].ep;
    if (ep->pending) return;
    if (ep->status == 0) {
        usb_ep_submit(hid[i].dev, ep, 0, 8);
        return;
    }
    uint8_t report[8];
    size_t actual = 0;
    int r = usb_ep_complete(hid[i].dev, ep, report, sizeof report, &actual);
    if (r < 0) usb_ep_abort(hid[i].dev, ep);
    if (r > 0) {
        struct input_event ev[6];
        int n = 0;
        if (hid[i].proto == 1 && actual >= 8) n = usb_hid_keyboard_report(&hid[i].kbd, report, ev, ARRAY_LEN(ev));
        else if (hid[i].proto == 2 && actual >= 3) n = usb_hid_mouse_report(&hid[i].mouse, report, actual, ev);
        for (int k = 0; k < n; k++) input_push(ev[k]);
        if (n && !hid[i].seen_input) {
            hid[i].seen_input = 1;
            kprintf("[usb] slot %u: HID %s input active\n", hid[i].dev->slot,
                    hid[i].proto == 1 ? "keyboard" : "mouse");
        }
    }
    /* A short report never means "stop polling"; an error is retried on the
     * next loop too, so a detached device cannot wedge the input queue. */
    if (!ep->pending) usb_ep_submit(hid[i].dev, ep, 0, 8);
}

void usb_hid_poll(void) {
    for (int i = 0; i < MAX_HID; i++)
        if (hid[i].used) hid_poll_one(i);
}
