/*
 * usb.c - polled xHCI host controller and USB device enumeration.
 *
 * The controller is reset, its command/event rings are put in owned pages,
 * and root-port devices are addressed one at a time. There are no interrupts:
 * usb_poll() drains the event ring from the window manager loop, and every
 * synchronous control command has a PIT deadline.
 *
 * Deliberate limits: root ports only (no external hub tier yet), one
 * configuration/interface per device, and only the USB classes a laptop
 * install needs: HID keyboard/mouse and CDC-ECM Ethernet. Every device
 * address, descriptor length, endpoint count and DMA length is checked
 * before it is used.
 */
#include "../include/abi.h"
#include "cpu.h"
#include "drivers.h"
#include "kernel.h"
#include "mm.h"
#include "pci.h"
#include "usb.h"

#define XHCI_WINDOW 0xFFFFFE0003000000ull
#define XHCI_MMIO_SIZE 0x10000ull
#define RING_TRBS 64
#define EVT_TRBS 64
#define CMD_TRBS 64

#define TRB_NORMAL 1
#define TRB_SETUP 2
#define TRB_DATA 3
#define TRB_STATUS 4
#define TRB_LINK 6
#define TRB_ENABLE_SLOT 9
#define TRB_DISABLE_SLOT 10
#define TRB_ADDRESS_DEVICE 11
#define TRB_CONFIGURE_ENDPOINT 12
#define TRB_EVALUATE_CONTEXT 13
#define TRB_STOP_ENDPOINT 15
#define TRB_SET_TR_DEQUEUE 16
#define TRB_TRANSFER_EVENT 32
#define TRB_COMMAND_EVENT 33
#define TRB_PORT_EVENT 34

#define TRB_CYCLE 1u
#define TRB_ISP (1u << 2)
#define TRB_IOC (1u << 5)
#define TRB_IDT (1u << 6)
#define TRB_DIR_IN (1u << 16)
#define TRB_TYPE(t) ((uint32_t)(t) << 10)

/* Operational registers, relative to the capability base. */
#define USBCMD 0x00
#define USBSTS 0x04
#define DNCTRL 0x14
#define CRCR 0x18
#define DCBAAP 0x30
#define CONFIG 0x38
#define PORTSC 0x400
#define PORTPMSC 0x404
#define PORTLI 0x408

#define USBCMD_RS (1u << 0)
#define USBCMD_HCRST (1u << 1)
#define USBCMD_INTE (1u << 2)
#define USBCMD_EWE (1u << 10)
#define USBSTS_HCH (1u << 0)
#define USBSTS_HSE (1u << 2)
#define USBSTS_EINT (1u << 3)
#define USBSTS_PCD (1u << 4)
#define USBSTS_CNR (1u << 11)
#define USBSTS_HCE (1u << 12)

#define PORTSC_CCS (1u << 0)
#define PORTSC_PED (1u << 1)
#define PORTSC_OCA (1u << 3)
#define PORTSC_PR (1u << 4)
#define PORTSC_PLS_MASK (0xFu << 5)
#define PORTSC_PP (1u << 9)
#define PORTSC_SPEED_MASK (0xFu << 10)
#define PORTSC_PIC_MASK (0x3u << 14)
#define PORTSC_LWS (1u << 16)
#define PORTSC_CSC (1u << 17)
#define PORTSC_PEC (1u << 18)
#define PORTSC_WRC (1u << 19)
#define PORTSC_OCC (1u << 20)
#define PORTSC_PRC (1u << 21)
#define PORTSC_PLC (1u << 22)
#define PORTSC_CEC (1u << 23)
#define PORTSC_CAS (1u << 24)
#define PORTSC_WCE (1u << 25)
#define PORTSC_WDE (1u << 26)
#define PORTSC_WOE (1u << 27)
#define PORTSC_CHANGE                                                                                                 \
    (PORTSC_CSC | PORTSC_PEC | PORTSC_WRC | PORTSC_OCC | PORTSC_PRC | PORTSC_PLC | PORTSC_CEC | PORTSC_CAS)

#define PORTSC_SPEED(s) (((s) >> 10) & 0xF)
#define PORTSC_SET_SPEED(s) (((uint32_t)(s) & 0xF) << 10)
/* These are the only PORTSC bits software should write back. PED and PR
 * are write-1-to-clear/initiate, not state to preserve. */
#define PORTSC_RW_KEEP (PORTSC_PP | PORTSC_PIC_MASK | PORTSC_WCE | PORTSC_WDE | PORTSC_WOE)

struct trb {
    uint64_t parameter;
    uint32_t status;
    uint32_t control;
} __attribute__((packed));
typedef char trb_size_check[sizeof(struct trb) == 16 ? 1 : -1];

struct erst_entry {
    uint64_t base;
    uint32_t size;
    uint32_t reserved;
} __attribute__((packed));

static struct {
    volatile uint8_t *mmio;
    uint32_t op, rt, db;
    uint32_t max_ports, max_slots;
    uint64_t dcbaa_phys;
    uint64_t *dcbaa;
    uint32_t scratch_count;
    uint64_t scratch_array_phys;
    struct usb_ring cmd;
    struct usb_ring evt;
    struct trb *evt_trbs;
    uint64_t evt_phys;
    struct erst_entry *erst;
    uint64_t erst_phys;
    volatile int cmd_done;
    volatile uint32_t cmd_code, cmd_slot;
    int failed;
} hc;

static struct usb_device devices[USB_MAX_DEVICES];

static inline uint32_t rd(uint32_t off) { return *(volatile uint32_t *)(hc.mmio + off); }
static inline void wr(uint32_t off, uint32_t v) { *(volatile uint32_t *)(hc.mmio + off) = v; }
static inline void wr64(uint32_t off, uint64_t v) { *(volatile uint64_t *)(hc.mmio + off) = v; }
static inline void barrier(void) { __asm__ volatile("mfence" ::: "memory"); }
static inline void pause(void) { __asm__ volatile("pause"); }

static uint64_t deadline_ms(int ms) { return aether_pit_ms() + (uint64_t)(ms < 0 ? 0 : ms) + 1; }

