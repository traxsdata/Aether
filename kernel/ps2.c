/*
 * ps2.c - i8042 PS/2 controller: keyboard (scancode set 1 via the
 * controller's translation) and a standard 3-byte mouse.
 */
#include "../include/abi.h"
#include "cpu.h"
#include "input.h"
#include "kernel.h"
#include "pic.h"

#define PS2_DATA 0x60
#define PS2_STATUS 0x64
#define PS2_CMD 0x64

/* ---- Controller helpers ------------------------------------------------ */

static int wait_write(void) {
    for (int i = 0; i < 100000; i++)
        if (!(inb(PS2_STATUS) & 2)) return 1;
    return 0;
}

static int wait_read(void) {
    for (int i = 0; i < 100000; i++)
        if (inb(PS2_STATUS) & 1) return 1;
    return 0;
}

static void ctrl_cmd(uint8_t c) {
    wait_write();
    outb(PS2_CMD, c);
}

static void ctrl_data(uint8_t d) {
    wait_write();
    outb(PS2_DATA, d);
}

static uint8_t ctrl_read(void) {
    wait_read();
    return inb(PS2_DATA);
}

static uint8_t mouse_write(uint8_t b) {
    ctrl_cmd(0xD4); /* next data byte goes to the aux (mouse) port */
    ctrl_data(b);
    return ctrl_read(); /* 0xFA = ACK */
}

static int packet_size = 3; /* 4 with a wheel */

void ps2_init(void) {
    ctrl_cmd(0xAD); /* disable keyboard port */
    ctrl_cmd(0xA7); /* disable mouse port */
    while (inb(PS2_STATUS) & 1) inb(PS2_DATA); /* flush */

    ctrl_cmd(0x20);
    uint8_t config = ctrl_read();
    config |= 0x01 | 0x02 | 0x40; /* IRQ1, IRQ12, scancode translation */
    config &= (uint8_t)~0x30;     /* clocks enabled for both ports */
    ctrl_cmd(0x60);
    ctrl_data(config);

    ctrl_cmd(0xAE); /* enable keyboard */
    ctrl_cmd(0xA8); /* enable mouse */
    uint8_t a1 = mouse_write(0xF6); /* defaults */
    /* The IntelliMouse handshake: sample rates 200, 100, 80, and a wheel
     * mouse answers "get ID" with 3 and sends 4-byte packets from then on. */
    static const uint8_t rates[] = {200, 100, 80};
    for (unsigned i = 0; i < sizeof rates; i++) {
        mouse_write(0xF3);
        mouse_write(rates[i]);
    }
    if (mouse_write(0xF2) == 0xFA && ctrl_read() == 3) packet_size = 4;
    uint8_t a2 = mouse_write(0xF4); /* start streaming */
    if (a1 != 0xFA || a2 != 0xFA) kprintf("[ps2] mouse did not acknowledge setup (%x %x)\n", a1, a2);
    if (packet_size == 4) kprintf("[ps2] wheel mouse\n");
    while (inb(PS2_STATUS) & 1) inb(PS2_DATA);

    pic_unmask(1);
    pic_unmask(12);
}

/* ---- Keyboard ---------------------------------------------------------- */

static const char keymap[128] = {
    0, 27, '1', '2', '3', '4', '5', '6', '7', '8', '9', '0', '-', '=', '\b', '\t',
    'q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p', '[', ']', '\n', 0, 'a', 's',
    'd', 'f', 'g', 'h', 'j', 'k', 'l', ';', '\'', '`', 0, '\\', 'z', 'x', 'c', 'v',
    'b', 'n', 'm', ',', '.', '/', 0, '*', 0, ' ', 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, '7', '8', '9', '-', '4', '5', '6', '+', '1',
    '2', '3', '0', '.', 0, 0, 0, 0, 0,
};

