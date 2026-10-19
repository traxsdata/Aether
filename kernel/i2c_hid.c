/*
 * i2c_hid.c - Intel LPSS I2C and a HID-over-I2C touchpad/pen.
 *
 * The Yoga 9 14IAP7 routes its touchpad through LPSS I2C1 (PCI 00:15.1) at
 * address 0x2c (Synaptics) or 0x15 (Elan); the pen is on I2C0 at 0x0a. These
 * are the resources in the machine's ACPI namespace. The driver below is
 * deliberately small and polled:
 *
 *   LPSS private reset/remap registers -> Synopsys DesignWare I2C master
 *   -> HID-over-I2C descriptor, report descriptor and input register.
 *
 * There is no ACPI AML interpreter and no GPIO interrupt controller in this
 * kernel, so the input register is polled at a bounded rate instead of being
 * woken by the touchpad's GpioInt. No user pointer or interrupt handler is
 * involved. Every report length, bit offset, I2C address and register is
 * checked before use.
 */
#include "../include/abi.h"
#include "cpu.h"
#include "drivers.h"
#include "i2c_hid.h"
#include "input.h"
#include "kernel.h"
#include "mm.h"
#include "pci.h"

#define I2C_WINDOW 0xFFFFFE0005000000ull
#define I2C_STRIDE 0x2000
#define LPSS_PRIV 0x200
#define LPSS_RESETS 0x04
#define LPSS_REMAP 0x40
#define LPSS_CAPS 0xFC

/* DesignWare I2C registers. */
#define IC_CON 0x00
#define IC_TAR 0x04
#define IC_DATA_CMD 0x10
#define IC_SS_SCL_HCNT 0x14
#define IC_SS_SCL_LCNT 0x18
#define IC_FS_SCL_HCNT 0x1C
#define IC_FS_SCL_LCNT 0x20
#define IC_INTR_MASK 0x30
#define IC_RAW_INTR_STAT 0x34
#define IC_RX_TL 0x38
#define IC_TX_TL 0x3C
#define IC_CLR_INTR 0x40
#define IC_CLR_TX_ABRT 0x54
#define IC_CLR_STOP_DET 0x60
#define IC_ENABLE 0x6C
#define IC_STATUS 0x70
#define IC_TXFLR 0x74
#define IC_RXFLR 0x78
#define IC_SDA_HOLD 0x7C
#define IC_TX_ABRT_SOURCE 0x80
#define IC_ENABLE_STATUS 0x9C
#define IC_COMP_PARAM1 0xF4
#define IC_COMP_VERSION 0xF8

#define IC_CON_MASTER 0x01
#define IC_CON_SPEED_FAST 0x04
#define IC_CON_RESTART_EN 0x20
#define IC_CON_SLAVE_DISABLE 0x40
#define IC_DATA_CMD_READ (1u << 8)
#define IC_DATA_CMD_STOP (1u << 9)
#define IC_DATA_CMD_RESTART (1u << 10)
#define IC_ENABLE_EN 0x01
#define IC_ENABLE_ABORT 0x02
#define IC_STATUS_ACTIVITY (1u << 0)
#define IC_STATUS_TFNF (1u << 1)
#define IC_STATUS_TFE (1u << 2)
#define IC_STATUS_RFNE (1u << 3)
#define IC_STATUS_MST_ACT (1u << 5)
#define IC_RAW_TX_ABRT (1u << 6)
#define IC_RAW_STOP_DET (1u << 9)

#define HID_DESC_AT 0x0001
#define HID_MAX_REPORT_DESC 4096
#define HID_MAX_INPUT 256
#define HID_MAX_FIELDS 96
#define HID_MAX_REPORTS 16

struct i2c_ctrl {
    volatile uint8_t *mmio;
    uint8_t fn;
    int ready;
    uint8_t tx_fifo, rx_fifo;
};

struct hid_desc {
    uint16_t len;
    uint16_t bcd;
    uint16_t report_len;
    uint16_t report_reg;
    uint16_t input_reg;
    uint16_t max_input;
    uint16_t output_reg;
    uint16_t max_output;
    uint16_t command_reg;
    uint16_t data_reg;
    uint16_t vendor;
    uint16_t product;
};

struct hid_field {
    uint8_t report_id;
    uint8_t size;
    uint8_t count;
    uint16_t page;
    uint16_t usage;
    uint32_t off;
    int32_t lmin, lmax;
    uint8_t relative;
};

struct hid_report {
    int present;
    uint8_t rid;
    int x, y, wheel, tip, contact;
    int button[3];
    int absolute;
    int32_t x_min, x_max, y_min, y_max;
};

struct hid_dev {
    int used;
    struct i2c_ctrl *ctrl;
    uint8_t addr;
    struct hid_desc d;
    struct hid_report rep;
    struct hid_field fields[HID_MAX_FIELDS];
    int nfields;
    int last_x, last_y, have_last, was_down;
    int pen, tap_down, tap_moved, tap_x, tap_y;
    uint64_t tap_at;
    uint8_t prev_buttons;
    uint8_t last_input[HID_MAX_INPUT];
    uint8_t last_report[64];
    int last_report_len;
    int seen_input;
    int fails;
    int fail_logged;
    uint64_t next_try;
};

static struct i2c_ctrl ctrls[2];
static struct hid_dev hids[2];
static uint64_t last_poll;