static int wait_bits(uint32_t off, uint32_t mask, uint32_t value, int ms) {
    uint64_t until = deadline_ms(ms);
    while ((rd(off) & mask) != value) {
        if (aether_pit_ms() >= until) return -E_TIMEOUT;
        pause();
    }
    return 0;
}

static void *alloc_page(uint64_t *phys) {
    uint64_t p = pmm_alloc();
    if (!p) return 0;
    *phys = p;
    return P2V(p);
}

/* ---- Rings ------------------------------------------------------------- */

static void ring_init(struct usb_ring *r, void *virt, uint64_t phys) {
    memset(virt, 0, PAGE_SIZE);
    r->virt = virt;
    r->phys = phys;
    r->enq = 0;
    r->cycle = 1;
    struct trb *link = (struct trb *)virt + RING_TRBS - 1;
    link->parameter = phys;
    link->status = 0;
    link->control = TRB_CYCLE | (1u << 1) | TRB_TYPE(TRB_LINK); /* cycle, TC */
}

static uint64_t ring_push(struct usb_ring *r, struct trb t) {
    struct trb *ring = r->virt;
    if (r->enq >= RING_TRBS - 1) {
        struct trb *link = &ring[RING_TRBS - 1];
        link->control = (link->control & ~TRB_CYCLE) | r->cycle;
        r->enq = 0;
        r->cycle ^= 1;
    }
    t.control = (t.control & ~TRB_CYCLE) | r->cycle;
    uint64_t phys = r->phys + (uint64_t)r->enq * sizeof(struct trb);
    ring[r->enq++] = t;
    return phys;
}

static void doorbell(uint8_t slot, uint8_t dci) {
    barrier();
    wr(hc.db + 4u * slot, dci); /* DB Target: endpoint ID in bits 0-7 */
}

/* ---- Event processing -------------------------------------------------- */

static struct usb_device *device_for_slot(uint8_t slot) {
    for (int i = 0; i < USB_MAX_DEVICES; i++)
        if (devices[i].used && devices[i].slot == slot) return &devices[i];
    return 0;
}

static struct usb_endpoint *endpoint_for_dci(struct usb_device *d, uint8_t dci) {
    if (dci == 1) return &d->ep0;
    for (int i = 0; i < d->neps; i++)
        if (d->eps[i].dci == dci) return &d->eps[i];
    return 0;
}

static uint32_t portsc_off(unsigned port) { return hc.op + PORTSC + (port - 1) * 0x10; }

static void clear_port_changes(unsigned port) {
    uint32_t off = portsc_off(port);
    uint32_t v = rd(off);
    /* PED and PR are write-1-to-clear/initiate; never write them back.
     * Preserve PP/PIC and acknowledge the change bits. */
    wr(off, (v & PORTSC_RW_KEEP) | PORTSC_CHANGE);
}

static void port_power_on(unsigned port) {
    uint32_t off = portsc_off(port);
    uint32_t v = rd(off);
    wr(off, (v & PORTSC_RW_KEEP) | PORTSC_PP);
}

static void process_events(void) {
    if (!hc.evt_trbs) return;
    for (int budget = 0; budget < EVT_TRBS; budget++) {
        struct trb *e = &hc.evt_trbs[hc.evt.enq];
        if ((e->control & TRB_CYCLE) != hc.evt.cycle) break;
        uint32_t type = (e->control >> 10) & 0x3F;
        uint32_t code = (e->status >> 24) & 0xFF;
        if (type == TRB_COMMAND_EVENT) {
            hc.cmd_code = code;
            hc.cmd_slot = (e->control >> 24) & 0xFF;
            hc.cmd_done = 1;
        } else if (type == TRB_TRANSFER_EVENT) {
            uint8_t slot = (uint8_t)((e->control >> 24) & 0xFF);
            uint8_t dci = (uint8_t)((e->control >> 16) & 0x1F);
            struct usb_device *d = device_for_slot(slot);
            struct usb_endpoint *ep = d ? endpoint_for_dci(d, dci) : 0;
            if (ep && ep->pending) {
                size_t residual = e->status & 0xFFFFFF;
                size_t sent = ep->submitted;
                uint64_t trb = e->parameter;
                if (ep->data_trb_phys && trb == ep->data_trb_phys) {
                    /* Control data stage: ISP makes a short packet report
                     * here, before the status-stage completion. */
                    ep->data_seen = 1;
                    ep->data_actual = residual < sent ? sent - residual : 0;
                    ep->code = (int)code;
                    if (code != 1 && code != 13) {
                        ep->status = -E_IO;
                        ep->pending = 0;
                    }
                } else if (trb == ep->last_trb_phys) {
                    ep->actual = ep->data_trb_phys
                                     ? (ep->data_seen ? ep->data_actual : sent)
                                     : (residual < sent ? sent - residual : 0);
                    ep->code = (int)code;
                    ep->status = (code == 1 || code == 13) ? 1 : -E_IO;
                    ep->pending = 0;
                }
                /* A completion for any other TRB is late or stale; do not
                 * credit it to the request that is pending now. */
            }
        } else if (type == TRB_PORT_EVENT) {
            unsigned port = (unsigned)((e->parameter >> 24) & 0xFF);
            if (port && port <= hc.max_ports) clear_port_changes(port);
        }
        hc.evt.enq++;
        if (hc.evt.enq == EVT_TRBS) {
            hc.evt.enq = 0;
            hc.evt.cycle ^= 1;
        }
    }
    /* ERDP points at the next event; EHB is clear after acknowledging. */
    wr64(hc.rt + 0x38, hc.evt.phys + (uint64_t)hc.evt.enq * sizeof(struct trb));
}

/* ---- Commands ---------------------------------------------------------- */

static int command_ex(uint32_t type, uint64_t parameter, uint8_t slot, uint8_t dci) {
    if (hc.failed) return -E_IO;
    if (slot > 63 || dci > 31) return -E_INVAL;
    hc.cmd_done = 0;
    hc.cmd_code = 0;
    struct trb t = {.parameter = parameter, .status = 0,
                    .control = TRB_TYPE(type) | ((uint32_t)slot << 24) | ((uint32_t)dci << 16)};
    ring_push(&hc.cmd, t);
    doorbell(0, 0);
    uint64_t until = deadline_ms(USB_TIMEOUT_MS);
    while (!hc.cmd_done) {
        process_events();
        if (aether_pit_ms() >= until) {
            kprintf("[usb] command %u timed out\n", type);
            hc.failed = 1; /* do not lap a command ring the controller stopped reading */
            return -E_TIMEOUT;
        }
        pause();
    }
    return hc.cmd_code == 1 ? 0 : -E_IO;
}

