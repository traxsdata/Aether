/*
 * usb_net.c - CDC Ethernet Control Model (ECM) network adapters.
 *
 * The class descriptor gives us one bulk-IN and one bulk-OUT endpoint. RX is
 * one persistent transfer that the network poll drains; TX is a bounded
 * synchronous transfer. No packet parsing happens here: the frame goes to
 * net.c unchanged, including its length check.
 *
 * A USB Ethernet dongle is the wired network path for laptops (including the
 * Yoga 9) that have no built-in Ethernet port.
 */
#include "kernel.h"
#include "net.h"
#include "usb.h"

#define USB_NET_MAX_FRAME 2048

static struct {
    int used;
    int ready;
    struct usb_device *dev;
    struct usb_endpoint *in, *out;
    uint8_t ctrl_iface;
    uint8_t mac[6];
    uint8_t rxbuf[USB_NET_MAX_FRAME];
    int errors;
} ecm;

static struct nic_ops ops;

static int hexval(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static int parse_mac(const char *s, uint8_t mac[6]) {
    uint8_t out[6] = {0};
    int n = 0;
    for (int i = 0; s[i] && n < 12; i++) {
        int v = hexval(s[i]);
        if (v < 0) continue;
        out[n / 2] = (uint8_t)((out[n / 2] << 4) | (uint8_t)v);
        n++;
    }
    if (n != 12) return -1;
    memcpy(mac, out, sizeof out);
    return 0;
}

static int valid_unicast_mac(const uint8_t mac[6]) {
    if (mac[0] & 1) return 0; /* multicast/broadcast */
    int zero = 1, ff = 1;
    for (int i = 0; i < 6; i++) {
        zero &= mac[i] == 0;
        ff &= mac[i] == 0xFF;
    }
    return !zero && !ff;
}

static int set_packet_filter(void) {
    /* Directed + broadcast; nothing promiscuous and no multicast groups. */
    return usb_control(ecm.dev, 0x21, 0x43, 0x000C, ecm.ctrl_iface, 0, 0);
}

static int tx(const void *frame, size_t len) {
    if (!ecm.ready || !frame || len < 14 || len > 1514) return -E_INVAL;
    int r = usb_ep_sync(ecm.dev, ecm.out, frame, 0, len, 100);
    if (r && ecm.errors++ < 4) kprintf("[usb] Ethernet transmit failed (%d)\n", r);
    return r;
}

static int rx(void *buf, size_t max) {
    if (!ecm.ready || !buf) return 0;
    if (!ecm.in->pending && ecm.in->status == 0) {
        if (usb_ep_submit(ecm.dev, ecm.in, 0, USB_NET_MAX_FRAME)) return 0;
        return 0;
    }
    if (ecm.in->pending) return 0;
    size_t actual = 0;
    int r = usb_ep_complete(ecm.dev, ecm.in, ecm.rxbuf, sizeof ecm.rxbuf, &actual);
    if (r < 0) {
        ecm.errors++;
        usb_ep_abort(ecm.dev, ecm.in);
        usb_ep_submit(ecm.dev, ecm.in, 0, USB_NET_MAX_FRAME);
        return -1;
    }
    if (r == 0) return 0;
    usb_ep_submit(ecm.dev, ecm.in, 0, USB_NET_MAX_FRAME);
    if (actual < 14 || actual > max) return -1;
    memcpy(buf, ecm.rxbuf, actual);
    return (int)actual;
}

static int link_up(void) { return ecm.ready; }

void usb_net_probe(struct usb_device *d, uint8_t ctrl_iface, uint8_t data_iface, uint8_t data_alt,
                   struct usb_endpoint *in, struct usb_endpoint *out, uint8_t mac_index) {
    (void)data_iface;
    (void)data_alt;
    if (ecm.used) {
        kprintf("[usb] slot %u: a second USB Ethernet adapter is ignored\n", d->slot);
        return;
    }
    memset(&ecm, 0, sizeof ecm);
    ecm.used = 1;
    ecm.dev = d;
    ecm.ctrl_iface = ctrl_iface;
    ecm.in = in;
    ecm.out = out;

    char text[64];
    int have_mac = 0;
    if (mac_index && usb_string(d, mac_index, text, sizeof text) == 0) {
        memset(ecm.mac, 0, sizeof ecm.mac);
        have_mac = parse_mac(text, ecm.mac) == 0 && valid_unicast_mac(ecm.mac);
    }
    if (!have_mac) {
        /* Locally administered, stable for this port/device. */
        memset(ecm.mac, 0, sizeof ecm.mac);
        ecm.mac[0] = 0x02;
        ecm.mac[1] = 0xAE;
        ecm.mac[2] = (uint8_t)(d->vendor >> 8);
        ecm.mac[3] = (uint8_t)d->vendor;
        ecm.mac[4] = (uint8_t)(d->product >> 8);
        ecm.mac[5] = (uint8_t)(d->product ^ d->port);
    }
    memcpy(ops.mac, ecm.mac, 6);
    kprintf("[usb] slot %u: USB Ethernet %02x:%02x:%02x:%02x:%02x:%02x\n", d->slot, ecm.mac[0], ecm.mac[1],
            ecm.mac[2], ecm.mac[3], ecm.mac[4], ecm.mac[5]);
}

struct nic_ops *usb_net_init(void) {
    if (!ecm.used) return 0;
    if (!ecm.ready) {
        if (set_packet_filter())
            kprintf("[usb] Ethernet packet filter not accepted; continuing\n");
        ecm.ready = 1;
    }
    ops.tx = tx;
    ops.rx = rx;
    ops.link_up = link_up;
    memcpy(ops.mac, ecm.mac, 6);
    kprintf("[net] USB Ethernet ready\n");
    return &ops;
}