static inline uint32_t rd(struct i2c_ctrl *c, uint32_t off) { return *(volatile uint32_t *)(c->mmio + off); }
static inline void wr(struct i2c_ctrl *c, uint32_t off, uint32_t v) { *(volatile uint32_t *)(c->mmio + off) = v; }
static inline void wr64(struct i2c_ctrl *c, uint32_t off, uint64_t v) {
    *(volatile uint64_t *)(c->mmio + off) = v;
}
static inline void pause(void) { __asm__ volatile("pause"); }

static void delay_ms(unsigned ms) {
    uint64_t until = aether_pit_ms() + ms + 1;
    while (aether_pit_ms() < until) pause();
}

static int wait_reg(struct i2c_ctrl *c, uint32_t off, uint32_t mask, uint32_t value, int ms) {
    uint64_t until = aether_pit_ms() + (uint64_t)ms + 1;
    while ((rd(c, off) & mask) != value) {
        if (aether_pit_ms() >= until) return -E_TIMEOUT;
        pause();
    }
    return 0;
}

/* ---- DesignWare I2C ---------------------------------------------------- */

static void i2c_abort(struct i2c_ctrl *c) {
    wr(c, IC_ENABLE, IC_ENABLE_ABORT);
    (void)wait_reg(c, IC_ENABLE_STATUS, 1, 0, 20);
    wr(c, IC_ENABLE, IC_ENABLE_EN);
    (void)wait_reg(c, IC_ENABLE_STATUS, 1, 1, 20);
    (void)rd(c, IC_CLR_INTR);
}

static int i2c_init(struct i2c_ctrl *c) {
    uint32_t param = rd(c, IC_COMP_PARAM1);
    if (param == 0xFFFFFFFFu || rd(c, IC_COMP_VERSION) == 0xFFFFFFFFu) return -1;
    c->tx_fifo = (uint8_t)(((param >> 16) & 0xFF) + 1);
    c->rx_fifo = (uint8_t)(((param >> 8) & 0xFF) + 1);
    if (c->tx_fifo < 1) c->tx_fifo = 1;
    if (c->rx_fifo < 1) c->rx_fifo = 1;

    wr(c, IC_ENABLE, 0);
    if (wait_reg(c, IC_ENABLE_STATUS, 1, 0, 20)) return -1;
    /* 133 MHz LPSS input clock, fast mode. ACPI declares 400 kHz for the
     * touchpad; 120/240 counts give about 370 kHz at 133 MHz and about
     * 278 kHz if the firmware selected the 100 MHz clock. */
    wr(c, IC_CON, IC_CON_MASTER | IC_CON_SPEED_FAST | IC_CON_RESTART_EN | IC_CON_SLAVE_DISABLE);
    wr(c, IC_SS_SCL_HCNT, 640);
    wr(c, IC_SS_SCL_LCNT, 640);
    wr(c, IC_FS_SCL_HCNT, 120);
    wr(c, IC_FS_SCL_LCNT, 240);
    wr(c, IC_SDA_HOLD, 28 | (1u << 16)); /* 207 ns TX hold, RX hold on */
    wr(c, IC_RX_TL, 0);
    wr(c, IC_TX_TL, 0);
    wr(c, IC_INTR_MASK, 0);
    (void)rd(c, IC_CLR_INTR);
    wr(c, IC_ENABLE, IC_ENABLE_EN);
    if (wait_reg(c, IC_ENABLE_STATUS, 1, 1, 20)) return -1;
    return 0;
}

/* One master transaction. tx is a register address and optional data; rx is
 * a repeated-start read of the same register. */