static int command(uint32_t type, uint64_t parameter, uint8_t slot) {
    return command_ex(type, parameter, slot, 0);
}

/* ---- Endpoint transfers ------------------------------------------------ */

/* Stop a timed-out endpoint and put its ring back at a known TRB. A late
 * completion from the old request cannot then be credited to the next one. */
int usb_ep_abort(struct usb_device *d, struct usb_endpoint *ep) {
    if (!d || !ep || !ep->ring.virt) return -E_INVAL;
    int stop = command_ex(TRB_STOP_ENDPOINT, 0, d->slot, ep->dci);
    ring_init(&ep->ring, ep->ring.virt, ep->ring.phys);
    int dequeue = command_ex(TRB_SET_TR_DEQUEUE, ep->ring.phys | 1, d->slot, ep->dci);
    ep->pending = 0;
    ep->status = 0;
    ep->last_trb_phys = 0;
    ep->data_trb_phys = 0;
    ep->data_seen = 0;
    return (stop && dequeue) ? stop : 0;
}

static int ep_submit_raw(struct usb_device *d, struct usb_endpoint *ep, size_t len) {
    if (!d || !ep || !ep->ring.virt || ep->pending) return -E_BUSY;
    if (len > ep->buf_len) return -E_INVAL;
    ep->submitted = len;
    ep->actual = 0;
    ep->status = 0;
    ep->data_trb_phys = 0;
    ep->data_seen = 0;
    ep->pending = 1;
    struct trb t = {.parameter = ep->buf_phys, .status = (uint32_t)len,
                    .control = TRB_TYPE(TRB_NORMAL) | (ep->in ? TRB_DIR_IN : 0) | TRB_IOC};
    ep->last_trb_phys = ring_push(&ep->ring, t);
    doorbell(d->slot, ep->dci);
    return 0;
}

int usb_ep_submit(struct usb_device *d, struct usb_endpoint *ep, const void *data, size_t len) {
    if (!d || !ep) return -E_INVAL;
    if (ep->pending) return -E_BUSY;
    if (len > ep->buf_len) return -E_INVAL;
    if (!ep->in && len && data) memcpy(ep->buf, data, len);
    return ep_submit_raw(d, ep, len);
}

int usb_ep_complete(struct usb_device *d, struct usb_endpoint *ep, void *data, size_t max, size_t *actual) {
    (void)d;
    process_events();
    if (!ep) return 0;
    if (ep->pending || ep->status == 0) return 0;
    int status = ep->status;
    ep->status = 0;
    if (status < 0) return status;
    size_t len = ep->actual;
    if (len > ep->buf_len) len = ep->buf_len;
    if (len > max) len = max;
    if (ep->in && len && data) memcpy(data, ep->buf, len);
    if (actual) *actual = len;
    return 1;
}

int usb_ep_sync(struct usb_device *d, struct usb_endpoint *ep, const void *tx, void *rx, size_t len, int timeout_ms) {
    int r = usb_ep_submit(d, ep, ep->in ? 0 : tx, len);
    if (r) return r;
    uint64_t until = deadline_ms(timeout_ms);
    for (;;) {
        size_t actual = 0;
        r = usb_ep_complete(d, ep, rx, len, &actual);
        if (r) return r < 0 ? r : 0;
        if (aether_pit_ms() >= until) {
            usb_ep_abort(d, ep);
            return -E_TIMEOUT;
        }
        pause();
    }
}

/* ---- Control transfers ------------------------------------------------- */

static int control(struct usb_device *d, uint8_t type, uint8_t req, uint16_t value, uint16_t index, void *buf,
                   uint16_t len) {
    if (!d || len > PAGE_SIZE) return -E_INVAL;
    int in = (type & 0x80) != 0;
    if (len && in && !buf) return -E_INVAL;
    if (len && !in) memcpy(d->scratch, buf, len);
    if (in) memset(d->scratch, 0, PAGE_SIZE); /* no stale descriptor bytes */

    uint64_t setup = (uint64_t)type | ((uint64_t)req << 8) | ((uint64_t)value << 16) | ((uint64_t)index << 32) |
                     ((uint64_t)len << 48);
    /* The setup packet is immediate data (IDT); xHCI rejects a setup TRB
     * without it. TRT names the data stage direction. */
    struct trb s = {.parameter = setup, .status = 8,
                    .control = TRB_TYPE(TRB_SETUP) | TRB_IDT | ((uint32_t)(len ? (in ? 3 : 2) : 0) << 16)};
    struct trb data = {.parameter = d->scratch_phys, .status = len,
                       .control = TRB_TYPE(TRB_DATA) | (in ? TRB_DIR_IN : 0) | TRB_ISP};
    struct trb status = {.parameter = 0, .status = 0,
                         .control = TRB_TYPE(TRB_STATUS) | (in ? 0 : TRB_DIR_IN) | TRB_IOC};

    d->ep0.buf = d->scratch;
    d->ep0.buf_phys = d->scratch_phys;
    d->ep0.buf_len = PAGE_SIZE;
    d->ep0.in = in;
    d->ep0.submitted = len;
    d->ep0.actual = 0;
    d->ep0.data_actual = 0;
    d->ep0.data_seen = 0;
    d->ep0.status = 0;
    d->ep0.pending = 1;

    ring_push(&d->ep0.ring, s);
    d->ep0.data_trb_phys = 0;
    if (len) d->ep0.data_trb_phys = ring_push(&d->ep0.ring, data);
    d->ep0.last_trb_phys = ring_push(&d->ep0.ring, status);
    doorbell(d->slot, 1);

    uint64_t until = deadline_ms(USB_TIMEOUT_MS);
    while (d->ep0.pending) {
        process_events();
        if (aether_pit_ms() >= until) {
            usb_ep_abort(d, &d->ep0);
            kprintf("[usb] slot %u: control %02x/%02x timed out\n", d->slot, type, req);
            return -E_TIMEOUT;
        }
        pause();
    }
    int st = d->ep0.status;
    d->ep0.status = 0;
    if (st < 0) {
        kprintf("[usb] slot %u: control %02x/%02x failed (code %d)\n", d->slot, type, req, d->ep0.code);
        usb_ep_abort(d, &d->ep0);
        return st;
    }
    size_t actual = d->ep0.actual;
    if (actual > len) actual = len;
    if (in && len && buf) memcpy(buf, d->scratch, actual);
    return (int)actual;
}