static const char keymap_shift[128] = {
    0, 27, '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '_', '+', '\b', '\t',
    'Q', 'W', 'E', 'R', 'T', 'Y', 'U', 'I', 'O', 'P', '{', '}', '\n', 0, 'A', 'S',
    'D', 'F', 'G', 'H', 'J', 'K', 'L', ':', '"', '~', 0, '|', 'Z', 'X', 'C', 'V',
    'B', 'N', 'M', '<', '>', '?', 0, '*', 0, ' ', 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, '7', '8', '9', '-', '4', '5', '6', '+', '1',
    '2', '3', '0', '.', 0, 0, 0, 0, 0,
};

static int shift, ctrl, alt, super, caps, extended;

static uint16_t cur_mods(void) {
    return (shift ? MOD_SHIFT : 0) | (ctrl ? MOD_CTRL : 0) | (alt ? MOD_ALT : 0) | (super ? MOD_SUPER : 0);
}

void ps2_keyboard_irq(void) {
    uint8_t sc = inb(PS2_DATA);
    if (sc == 0xE0) {
        extended = 1;
        return;
    }
    int release = sc & 0x80;
    uint8_t code = sc & 0x7F;
    int ext = extended;
    extended = 0;

    /* Modifier changes are queued too: the window switcher stays open while
     * Alt or Super is held and commits when it is let go. */
    int *mod = 0;
    switch (code) {
    case 0x2A: case 0x36: mod = &shift; break;
    case 0x1D: mod = &ctrl; break;
    case 0x38: mod = &alt; break;
    case 0x5B: case 0x5C: if (ext) mod = &super; break; /* left and right logo keys */
    case 0x3A: if (!release) caps = !caps; return;
    }
    if (mod) {
        if (*mod != !release) {
            *mod = !release;
            input_push((struct input_event){.type = IN_MODS, .mods = cur_mods()});
        }
        return;
    }
    if (release) return;

    uint32_t key = 0;
    if (ext) {
        switch (code) {
        case 0x48: key = KEY_UP; break;
        case 0x50: key = KEY_DOWN; break;
        case 0x4B: key = KEY_LEFT; break;
        case 0x4D: key = KEY_RIGHT; break;
        case 0x47: key = KEY_HOME; break;
        case 0x4F: key = KEY_END; break;
        case 0x49: key = KEY_PGUP; break;
        case 0x51: key = KEY_PGDN; break;
        case 0x53: key = KEY_DELETE; break;
        case 0x1C: key = '\n'; break; /* keypad Enter */
        case 0x35: key = '/'; break;  /* keypad / */
        default: return;
        }
    } else {
        char c = shift ? keymap_shift[code] : keymap[code];
        if (caps && c >= 'a' && c <= 'z') c -= 32;
        else if (caps && c >= 'A' && c <= 'Z') c += 32;
        if (!c) return;
        key = (uint8_t)c;
    }
    input_push((struct input_event){.type = IN_KEY, .key = key, .mods = cur_mods()});
}

/* ---- Mouse ------------------------------------------------------------- */

static uint8_t packet[4];
static int packet_len;

void ps2_mouse_irq(void) {
    uint8_t b = inb(PS2_DATA);
    /* Byte 0 always has bit 3 set and never has the overflow bits set; a
     * late command ACK (0xFA) would otherwise knock us out of sync. */
    if (packet_len == 0 && (!(b & 0x08) || b == 0xFA)) return;
    packet[packet_len++] = b;
    if (packet_len < packet_size) return;
    packet_len = 0;
    if (packet[0] & 0xC0) return; /* overflow */
    int dx = packet[1] - ((packet[0] & 0x10) ? 256 : 0);
    int dy = packet[2] - ((packet[0] & 0x20) ? 256 : 0);
    int dz = packet_size == 4 ? (int)(packet[3] & 0x0F) - ((packet[3] & 0x08) ? 16 : 0) : 0;
    input_push((struct input_event){.type = IN_MOUSE, .buttons = packet[0] & 7, .dx = (int16_t)dx, .dy = (int16_t)-dy,
                                    .dz = (int8_t)dz});
}