static int i2c_xfer(struct i2c_ctrl *c, uint8_t addr, const uint8_t *tx, size_t txlen, uint8_t *rx, size_t rxlen) {
    if (!c->ready || txlen > 64 || rxlen > 4096 || (txlen && !tx) || (rxlen && !rx)) return -E_INVAL;
    if (rd(c, IC_STATUS) & IC_STATUS_ACTIVITY) {
        if (wait_reg(c, IC_STATUS, IC_STATUS_ACTIVITY, 0, 20)) {
            i2c_abort(c);
            return -E_BUSY;
        }
    }
    (void)rd(c, IC_CLR_INTR);
    wr(c, IC_TAR, addr);

    for (size_t i = 0; i < txlen; i++) {
        if (wait_reg(c, IC_STATUS, IC_STATUS_TFNF, IC_STATUS_TFNF, 20)) {
            i2c_abort(c);
            return -E_TIMEOUT;
        }
        uint32_t cmd = tx[i];
        if (i == txlen - 1 && rxlen == 0) cmd |= IC_DATA_CMD_STOP;
        wr(c, IC_DATA_CMD, cmd);
    }

    if (rxlen == 0) {
        uint64_t until = aether_pit_ms() + 20;
        while (!(rd(c, IC_RAW_INTR_STAT) & (IC_RAW_TX_ABRT | IC_RAW_STOP_DET))) {
            if (aether_pit_ms() >= until) {
                i2c_abort(c);
                return -E_TIMEOUT;
            }
            pause();
        }
        int aborted = (rd(c, IC_RAW_INTR_STAT) & IC_RAW_TX_ABRT) != 0;
        if (aborted) (void)rd(c, IC_CLR_TX_ABRT);
        (void)rd(c, IC_CLR_STOP_DET);
        (void)rd(c, IC_CLR_INTR);
        return aborted ? -E_IO : 0;
    }

    /* Queue the read commands immediately behind the register address. The
     * controller keeps the bus for the repeated start; waiting for the TX
     * FIFO to drain first would leave an empty-FIFO gap. */
    /* The FIFOs are small; issue a batch that both can hold, drain it, and
     * continue with the same transaction. */
    size_t done = 0;
    while (done < rxlen) {
        size_t batch = rxlen - done;
        if (batch > c->tx_fifo) batch = c->tx_fifo;
        if (batch > c->rx_fifo) batch = c->rx_fifo;
        if (batch == 0) batch = 1;
        for (size_t i = 0; i < batch; i++) {
            if (wait_reg(c, IC_STATUS, IC_STATUS_TFNF, IC_STATUS_TFNF, 20)) {
                i2c_abort(c);
                return -E_TIMEOUT;
            }
            uint32_t cmd = IC_DATA_CMD_READ;
            if (done + i == 0 && txlen) cmd |= IC_DATA_CMD_RESTART;
            if (done + i == rxlen - 1) cmd |= IC_DATA_CMD_STOP;
            wr(c, IC_DATA_CMD, cmd);
        }
        uint64_t until = aether_pit_ms() + 20;
        while (rd(c, IC_RXFLR) < batch) {
            if (rd(c, IC_RAW_INTR_STAT) & IC_RAW_TX_ABRT) {
                (void)rd(c, IC_CLR_TX_ABRT);
                (void)rd(c, IC_CLR_INTR);
                return -E_IO;
            }
            if (aether_pit_ms() >= until) {
                i2c_abort(c);
                return -E_TIMEOUT;
            }
            pause();
        }
        for (size_t i = 0; i < batch; i++) rx[done + i] = (uint8_t)(rd(c, IC_DATA_CMD) & 0xFF);
        done += batch;
    }
    (void)rd(c, IC_CLR_STOP_DET);
    (void)rd(c, IC_CLR_INTR);
    return 0;
}

/* ---- HID over I2C ------------------------------------------------------ */

static uint16_t le16(const uint8_t *p) { return (uint16_t)(p[0] | p[1] << 8); }
static void put16(uint8_t *p, uint16_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}

static int hid_read(struct hid_dev *h, uint16_t reg, void *buf, size_t len) {
    uint8_t a[2] = {(uint8_t)reg, (uint8_t)(reg >> 8)};
    return i2c_xfer(h->ctrl, h->addr, a, sizeof a, buf, len);
}

/* Input reports are a plain read; the HID input register is not addressed. */
static int hid_read_input(struct hid_dev *h, void *buf, size_t len) {
    return i2c_xfer(h->ctrl, h->addr, 0, 0, buf, len);
}

static int hid_write(struct hid_dev *h, uint16_t reg, const void *buf, size_t len) {
    uint8_t tx[66];
    if (!buf || len > sizeof tx - 2) return -E_INVAL;
    put16(tx, reg);
    memcpy(tx + 2, buf, len);
    return i2c_xfer(h->ctrl, h->addr, tx, len + 2, 0, 0);
}

/* HID over I2C command: first byte is report type/id (the value for commands
 * such as SET_POWER/SET_PROTOCOL), second byte is the opcode. */
static int hid_command(struct hid_dev *h, uint8_t opcode, uint8_t value) {
    uint8_t cmd[2] = {value, opcode};
    return hid_write(h, h->d.command_reg, cmd, sizeof cmd) ? -E_IO : 0;
}

static int hid_power_on(struct hid_dev *h) {
    int r = hid_command(h, 0x08, 0); /* SET_POWER, PWR_ON */
    delay_ms(60);
    return r;
}

static int hid_reset(struct hid_dev *h) {
    int r = hid_command(h, 0x01, 0); /* RESET */
    delay_ms(120);
    if (r) return r;
    return hid_power_on(h);
}

/* ---- HID report descriptor parser ------------------------------------- */

struct parser {
    uint32_t usage_page;
    int32_t lmin, lmax;
    uint32_t rsize, rcount;
    uint16_t usages[32];
    int nusages;
    int have_umin;
    uint32_t umin, umax;
    uint32_t bitpos[HID_MAX_REPORTS];
    uint8_t rid;
    struct hid_field fields[HID_MAX_FIELDS];
    int nfields;
};

static int32_t item_signed(uint32_t v, unsigned n) {
    if (n == 1) return (int8_t)v;
    if (n == 2) return (int16_t)v;
    return (int32_t)v;
}

static void add_local_usage(struct parser *p, uint32_t u) {
    if (p->nusages < (int)ARRAY_LEN(p->usages)) p->usages[p->nusages++] = (uint16_t)u;
}

static void reset_locals(struct parser *p) {
    p->nusages = 0;
    p->have_umin = 0;
}

static int add_field(struct parser *p, uint32_t off, uint32_t size, uint32_t page, uint32_t usage) {
    if (p->nfields >= HID_MAX_FIELDS || size == 0 || size > 32) return 0;
    struct hid_field *f = &p->fields[p->nfields++];
    memset(f, 0, sizeof *f);
    f->report_id = p->rid;
    f->size = (uint8_t)size;
    f->count = 1;
    f->page = (uint16_t)page;
    f->usage = (uint16_t)usage;
    f->off = off;
    f->lmin = p->lmin;
    f->lmax = p->lmax;
    return 1;
}