int usb_control(struct usb_device *d, uint8_t type, uint8_t req, uint16_t value, uint16_t index, void *buf,
                uint16_t len) {
    return control(d, type, req, value, index, buf, len);
}

int usb_string(struct usb_device *d, uint8_t index, char *out, size_t max) {
    if (!index || max < 2) return -E_INVAL;
    uint8_t desc[4];
    int n = control(d, 0x80, 6, (uint16_t)(0x0300 | index), 0x0409, desc, 2);
    if (n < 2) return -E_IO;
    unsigned len = desc[0];
    if (len < 2 || len > 255 || len > PAGE_SIZE) return -E_INVAL;
    uint8_t *tmp = kmalloc(len);
    if (!tmp) return -E_NOMEM;
    n = control(d, 0x80, 6, (uint16_t)(0x0300 | index), 0x0409, tmp, (uint16_t)len);
    if (n < 2) {
        kfree(tmp);
        return -E_IO;
    }
    size_t o = 0;
    for (unsigned i = 2; i + 1 < (unsigned)n && o + 1 < max; i += 2) {
        uint16_t c = (uint16_t)(tmp[i] | tmp[i + 1] << 8);
        out[o++] = c < 128 ? (char)c : '?';
    }
    out[o] = 0;
    kfree(tmp);
    return 0;
}

/* ---- Descriptor parsing ------------------------------------------------ */

struct config_info {
    uint8_t config_value;
    int hid;
    uint8_t hid_iface, hid_alt, hid_proto, hid_ep, hid_interval;
    uint16_t hid_mps;
    int ecm, ecm_ctrl_ok;
    uint8_t ecm_ctrl, ecm_data, ecm_alt, ecm_in, ecm_out, mac_index;
    uint16_t ecm_in_mps, ecm_out_mps;
    int ms; /* USB mass storage, bulk-only transport (class 8/6/50) */
    uint8_t ms_iface, ms_alt, ms_in, ms_out;
    uint16_t ms_in_mps, ms_out_mps;
};

static int is_hid_boot(uint8_t cls, uint8_t sub, uint8_t proto) {
    return cls == 3 && sub == 1 && (proto == 1 || proto == 2);
}

static void parse_config(const uint8_t *buf, size_t len, struct config_info *ci) {
    memset(ci, 0, sizeof *ci);
    uint8_t iface_cls = 0, iface_sub = 0, iface_proto = 0, iface_num = 0, iface_alt = 0;
    int iface_ecm = 0, iface_data = 0, iface_ms = 0;
    for (size_t off = 0; off + 2 <= len;) {
        uint8_t blen = buf[off], btype = buf[off + 1];
        if (blen < 2 || off + blen > len) break;
        const uint8_t *p = buf + off;
        if (btype == 2 && blen >= 6) {
            ci->config_value = p[5];
        } else if (btype == 4 && blen >= 9) {
            iface_num = p[2];
            iface_alt = p[3];
            iface_cls = p[5];
            iface_sub = p[6];
            iface_proto = p[7];
            iface_ecm = iface_cls == 2 && iface_sub == 6 && iface_proto == 0;
            iface_data = iface_cls == 10;
            /* Bulk-only transport: mass storage, SCSI transparent command
             * set, BBB protocol. Other protocols (CBI/UAS) are not driven. */
            iface_ms = iface_cls == 8 && iface_sub == 6 && iface_proto == 0x50;
            if (iface_ms) {
                ci->ms = 1;
                ci->ms_iface = iface_num;
                ci->ms_alt = iface_alt;
            }
            if (iface_ecm) {
                ci->ecm_ctrl_ok = 1;
                ci->ecm_ctrl = iface_num;
            }
            if (iface_data) ci->ecm_data = iface_num;
            if (is_hid_boot(iface_cls, iface_sub, iface_proto)) {
                ci->hid = 1;
                ci->hid_iface = iface_num;
                ci->hid_alt = iface_alt;
                ci->hid_proto = iface_proto;
            }
        } else if (btype == 5 && blen >= 7) {
            uint8_t addr = p[2], attr = p[3];
            uint16_t mps = (uint16_t)(p[4] | p[5] << 8);
            mps &= 0x7FF;
            if ((attr & 3) == 3 && (addr & 0x80) && is_hid_boot(iface_cls, iface_sub, iface_proto)) {
                ci->hid = 1;
                ci->hid_iface = iface_num;
                ci->hid_alt = iface_alt;
                ci->hid_proto = iface_proto;
                ci->hid_ep = addr & 0x0F;
                ci->hid_mps = mps;
                ci->hid_interval = p[6];
            }
            if ((attr & 3) == 2 && iface_data) {
                ci->ecm = 1;
                ci->ecm_data = iface_num;
                ci->ecm_alt = iface_alt;
                if (addr & 0x80) {
                    ci->ecm_in = addr & 0x0F;
                    ci->ecm_in_mps = mps;
                } else {
                    ci->ecm_out = addr & 0x0F;
                    ci->ecm_out_mps = mps;
                }
            }
            if ((attr & 3) == 2 && iface_ms) {
                ci->ms = 1;
                if (addr & 0x80) {
                    ci->ms_in = addr & 0x0F;
                    ci->ms_in_mps = mps;
                } else {
                    ci->ms_out = addr & 0x0F;
                    ci->ms_out_mps = mps;
                }
            }
        } else if (btype == 0x24 && blen >= 13 && p[2] == 0x0F) {
            ci->mac_index = p[3];
        }
        off += blen;
    }
    if (!ci->hid || !ci->hid_ep || !ci->hid_mps) ci->hid = 0;
    /* A CDC data interface alone is not ECM: require the Ethernet control
     * interface too, so a RNDIS configuration is never mistaken for one. */
    if (!ci->ecm_ctrl_ok || !ci->ecm || !ci->ecm_in || !ci->ecm_out) ci->ecm = 0;
    if (!ci->ms || !ci->ms_in || !ci->ms_out) ci->ms = 0;
}

