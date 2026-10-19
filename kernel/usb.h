/*
 * usb.h - USB xHCI transport and the shared device/endpoint model.
 *
 * The controller is polled: there is no interrupt handler and no allocation
 * on the data path. All rings, device contexts and endpoint buffers are
 * owned physical pages, mapped through the direct map. Class drivers (HID and
 * CDC-ECM) sit above this file; they never touch MMIO or user pointers.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

/* One hub-less laptop path: root ports, up to a few directly attached
 * devices. External hubs are deliberately not parsed yet. */
#define USB_MAX_DEVICES 8
#define USB_MAX_EPS 4

enum { USB_EP_CONTROL = 0, USB_EP_BULK = 2, USB_EP_INTERRUPT = 3 };

enum { USB_SPEED_LOW = 2, USB_SPEED_FULL = 1, USB_SPEED_HIGH = 3, USB_SPEED_SUPER = 4 };

struct usb_ring {
    void *virt;
    uint64_t phys;
    unsigned enq;
    uint8_t cycle;
};

struct usb_endpoint {
    uint8_t dci;      /* device context index: 2*number + IN */
    uint8_t number;   /* endpoint number from the descriptor */
    uint8_t in;
    uint8_t type;     /* USB_EP_* */
    uint8_t interval;
    uint16_t mps;
    uint8_t *buf;
    uint64_t buf_phys;
    size_t buf_len;
    struct usb_ring ring;
    volatile int pending;
    volatile int status;  /* 0 idle, 1 complete, negative error */
    volatile int code;    /* last xHCI completion code, for diagnostics */
    volatile size_t submitted;
    volatile size_t actual;
    uint64_t last_trb_phys;
    uint64_t data_trb_phys;
    volatile size_t data_actual;
    volatile int data_seen;
};

struct usb_device {
    int used;
    uint8_t slot, port, speed;
    uint16_t vendor, product;
    uint16_t ep0_mps;
    uint8_t configs;
    uint8_t config_value;
    uint8_t iface;
    uint8_t alt;
    uint8_t *scratch;
    uint64_t scratch_phys;
    uint8_t *inctx;
    uint64_t inctx_phys;
    uint8_t *devctx;
    uint64_t devctx_phys;
    struct usb_endpoint ep0;
    struct usb_endpoint eps[USB_MAX_EPS];
    int neps;
};

int usb_init(void);
void usb_poll(void);

#define USB_TIMEOUT_MS 1000
int usb_control(struct usb_device *d, uint8_t type, uint8_t req, uint16_t value, uint16_t index, void *buf, uint16_t len);
int usb_string(struct usb_device *d, uint8_t index, char *out, size_t max);

int usb_ep_submit(struct usb_device *d, struct usb_endpoint *ep, const void *data, size_t len);
int usb_ep_complete(struct usb_device *d, struct usb_endpoint *ep, void *data, size_t max, size_t *actual);
int usb_ep_sync(struct usb_device *d, struct usb_endpoint *ep, const void *tx, void *rx, size_t len, int timeout_ms);
int usb_ep_abort(struct usb_device *d, struct usb_endpoint *ep);

/* Class drivers. usb.c calls probe() once the configuration is selected and
 * the endpoints are configured; poll() runs from the window manager loop. */
struct usb_endpoint;
void usb_hid_probe(struct usb_device *d, uint8_t proto, struct usb_endpoint *ep);
void usb_hid_poll(void);
void usb_net_probe(struct usb_device *d, uint8_t ctrl_iface, uint8_t data_iface, uint8_t data_alt,
                   struct usb_endpoint *in, struct usb_endpoint *out, uint8_t mac_index);
struct nic_ops *usb_net_init(void);