static void parse_input(struct parser *p, uint32_t data) {
    if (p->rid >= HID_MAX_REPORTS) {
        reset_locals(p);
        return;
    }
    uint32_t off = p->bitpos[p->rid];
    int variable = (data & 2) != 0;
    int constant = (data & 1) != 0;
    if (!variable || constant || p->rsize == 0 || p->rcount == 0) {
        p->bitpos[p->rid] = off + p->rsize * p->rcount;
        reset_locals(p);
        return;
    }
    uint32_t count = p->rcount > 32 ? 32 : p->rcount;
    for (uint32_t i = 0; i < count; i++) {
        uint32_t usage = i < (uint32_t)p->nusages ? p->usages[i]
                          : p->have_umin ? p->umin + i
                                         : 0;
        if (add_field(p, off, p->rsize, p->usage_page, usage))
            p->fields[p->nfields - 1].relative = (data & 4) != 0;
        off += p->rsize;
    }
    if (p->rcount > count) off += (p->rcount - count) * p->rsize;
    p->bitpos[p->rid] = off;
    reset_locals(p);
}

static int parse_report_descriptor(struct hid_dev *h, const uint8_t *d, size_t len) {
    struct parser p;
    memset(&p, 0, sizeof p);
    for (size_t i = 0; i < len;) {
        uint8_t prefix = d[i++];
        if (prefix == 0xFE) { /* long item */
            if (i + 2 > len) break;
            unsigned sz = d[i++];
            i++; /* tag */
            if (i + sz > len) break;
            i += sz;
            continue;
        }
        unsigned n = prefix & 3;
        if (n == 3) n = 4;
        unsigned type = (prefix >> 2) & 3;
        unsigned tag = prefix >> 4;
        if (i + n > len) break;
        uint32_t v = 0;
        for (unsigned k = 0; k < n; k++) v |= (uint32_t)d[i + k] << (8 * k);
        i += n;

        if (type == 0) { /* main */
            if (tag == 0x8) { /* Input */
                parse_input(&p, v);
            } else if (tag == 0x9 || tag == 0xB) { /* Output/Feature */
                reset_locals(&p);
            } else if (tag == 0xA) { /* Collection */
                reset_locals(&p);
            } else if (tag == 0xC) { /* End Collection */
                reset_locals(&p);
            }
        } else if (type == 1) { /* global */
            switch (tag) {
            case 0x0: p.usage_page = v; break;
            case 0x1: p.lmin = item_signed(v, n); break;
            case 0x2: p.lmax = item_signed(v, n); break;
            case 0x7: p.rsize = v; break;
            case 0x8:
                if (v < HID_MAX_REPORTS) {
                    p.rid = (uint8_t)v;
                    if (p.bitpos[p.rid] == 0) p.bitpos[p.rid] = 8; /* report ID byte */
                } else {
                    /* Out-of-range report ID: ignore its fields rather than
                     * filing them under the previous report. */
                    p.rid = HID_MAX_REPORTS;
                }
                break;
            case 0x9: p.rcount = v; break;
            default: break;
            }
        } else if (type == 2) { /* local */
            switch (tag) {
            case 0x0: add_local_usage(&p, v); break;
            case 0x1:
                p.have_umin = 1;
                p.umin = v;
                break;
            case 0x2: p.umax = v; break;
            default: break;
            }
        }
    }
    if (!p.nfields) return -E_NOTSUP;

    /* Prefer a normal mouse report (Generic Desktop X/Y) over a digitizer
     * report, because relative deltas need no absolute scaling. */
    int best = -1, best_score = -1;
    for (int rid = 0; rid < HID_MAX_REPORTS; rid++) {
        int x = -1, y = -1, wheel = -1, tip = -1, contact = -1, btn[3] = {-1, -1, -1};
        int score = 0;
        for (int i = 0; i < p.nfields; i++) {
            struct hid_field *f = &p.fields[i];
            if (f->report_id != rid) continue;
            if (f->page == 1 && f->usage == 0x30 && x < 0) x = i;
            if (f->page == 1 && f->usage == 0x31 && y < 0) y = i;
            if (f->page == 1 && f->usage == 0x38 && wheel < 0) wheel = i;
            if (f->page == 0x0D && f->usage == 0x30 && x < 0) x = i;
            if (f->page == 0x0D && f->usage == 0x31 && y < 0) y = i;
            if (f->page == 0x0D && f->usage == 0x42 && tip < 0) tip = i;
            if (f->page == 0x0D && f->usage == 0x54 && contact < 0) contact = i;
            if (f->page == 9 && f->usage >= 1 && f->usage <= 3 && btn[f->usage - 1] < 0) btn[f->usage - 1] = i;
        }
        if (x < 0 || y < 0) continue;
        if (p.fields[x].page == 1) score += 8;
        if (tip >= 0) score += 2;
        if (btn[0] >= 0) score += 1;
        if (score > best_score) {
            best = rid;
            best_score = score;
        }
    }
    if (best < 0) return -E_NOTSUP;

    struct hid_report *r = &h->rep;
    memset(r, 0, sizeof *r);
    r->present = 1;
    r->rid = (uint8_t)best;
    r->x = r->y = r->wheel = r->tip = r->contact = -1;
    r->button[0] = r->button[1] = r->button[2] = -1;
    for (int i = 0; i < p.nfields; i++) {
        struct hid_field *f = &p.fields[i];
        if (f->report_id != best) continue;
        if (f->page == 1 && f->usage == 0x30 && r->x < 0) r->x = i;
        if (f->page == 1 && f->usage == 0x31 && r->y < 0) r->y = i;
        if (f->page == 0x0D && f->usage == 0x30 && r->x < 0) r->x = i;
        if (f->page == 0x0D && f->usage == 0x31 && r->y < 0) r->y = i;
        if (f->page == 1 && f->usage == 0x38 && r->wheel < 0) r->wheel = i;
        if (f->page == 0x0D && f->usage == 0x42 && r->tip < 0) r->tip = i;
        if (f->page == 0x0D && f->usage == 0x54 && r->contact < 0) r->contact = i;
        if (f->page == 9 && f->usage >= 1 && f->usage <= 3 && r->button[f->usage - 1] < 0)
            r->button[f->usage - 1] = i;
    }
    if (r->x < 0 || r->y < 0) return -E_NOTSUP;
    struct hid_field *xf = &p.fields[r->x], *yf = &p.fields[r->y];
    r->absolute = !xf->relative;
    if (r->absolute && (xf->lmin >= xf->lmax || yf->lmin >= yf->lmax)) return -E_NOTSUP;
    r->x_min = xf->lmin;
    r->x_max = xf->lmax;
    r->y_min = yf->lmin;
    r->y_max = yf->lmax;
    /* Keep the selected field table for the decode path. */
    for (int i = 0; i < p.nfields; i++) h->fields[i] = p.fields[i];
    h->nfields = p.nfields;
    return 0;
}