/* ---- xHCI bring-up ----------------------------------------------------- */

static int bios_handoff(uint32_t hccparams1) {
    uint32_t off = (hccparams1 >> 16) & 0x3FF;
    for (int n = 0; off && n < 16; n++) {
        uint32_t cap = rd(off * 4);
        uint32_t id = cap & 0xFF;
        uint32_t next = (cap >> 8) & 0xFF;
        if (id == 1) { /* USB Legacy Support */
            uint32_t v = rd(off * 4);
            v |= 1u << 24; /* OS owned */
            v &= ~((1u << 16) | (1u << 17) | (1u << 18) | (1u << 19)); /* BIOS owned + SMI enables */
            wr(off * 4, v);
            uint64_t until = deadline_ms(1000);
            while ((rd(off * 4) & (1u << 16)) && aether_pit_ms() < until) pause();
            if (rd(off * 4) & (1u << 16)) kprintf("[usb] BIOS did not release the controller\n");
            return 0;
        }
        off = next;
    }
    return 0;
}

static int controller_init(struct pci_dev *pci) {
    int is_io = 0;
    uint64_t bar = pci_bar_ex(pci, 0, &is_io);
    if (!bar || is_io || (bar & 0xF)) {
        kprintf("[usb] xHCI BAR0 is not usable memory\n");
        return -1;
    }
    pci_enable_bus_master_mem(pci);
    for (uint64_t o = 0; o < XHCI_MMIO_SIZE; o += PAGE_SIZE) {
        if (vmm_map(vmm_kernel_pml4(), XHCI_WINDOW + o, bar + o,
                    PTE_RW | PTE_NX | PTE_G | PTE_PCD | PTE_PWT))
            return -1;
        invlpg(XHCI_WINDOW + o);
    }
    hc.mmio = (volatile uint8_t *)XHCI_WINDOW;
    uint8_t caplen = *(volatile uint8_t *)hc.mmio;
    uint32_t hcsparams1 = rd(0x04);
    uint32_t hcsparams2 = rd(0x08);
    uint32_t hccparams1 = rd(0x10);
    hc.max_ports = (hcsparams1 >> 24) & 0xFF;
    hc.max_slots = hcsparams1 & 0xFF;
    hc.db = rd(0x14);
    hc.rt = rd(0x18);
    hc.op = caplen;
    if (!hc.max_ports || !hc.max_slots || hc.max_ports > 64 || hc.max_slots > 64 || hc.db < hc.op || hc.rt < hc.op) {
        kprintf("[usb] xHCI capabilities look wrong (ports %u slots %u)\n", hc.max_ports, hc.max_slots);
        return -1;
    }
    /* The context size is either 32 or 64 bytes and changes every context
     * offset. This driver implements the 32-byte layout. */
    if (hccparams1 & (1u << 2)) {
        kprintf("[usb] xHCI uses 64-byte contexts; not supported\n");
        return -1;
    }
    hc.scratch_count = ((hcsparams2 >> 21) & 0x1F) | (((hcsparams2 >> 27) & 0x1F) << 5);
    if (hc.scratch_count > 1023) {
        kprintf("[usb] xHCI reports too many scratchpads (%u)\n", hc.scratch_count);
        return -1;
    }

    bios_handoff(hccparams1);

    /* Stop the controller before touching its rings. */
    wr(hc.op + USBCMD, rd(hc.op + USBCMD) & ~USBCMD_RS);
    if (wait_bits(hc.op + USBSTS, USBSTS_HCH, USBSTS_HCH, 100)) {
        kprintf("[usb] xHCI did not halt\n");
        return -1;
    }
    wr(hc.op + USBCMD, USBCMD_HCRST);
    if (wait_bits(hc.op + USBCMD, USBCMD_HCRST, 0, 1000)) {
        kprintf("[usb] xHCI reset timed out\n");
        return -1;
    }
    if (wait_bits(hc.op + USBSTS, USBSTS_CNR, 0, 1000)) {
        kprintf("[usb] xHCI still not ready after reset\n");
        return -1;
    }

    /* PAGESIZE is a bitmask: bit 0 means 4 KiB pages are supported. Select
     * them explicitly; the kernel has no larger contiguous-DMA contract. */
    uint32_t pagesize = rd(hc.op + 0x08);
    if (!(pagesize & 1)) {
        kprintf("[usb] xHCI does not support 4 KiB pages (mask %x)\n", pagesize);
        return -1;
    }
    wr(hc.op + 0x08, 1);

    hc.dcbaa = alloc_page(&hc.dcbaa_phys);
    hc.cmd.virt = alloc_page(&hc.cmd.phys);
    hc.evt_trbs = alloc_page(&hc.evt_phys);
    hc.erst = alloc_page(&hc.erst_phys);
    if (!hc.dcbaa || !hc.cmd.virt || !hc.evt_trbs || !hc.erst) return -E_NOMEM;
    if (hc.scratch_count) {
        size_t bytes = (size_t)hc.scratch_count * sizeof(uint64_t);
        size_t pages = (bytes + PAGE_SIZE - 1) / PAGE_SIZE;
        hc.scratch_array_phys = pmm_alloc_contig(pages);
        if (!hc.scratch_array_phys) {
            kprintf("[usb] xHCI scratchpad array allocation failed\n");
            return -E_NOMEM;
        }
        uint64_t *array = P2V(hc.scratch_array_phys);
        for (uint32_t i = 0; i < hc.scratch_count; i++) {
            uint64_t p = pmm_alloc();
            if (!p) {
                kprintf("[usb] xHCI scratchpad page allocation failed\n");
                return -E_NOMEM;
            }
            array[i] = p;
        }
        hc.dcbaa[0] = hc.scratch_array_phys;
        kprintf("[usb] xHCI %u scratchpad buffers\n", hc.scratch_count);
    }
    ring_init(&hc.cmd, hc.cmd.virt, hc.cmd.phys);
    hc.evt.virt = hc.evt_trbs;
    hc.evt.phys = hc.evt_phys;
    hc.evt.enq = 0;
    hc.evt.cycle = 1;
    hc.erst[0].base = hc.evt_phys;
    hc.erst[0].size = EVT_TRBS;
    hc.erst[0].reserved = 0;

    wr(hc.op + CONFIG, hc.max_slots);
    wr64(hc.op + DCBAAP, hc.dcbaa_phys);
    wr64(hc.op + CRCR, hc.cmd.phys | 1);
    wr(hc.op + DNCTRL, 0);

    /* Interrupter 0 starts 0x20 bytes into the runtime register set. */
    wr(hc.rt + 0x20, 0); /* IMAN */
    wr(hc.rt + 0x24, 0); /* IMOD */
    wr(hc.rt + 0x28, 1); /* ERSTSZ */
    wr64(hc.rt + 0x30, hc.erst_phys);
    wr64(hc.rt + 0x38, hc.evt_phys | (1u << 3)); /* ERDP, EHB */

    wr(hc.op + USBCMD, USBCMD_RS | USBCMD_EWE);
    if (wait_bits(hc.op + USBSTS, USBSTS_HCH, 0, 1000)) {
        kprintf("[usb] xHCI did not start\n");
        return -1;
    }
    if (rd(hc.op + USBSTS) & USBSTS_HCE) {
        kprintf("[usb] xHCI reported a host controller error\n");
        return -1;
    }

    kprintf("[usb] xHCI %02x:%02x.%u, %u ports, %u slots\n", pci->bus, pci->dev, pci->fn, hc.max_ports, hc.max_slots);
    return 0;
}