/* ---- Report decode ----------------------------------------------------- */

static uint32_t get_bits(const uint8_t *d, size_t len, uint32_t off, unsigned size) {
    uint64_t v = 0;
    for (unsigned i = 0; i < size && i < 32; i++) {
        uint32_t bit = off + i;
        if (bit >= len * 8) break;
        v |= (uint64_t)((d[bit / 8] >> (bit & 7)) & 1) << i;
    }
    return (uint32_t)v;
}

static int32_t get_field(const struct hid_dev *h, const struct hid_field *f, const uint8_t *d, size_t len) {
    (void)h;
    if (f->lmin > f->lmax) return 0;
    uint32_t raw = get_bits(d, len, f->off, f->size);
    if (f->size < 32 && f->lmin < 0 && (raw & (1u << (f->size - 1))))
        raw |= ~((1u << f->size) - 1);
    int32_t v = (int32_t)raw;
    if (v < f->lmin) v = f->lmin;
    if (v > f->lmax) v = f->lmax;
    return v;
}

static int clamp_i16(int v) { return v > 32767 ? 32767 : v < -32768 ? -32768 : v; }

static void decode_and_push(struct hid_dev *h, const uint8_t *d, size_t len) {
    struct hid_report *r = &h->rep;
    if (!r->present || (r->rid && (len == 0 || d[0] != r->rid))) return;
    int buttons = 0;
    int has_button = 0;
    for (int i = 0; i < 3; i++)
        if (r->button[i] >= 0) {
            has_button = 1;
            if (get_bits(d, len, h->fields[r->button[i]].off, 1)) buttons |= 1 << i;
        }
    int tip = 0;
    if (r->tip >= 0) tip = get_bits(d, len, h->fields[r->tip].off, 1) != 0;
    if (r->contact >= 0) {
        int32_t n = get_field(h, &h->fields[r->contact], d, len);
        if (n == 0) tip = 0;
    }
    int dx = 0, dy = 0, dz = 0;
    int32_t x = get_field(h, &h->fields[r->x], d, len);
    int32_t y = get_field(h, &h->fields[r->y], d, len);
    if (r->wheel >= 0) dz = (int)get_field(h, &h->fields[r->wheel], d, len);
    int tap_click = 0;
    if (r->absolute) {
        int down = r->tip >= 0 ? tip : 1;
        if (down) {
            if (!h->have_last || !h->was_down) {
                h->last_x = x;
                h->last_y = y;
                h->have_last = 1;
                if (r->tip >= 0) {
                    h->tap_down = 1;
                    h->tap_moved = 0;
                    h->tap_x = x;
                    h->tap_y = y;
                    h->tap_at = ticks;
                }
            } else {
                int64_t w = (int64_t)boot_fb.width > 0 ? boot_fb.width : 1280;
                int64_t hgt = (int64_t)boot_fb.height > 0 ? boot_fb.height : 800;
                int64_t xr = (int64_t)r->x_max - r->x_min + 1;
                int64_t yr = (int64_t)r->y_max - r->y_min + 1;
                int64_t ddx = (int64_t)x - h->last_x;
                int64_t ddy = (int64_t)y - h->last_y;
                if (ddx > (1 << 20)) ddx = 1 << 20;
                if (ddx < -(1 << 20)) ddx = -(1 << 20);
                if (ddy > (1 << 20)) ddy = 1 << 20;
                if (ddy < -(1 << 20)) ddy = -(1 << 20);
                if (xr > 0) dx = clamp_i16((int)(ddx * w / xr));
                if (yr > 0) dy = clamp_i16((int)(ddy * hgt / yr));
                if (h->tap_down && xr > 0 && yr > 0) {
                    int64_t tx = (int64_t)h->tap_x - x, ty = (int64_t)h->tap_y - y;
                    if (tx > (1 << 20)) tx = 1 << 20;
                    if (tx < -(1 << 20)) tx = -(1 << 20);
                    if (ty > (1 << 20)) ty = 1 << 20;
                    if (ty < -(1 << 20)) ty = -(1 << 20);
                    uint64_t ux = (uint64_t)(tx < 0 ? -tx : tx);
                    uint64_t uy = (uint64_t)(ty < 0 ? -ty : ty);
                    uint64_t rx = (uint64_t)(xr > (1 << 20) ? (1 << 20) : xr);
                    uint64_t ry = (uint64_t)(yr > (1 << 20) ? (1 << 20) : yr);
                    uint64_t threshold = (rx * ry) / 400;
                    if (threshold == 0) threshold = 1;
                    if (ux * ux + uy * uy > threshold) h->tap_moved = 1;
                }
                h->last_x = x;
                h->last_y = y;
            }
            if (h->pen) buttons |= 1;
        } else {
            h->have_last = 0;
            if (h->tap_down) {
                h->tap_down = 0;
                if (!has_button && !h->pen && !h->tap_moved && ticks - h->tap_at <= 250) tap_click = 1;
            }
        }
        h->was_down = down;
    } else {
        dx = clamp_i16(x);
        dy = clamp_i16(y);
    }
    if (dz > 127) dz = 127;
    if (dz < -128) dz = -128;
    if (tap_click) {
        input_push((struct input_event){.type = IN_MOUSE, .buttons = 1, .dx = 0, .dy = 0, .dz = 0});
        input_push((struct input_event){.type = IN_MOUSE, .buttons = 0, .dx = 0, .dy = 0, .dz = 0});
        h->prev_buttons = 0;
    } else if (dx || dy || dz || buttons != h->prev_buttons) {
        input_push((struct input_event){.type = IN_MOUSE, .buttons = (uint8_t)buttons, .dx = (int16_t)dx,
                                        .dy = (int16_t)dy, .dz = (int8_t)dz});
        h->prev_buttons = (uint8_t)buttons;
    }
    if ((tap_click || dx || dy || dz) && !h->seen_input) {
        h->seen_input = 1;
        kprintf("[i2c-hid] touchpad input active\n");
    }
}

/* ---- Probe ------------------------------------------------------------- */

static int hid_parse_one(struct i2c_ctrl *c, uint8_t addr) {
    struct hid_dev *h = 0;
    for (int i = 0; i < (int)ARRAY_LEN(hids); i++)
        if (!hids[i].used) {
            h = &hids[i];
            break;
        }
    if (!h) return -E_LIMIT;
    memset(h, 0, sizeof *h);
    h->ctrl = c;
    h->addr = addr;
    h->pen = c->fn == 0;

    /* ACPI's HIDD _DSM gives the descriptor register: 0x0001 for the Elan
     * variant and 0x0020 for the Synaptics variant of this Yoga. Try both;
     * the 30-byte descriptor's own length/version validate the choice. */
    static const uint16_t desc_addrs[] = {0x0001, 0x0020};
    uint8_t desc[30];
    int dr = -1, good = 0;
    for (unsigned a = 0; a < ARRAY_LEN(desc_addrs) && !good; a++) {
        for (int tries = 0; tries < 3; tries++) {
            dr = hid_read(h, desc_addrs[a], desc, sizeof desc);
            if (!dr && le16(desc) == 30 && le16(desc + 2) == 0x0100) {
                good = 1;
                break;
            }
            delay_ms(10);
        }
    }
    if (!good) return dr ? dr : -E_INVAL;
    h->d.len = le16(desc + 0);
    h->d.bcd = le16(desc + 2);
    h->d.report_len = le16(desc + 4);
    h->d.report_reg = le16(desc + 6);
    h->d.input_reg = le16(desc + 8);
    h->d.max_input = le16(desc + 10);
    h->d.output_reg = le16(desc + 12);
    h->d.max_output = le16(desc + 14);
    h->d.command_reg = le16(desc + 16);
    h->d.data_reg = le16(desc + 18);
    h->d.vendor = le16(desc + 20);
    h->d.product = le16(desc + 22);
    if (h->d.len != 30 || h->d.bcd != 0x0100 || !h->d.report_reg || !h->d.report_len ||
        h->d.report_len > HID_MAX_REPORT_DESC || !h->d.input_reg || !h->d.command_reg)
        return -E_INVAL;

    int power = hid_power_on(h);
    int reset = hid_reset(h);
    if (power || reset) kprintf("[i2c-hid] %02x: power/reset did not complete\n", addr);
    delay_ms(20);

    uint8_t *rdesc = kmalloc(h->d.report_len);
    if (!rdesc) return -E_NOMEM;
    int r = hid_read(h, h->d.report_reg, rdesc, h->d.report_len);
    if (r) {
        kfree(rdesc);
        return -E_IO;
    }
    r = parse_report_descriptor(h, rdesc, h->d.report_len);
    kfree(rdesc);
    if (r) return r;

    /* Report protocol, no idle reports. */
    (void)hid_command(h, 0x07, 1); /* SET_PROTOCOL, report */
    (void)hid_command(h, 0x05, 0); /* SET_IDLE, 0 */
    h->used = 1;
    kprintf("[i2c-hid] %02x: %04x:%04x, %u-byte report descriptor, %s %s\n", addr, h->d.vendor, h->d.product,
            h->d.report_len, h->rep.absolute ? "absolute" : "relative", h->rep.tip >= 0 ? "touchpad" : "mouse");
    return 0;
}