static int port_reset(unsigned port, uint8_t *speed) {
    uint32_t off = portsc_off(port);
    uint32_t v = rd(off);
    if (!(v & PORTSC_CCS)) return -1;
    if (!(v & PORTSC_PP)) {
        port_power_on(port);
        uint64_t until = deadline_ms(100);
        while (!(rd(off) & PORTSC_PP) && aether_pit_ms() < until) pause();
        v = rd(off);
    }
    clear_port_changes(port); /* clear stale changes */
    v = rd(off);
    wr(off, (v & PORTSC_RW_KEEP) | PORTSC_PR);
    uint64_t until = deadline_ms(500);
    while (aether_pit_ms() < until) {
        v = rd(off);
        if (v & PORTSC_PRC) break;
        if (!(v & PORTSC_PR) && (v & PORTSC_PED)) break;
        pause();
    }
    clear_port_changes(port);
    v = rd(off);
    if (!(v & PORTSC_CCS) || !(v & PORTSC_PED)) return -1;
    *speed = (uint8_t)PORTSC_SPEED(v);
    return 0;
}

static void set_slot_context(uint32_t *ctx, struct usb_device *d, uint8_t entries) {
    /* Input context layout: 8 dwords control, 8 dwords slot, then one
     * 8-dword context per endpoint. */
    uint32_t *s = ctx + 8;
    s[0] = ((uint32_t)d->speed << 20) | ((uint32_t)entries << 27);
    s[1] = (uint32_t)d->port << 16;
    s[2] = 0;
    s[3] = 0;
}

static uint32_t ep_type_bits(const struct usb_endpoint *ep) {
    unsigned t;
    if (ep->type == USB_EP_CONTROL) t = 4;
    else if (ep->type == USB_EP_BULK) t = ep->in ? 6 : 2;
    else t = ep->in ? 7 : 3;
    return t << 3;
}

static void set_ep_context(uint32_t *ctx, uint8_t dci, struct usb_endpoint *ep, uint8_t interval) {
    uint32_t *e = ctx + 8 * ((uint32_t)dci + 1);
    e[0] = 1 | ((uint32_t)interval << 16); /* running */
    e[1] = ep_type_bits(ep) | ((uint32_t)ep->mps << 16);
    e[2] = (uint32_t)(ep->ring.phys & ~0xFull) | 1; /* DCS */
    e[3] = (uint32_t)(ep->ring.phys >> 32);
    e[4] = 8; /* average TRB length */
}

static int endpoint_init(struct usb_device *d, struct usb_endpoint *ep, uint8_t number, int in, uint8_t type,
                         uint16_t mps, uint8_t interval) {
    if (d->neps >= USB_MAX_EPS) return -E_LIMIT;
    memset(ep, 0, sizeof *ep);
    ep->number = number;
    ep->in = in ? 1 : 0;
    ep->type = type;
    ep->mps = mps ? mps : 8;
    ep->interval = interval;
    ep->dci = (uint8_t)(2 * number + ep->in);
    if (!alloc_page(&ep->ring.phys)) return -E_NOMEM;
    ep->ring.virt = P2V(ep->ring.phys);
    if (!alloc_page(&ep->buf_phys)) return -E_NOMEM;
    ep->buf = P2V(ep->buf_phys);
    ep->buf_len = PAGE_SIZE;
    ring_init(&ep->ring, ep->ring.virt, ep->ring.phys);
    return 0;
}

static uint8_t endpoint_interval(uint8_t speed, uint8_t type, uint8_t binterval) {
    if (type != USB_EP_INTERRUPT) return 0; /* NAK interval is irrelevant for bulk */
    if (speed == USB_SPEED_HIGH || speed == USB_SPEED_SUPER) {
        if (!binterval) binterval = 1;
        if (binterval > 16) binterval = 16;
        return (uint8_t)(binterval - 1);
    }
    /* Full/low-speed bInterval is in frames; the xHCI field is an exponent
     * of 125 us microframes, clamped to 3..10 (1 ms .. 128 ms). */
    unsigned desc = (binterval ? binterval : 1u) * 8u;
    unsigned exp = 0;
    while (exp < 15 && (1u << (exp + 1)) <= desc) exp++;
    if (exp < 3) exp = 3;
    if (exp > 10) exp = 10;
    return (uint8_t)exp;
}