/* Alder Lake-P/M LPSS I2C functions. Matching 00:15.x by vendor alone would
 * also match unrelated Intel functions on other generations (for example the
 * Haswell/Broadwell Serial IO DMA), so use the I2C device IDs. */
static int i2c_device_id(uint16_t id) {
    switch (id) {
    case 0x51c5: case 0x51c6: case 0x51d8: case 0x51d9:
    case 0x51e8: case 0x51e9: case 0x51ea: case 0x51eb:
    case 0x54c5: case 0x54c6: case 0x54e8: case 0x54e9:
    case 0x54ea: case 0x54eb:
        return 1;
    default:
        return 0;
    }
}

static int ctrl_probe(int fn) {
    struct pci_dev *d = 0;
    for (int i = 0, n = pci_count(); i < n; i++) {
        struct pci_dev *p = pci_get(i);
        if (p && p->bus == 0 && p->dev == 0x15 && p->fn == fn) {
            d = p;
            break;
        }
    }
    if (!d || d->vendor != 0x8086 || d->class != 0x0C || !i2c_device_id(d->device)) return -1;
    int is_io = 0;
    uint64_t bar = pci_bar_ex(d, 0, &is_io);
    if (!bar || is_io || (bar & 0xFFF)) return -1;

    /* This driver uses PIO only. Enable memory decoding but not bus
     * mastering, and restore the original command register on failure. */
    uint16_t orig_cmd = pci_read16(d, PCI_CMD);
    uint16_t cmd = (uint16_t)((orig_cmd | PCI_CMD_MEM) & ~PCI_CMD_BUS_MASTER);
    if (cmd != orig_cmd) pci_write16(d, PCI_CMD, cmd);

    struct i2c_ctrl *c = &ctrls[fn];
    memset(c, 0, sizeof *c);
    c->fn = (uint8_t)fn;
    uint64_t win = I2C_WINDOW + (uint64_t)fn * I2C_STRIDE;
    for (uint64_t o = 0; o < 0x1000; o += PAGE_SIZE) {
        if (vmm_map(vmm_kernel_pml4(), win + o, bar + o, PTE_RW | PTE_NX | PTE_G | PTE_PCD | PTE_PWT))
            goto fail;
        invlpg(win + o);
    }
    c->mmio = (volatile uint8_t *)win;

    /* LPSS private block: hold the function in reset, bring it out, and set
     * the BAR remap value used by the DMA block. */
    uint32_t caps = *(volatile uint32_t *)(c->mmio + LPSS_PRIV + LPSS_CAPS);
    if (((caps >> 4) & 0xF) != 0) goto fail; /* not an I2C LPSS function */
    *(volatile uint32_t *)(c->mmio + LPSS_PRIV + LPSS_RESETS) = 0;
    delay_ms(1);
    *(volatile uint32_t *)(c->mmio + LPSS_PRIV + LPSS_RESETS) = 0x7;
    wr64(c, LPSS_PRIV + LPSS_REMAP, bar);
    delay_ms(1);

    if (i2c_init(c)) goto fail;
    c->ready = 1;
    return 0;

fail:
    pci_write16(d, PCI_CMD, orig_cmd);
    return -1;
}

void i2c_hid_init(void) {
    memset(ctrls, 0, sizeof ctrls);
    memset(hids, 0, sizeof hids);
    last_poll = 0;
    int found = 0;
    for (int fn = 1; fn >= 0; fn--) {
        if (ctrl_probe(fn)) continue;
        static const uint8_t tp_addr[] = {0x2C, 0x15};
        static const uint8_t pen_addr[] = {0x0A};
        if (fn == 1) {
            for (unsigned i = 0; i < ARRAY_LEN(tp_addr); i++)
                if (!hid_parse_one(&ctrls[fn], tp_addr[i])) found++;
        } else {
            for (unsigned i = 0; i < ARRAY_LEN(pen_addr); i++)
                if (!hid_parse_one(&ctrls[fn], pen_addr[i])) found++;
        }
    }
    if (!found) kprintf("[i2c-hid] no HID device found on LPSS I2C\n");
}

void i2c_hid_poll(void) {
    if (ticks - last_poll < 8) return;
    last_poll = ticks;
    for (int i = 0; i < (int)ARRAY_LEN(hids); i++) {
        struct hid_dev *h = &hids[i];
        if (!h->used) continue;
        if (ticks < h->next_try) continue;
        size_t len = h->d.max_input;
        if (len < 2) len = 64;
        if (len > sizeof h->last_input) len = sizeof h->last_input;
        int r = hid_read_input(h, h->last_input, len);
        if (r == -E_TIMEOUT || r == -E_BUSY) {
            if (h->fails < 1000) h->fails++;
            if (!h->fail_logged) {
                h->fail_logged = 1;
                kprintf("[i2c-hid] %02x: input polling timed out; backing off\n", h->addr);
            }
            unsigned shift = h->fails > 7 ? 7 : (unsigned)h->fails;
            uint64_t backoff = 8u << shift;
            if (backoff > 1000) backoff = 1000;
            h->next_try = ticks + backoff;
            continue;
        }
        /* A NACK means the device is alive and has no report; it is not a
         * stuck bus and must not trigger backoff. */
        h->fails = 0;
        h->fail_logged = 0;
        if (r) continue;
        size_t n = le16(h->last_input);
        if (n < 2 || n > len) continue;
        size_t payload = n - 2;
        uint8_t report[64];
        if (payload > sizeof report) payload = sizeof report;
        memcpy(report, h->last_input + 2, payload);
        /* Some devices return the same holding report on every read instead
         * of NAKing. Do not feed it to the pointer twice. */
        if ((int)payload == h->last_report_len && !memcmp(report, h->last_report, payload)) continue;
        memcpy(h->last_report, report, payload);
        h->last_report_len = (int)payload;
        decode_and_push(h, report, payload);
    }
}

/* ---- Self-test --------------------------------------------------------- */

int i2c_hid_selftest(void) {
    /* A minimal boot-mouse style descriptor: 3 buttons, X/Y relative, wheel. */
    static const uint8_t desc[] = {
        0x05, 0x01,       /* Usage Page (Generic Desktop) */
        0x09, 0x02,       /* Usage (Mouse) */
        0xA1, 0x01,       /* Collection (Application) */
        0x09, 0x01,       /* Usage (Pointer) */
        0xA1, 0x00,       /* Collection (Physical) */
        0x05, 0x09,       /* Usage Page (Buttons) */
        0x19, 0x01,       /* Usage Minimum (1) */
        0x29, 0x03,       /* Usage Maximum (3) */
        0x15, 0x00,       /* Logical Minimum (0) */
        0x25, 0x01,       /* Logical Maximum (1) */
        0x95, 0x03,       /* Report Count (3) */
        0x75, 0x01,       /* Report Size (1) */
        0x81, 0x02,       /* Input (Data, Variable, Absolute) */
        0x95, 0x01,       /* Report Count (1) */
        0x75, 0x05,       /* Report Size (5) */
        0x81, 0x01,       /* Input (Constant) */
        0x05, 0x01,       /* Usage Page (Generic Desktop) */
        0x09, 0x30,       /* Usage (X) */
        0x09, 0x31,       /* Usage (Y) */
        0x09, 0x38,       /* Usage (Wheel) */
        0x15, 0x81,       /* Logical Minimum (-127) */
        0x25, 0x7F,       /* Logical Maximum (127) */
        0x75, 0x08,       /* Report Size (8) */
        0x95, 0x03,       /* Report Count (3) */
        0x81, 0x06,       /* Input (Data, Variable, Relative) */
        0xC0,             /* End Collection */
        0xC0,             /* End Collection */
    };
    static struct hid_dev h;
    memset(&h, 0, sizeof h);
    h.seen_input = 1;
    if (parse_report_descriptor(&h, desc, sizeof desc)) return -1;
    if (!h.rep.present || h.rep.x < 0 || h.rep.y < 0 || h.rep.wheel < 0 || h.rep.button[0] < 0) return -1;
    /* Report ID 0: byte 0 buttons, bytes 1..3 X/Y/wheel. */
    uint8_t report[4] = {1, 5, 0xFB, 2};
    struct input_event drain;
    while (input_pop(&drain)) {
    }
    decode_and_push(&h, report, sizeof report);
    struct input_event ev;
    if (!input_pop(&ev)) return -1;
    if (ev.type != IN_MOUSE || ev.buttons != 1 || ev.dx != 5 || ev.dy != -5 || ev.dz != 2) return -1;

    /* A small Windows-Precision-Touchpad style report: contact count, tip,
     * then absolute 16-bit X/Y on the Generic Desktop page. */
    static const uint8_t ptp[] = {
        0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, /* Digitizer / Touch Pad */
        0x85, 0x01,                         /* Report ID 1 */
        0x09, 0x54, 0x15, 0x00, 0x25, 0x05, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, /* Contact Count */
        0x09, 0x42, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, /* Tip Switch */
        0x75, 0x07, 0x95, 0x01, 0x81, 0x01, /* padding */
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x0F,
        0x75, 0x10, 0x95, 0x02, 0x81, 0x02, /* X/Y absolute */
        0xC0,
    };
    memset(&h, 0, sizeof h);
    h.seen_input = 1;
    if (parse_report_descriptor(&h, ptp, sizeof ptp)) {
        kprintf("[i2c-hid] selftest: PTP descriptor parse failed\n");
        return -1;
    }
    if (!h.rep.absolute || h.rep.tip < 0 || h.rep.x < 0 || h.rep.y < 0) {
        kprintf("[i2c-hid] selftest: PTP fields %d abs %d tip %d x %d y %d\n", h.rep.present, h.rep.absolute,
                h.rep.tip, h.rep.x, h.rep.y);
        return -1;
    }
    uint8_t down1[7] = {1, 1, 1, 0x00, 0x01, 0x00, 0x02};
    uint8_t down2[7] = {1, 1, 1, 0x00, 0x02, 0x00, 0x03};
    while (input_pop(&drain)) {
    }
    decode_and_push(&h, down1, sizeof down1);
    decode_and_push(&h, down2, sizeof down2);
    if (!input_pop(&ev)) {
        kprintf("[i2c-hid] selftest: PTP produced no event\n");
        return -1;
    }
    if (ev.type != IN_MOUSE || ev.dx == 0 || ev.dy == 0) {
        kprintf("[i2c-hid] selftest: PTP event type %u dx %d dy %d\n", ev.type, ev.dx, ev.dy);
        return -1;
    }
    return 0;
}