static int configure_endpoints(struct usb_device *d, struct usb_endpoint *eps, int n) {
    memset(d->inctx, 0, PAGE_SIZE);
    uint32_t *ctx = (uint32_t *)d->inctx;
    uint32_t add = 1; /* slot context */
    uint8_t highest = 1;
    for (int i = 0; i < n; i++) {
        add |= 1u << eps[i].dci;
        if (eps[i].dci > highest) highest = eps[i].dci;
    }
    ctx[1] = add;
    set_slot_context(ctx, d, highest);
    for (int i = 0; i < n; i++)
        set_ep_context(ctx, eps[i].dci, &eps[i], endpoint_interval(d->speed, eps[i].type, eps[i].interval));
    int r = command(TRB_CONFIGURE_ENDPOINT, d->inctx_phys, d->slot);
    if (r) kprintf("[usb] slot %u: configure endpoints failed\n", d->slot);
    return r;
}

static int read_device_descriptor(struct usb_device *d) {
    uint8_t desc[18];
    int n = control(d, 0x80, 6, 0x0100, 0, desc, 8);
    if (n < 8) return -E_IO;
    uint16_t mps = d->speed == USB_SPEED_SUPER ? (uint16_t)(1u << (desc[7] & 0xF)) : desc[7];
    if (!mps || mps > 512 || (d->speed != USB_SPEED_SUPER && mps > 64)) return -E_INVAL;
    if (mps != d->ep0_mps) {
        d->ep0_mps = mps;
        memset(d->inctx, 0, PAGE_SIZE);
        uint32_t *ctx = (uint32_t *)d->inctx;
        ctx[1] = 3; /* slot + EP0 */
        set_slot_context(ctx, d, 1);
        d->ep0.mps = mps;
        /* Point EP0 at the next free TRB; its ring has already carried the
         * first descriptor request. */
        uint64_t next = d->ep0.ring.phys + (uint64_t)d->ep0.ring.enq * sizeof(struct trb);
        ctx[16] = 1;
        ctx[17] = (uint32_t)mps << 16;
        ctx[18] = (uint32_t)(next & ~0xFull) | (d->ep0.ring.cycle & 1);
        ctx[19] = (uint32_t)(next >> 32);
        ctx[20] = 8;
        int r = command(TRB_EVALUATE_CONTEXT, d->inctx_phys, d->slot);
        if (r) {
            kprintf("[usb] slot %u: EP0 max-packet update failed\n", d->slot);
            return r;
        }
    }
    n = control(d, 0x80, 6, 0x0100, 0, desc, sizeof desc);
    if (n < 18) return -E_IO;
    d->vendor = (uint16_t)(desc[8] | desc[9] << 8);
    d->product = (uint16_t)(desc[10] | desc[11] << 8);
    d->configs = desc[17] ? desc[17] : 1;
    return 0;
}

static int read_one_config(struct usb_device *d, uint8_t index, struct config_info *ci) {
    uint8_t head[9];
    int n = control(d, 0x80, 6, (uint16_t)(0x0200 | index), 0, head, sizeof head);
    if (n < 9 || head[0] < 9) return -E_IO;
    unsigned total = (unsigned)(head[2] | head[3] << 8);
    if (total < 9 || total > 4096) return -E_INVAL;
    uint8_t *buf = kmalloc(total);
    if (!buf) return -E_NOMEM;
    n = control(d, 0x80, 6, (uint16_t)(0x0200 | index), 0, buf, (uint16_t)total);
    if (n < 9) {
        kfree(buf);
        return -E_IO;
    }
    parse_config(buf, (size_t)n, ci);
    kfree(buf);
    return 0;
}

/* A device may offer several configurations (QEMU's USB net exposes RNDIS
 * first and CDC-ECM second). Walk them and take the first one we can drive. */
static int read_config_descriptor(struct usb_device *d, struct config_info *out) {
    struct config_info first, ci;
    int have_first = 0;
    for (uint8_t i = 0; i < d->configs && i < 4; i++) {
        if (read_one_config(d, i, &ci)) continue;
        if (!have_first) {
            first = ci;
            have_first = 1;
        }
        if (ci.hid || ci.ecm || ci.ms) {
            *out = ci;
            return 0;
        }
    }
    if (!have_first) return -E_IO;
    *out = first;
    return 0;
}

static int address_device(struct usb_device *d) {
    memset(d->inctx, 0, PAGE_SIZE);
    memset(d->devctx, 0, PAGE_SIZE);
    uint32_t *ctx = (uint32_t *)d->inctx;
    ctx[1] = 3; /* slot + EP0 */
    set_slot_context(ctx, d, 1);
    d->ep0.dci = 1;
    d->ep0.number = 0;
    d->ep0.in = 1;
    d->ep0.type = USB_EP_CONTROL;
    d->ep0.mps = d->ep0_mps;
    d->ep0.buf = d->scratch;
    d->ep0.buf_phys = d->scratch_phys;
    d->ep0.buf_len = PAGE_SIZE;
    set_ep_context(ctx, 1, &d->ep0, 0);
    hc.dcbaa[d->slot] = d->devctx_phys;
    int r = command(TRB_ADDRESS_DEVICE, d->inctx_phys, d->slot);
    if (r) {
        kprintf("[usb] slot %u: address device failed (code %u)\n", d->slot, hc.cmd_code);
        return r;
    }
    kprintf("[usb] slot %u: device addressed\n", d->slot);
    return r;
}

static void enumerate_port(unsigned port, uint8_t speed) {
    int slotidx = -1;
    for (int i = 0; i < USB_MAX_DEVICES; i++)
        if (!devices[i].used) {
            slotidx = i;
            break;
        }
    if (slotidx < 0) {
        kprintf("[usb] port %u: no free device slot\n", port);
        return;
    }
    struct usb_device *d = &devices[slotidx];
    memset(d, 0, sizeof *d);
    d->port = (uint8_t)port;
    d->speed = speed;
    d->ep0_mps = speed == USB_SPEED_SUPER ? 512 : speed == USB_SPEED_HIGH ? 64 : 8;
    if (!alloc_page(&d->scratch_phys) || !alloc_page(&d->inctx_phys) || !alloc_page(&d->devctx_phys)) {
        kprintf("[usb] port %u: out of DMA pages\n", port);
        return;
    }
    d->scratch = P2V(d->scratch_phys);
    d->inctx = P2V(d->inctx_phys);
    d->devctx = P2V(d->devctx_phys);
    if (!alloc_page(&d->ep0.ring.phys)) {
        kprintf("[usb] port %u: out of DMA pages\n", port);
        return;
    }
    d->ep0.ring.virt = P2V(d->ep0.ring.phys);
    ring_init(&d->ep0.ring, d->ep0.ring.virt, d->ep0.ring.phys);
    d->used = 1;

    int r = command(TRB_ENABLE_SLOT, 0, 0);
    if (r) {
        kprintf("[usb] port %u: enable slot failed\n", port);
        d->used = 0;
        return;
    }
    if (hc.cmd_slot < 1 || hc.cmd_slot > hc.max_slots || hc.cmd_slot > 63) {
        kprintf("[usb] port %u: controller returned an invalid slot %u\n", port, hc.cmd_slot);
        d->used = 0;
        return;
    }
    d->slot = (uint8_t)hc.cmd_slot;
    r = address_device(d);
    if (r) {
        d->used = 0;
        return;
    }
    r = read_device_descriptor(d);
    if (r) {
        kprintf("[usb] slot %u: device descriptor failed\n", d->slot);
        d->used = 0;
        return;
    }
    struct config_info ci;
    r = read_config_descriptor(d, &ci);
    if (r) {
        kprintf("[usb] slot %u: configuration descriptor failed\n", d->slot);
        d->used = 0;
        return;
    }
    unsigned ifaces = (unsigned)(!!ci.hid + !!ci.ecm + !!ci.ms);
    kprintf("[usb] slot %u: %04x:%04x %s speed, %u interface%s\n", d->slot, d->vendor, d->product,
            speed == USB_SPEED_SUPER ? "super" : speed == USB_SPEED_HIGH ? "high" : speed == USB_SPEED_LOW ? "low" : "full",
            ifaces, ifaces == 1 ? "" : "s");
    if (!ifaces) {
        kprintf("[usb] slot %u: no supported class\n", d->slot);
        return;
    }
    if (!ci.config_value) {
        kprintf("[usb] slot %u: configuration value 0\n", d->slot);
        return;
    }
    r = control(d, 0x00, 9, ci.config_value, 0, 0, 0); /* SET_CONFIGURATION */
    if (r < 0) {
        kprintf("[usb] slot %u: set configuration failed\n", d->slot);
        return;
    }
    d->config_value = ci.config_value;

    if (ci.hid) {
        int in = 1;
        if (endpoint_init(d, &d->eps[d->neps], ci.hid_ep, in, USB_EP_INTERRUPT, ci.hid_mps, ci.hid_interval)) {
            kprintf("[usb] slot %u: HID endpoint allocation failed\n", d->slot);
            return;
        }
        struct usb_endpoint *ep = &d->eps[d->neps];
        d->neps++;
        d->iface = ci.hid_iface;
        d->alt = ci.hid_alt;
        if (configure_endpoints(d, ep, 1)) return;
        usb_hid_probe(d, ci.hid_proto, ep);
        return;
    }

    if (ci.ecm) {
        d->iface = ci.ecm_data;
        d->alt = ci.ecm_alt;
        if (ci.ecm_alt) {
            control(d, 0x01, 11, ci.ecm_alt, ci.ecm_data, 0, 0); /* SET_INTERFACE */
        }
        struct usb_endpoint *in = &d->eps[d->neps];
        if (endpoint_init(d, in, ci.ecm_in, 1, USB_EP_BULK, ci.ecm_in_mps, 0)) return;
        d->neps++;
        struct usb_endpoint *out = &d->eps[d->neps];
        if (endpoint_init(d, out, ci.ecm_out, 0, USB_EP_BULK, ci.ecm_out_mps, 0)) return;
        d->neps++;
        if (configure_endpoints(d, d->eps, d->neps)) return;
        usb_net_probe(d, ci.ecm_ctrl, ci.ecm_data, ci.ecm_alt, in, out, ci.mac_index);
        return;
    }

    if (ci.ms) {
        d->iface = ci.ms_iface;
        d->alt = ci.ms_alt;
        if (ci.ms_alt) control(d, 0x01, 11, ci.ms_alt, ci.ms_iface, 0, 0); /* SET_INTERFACE */
        struct usb_endpoint *in = &d->eps[d->neps];
        if (endpoint_init(d, in, ci.ms_in, 1, USB_EP_BULK, ci.ms_in_mps, 0)) return;
        d->neps++;
        struct usb_endpoint *out = &d->eps[d->neps];
        if (endpoint_init(d, out, ci.ms_out, 0, USB_EP_BULK, ci.ms_out_mps, 0)) return;
        d->neps++;
        if (configure_endpoints(d, d->eps, d->neps)) return;
        usb_storage_probe(d, ci.ms_iface, in, out);
        return;
    }
}

/* ---- Public entry points ----------------------------------------------- */

int usb_init(void) {
    memset(&hc, 0, sizeof hc);
    memset(devices, 0, sizeof devices);
    struct pci_dev *pci = pci_find_class(0x0C, 0x03, 0x30, 0);
    if (!pci) {
        kprintf("[usb] no xHCI controller\n");
        return -E_NOTSUP;
    }
    int r = controller_init(pci);
    if (r) return r;
    for (unsigned port = 1; port <= hc.max_ports; port++) {
        uint32_t off = hc.op + PORTSC + (port - 1) * 0x10;
        uint32_t v = rd(off);
        if (!(v & PORTSC_PP)) {
            port_power_on(port);
            uint64_t until = deadline_ms(100);
            while (!(rd(off) & PORTSC_PP) && aether_pit_ms() < until) pause();
            v = rd(off);
        }
        clear_port_changes(port);
        if (!(v & PORTSC_CCS)) continue;
        uint8_t speed = 0;
        if (port_reset(port, &speed)) {
            kprintf("[usb] port %u: reset failed\n", port);
            continue;
        }
        kprintf("[usb] port %u: device connected (speed %u)\n", port, speed);
        enumerate_port(port, speed);
    }
    process_events();
    return 0;
}

void usb_poll(void) {
    if (!hc.mmio) return;
    process_events();
    usb_hid_poll();
}
