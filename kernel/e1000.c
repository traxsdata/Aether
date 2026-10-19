/*
 * e1000.c - Intel PRO/1000 network cards: the 8254x family that QEMU and
 * VMware emulate (82540EM, 82545EM) and the 82574L ("e1000e" in VMware).
 *
 * The driver is polled, like the disk driver: INTx stays masked and
 * net_poll() collects received frames from the window manager loop. Both
 * rings use legacy descriptors, which all of these chips understand.
 *
 * What the card may touch is limited to the rings and their buffers: every
 * DMA address it's given is a page we allocated for it, and a received
 * length is checked against the buffer before anyone reads it.
 */
#include "cpu.h"
#include "kernel.h"
#include "mm.h"
#include "net.h"
#include "pci.h"

#define NIC_WINDOW 0xFFFFFE0001000000ull /* 128 KiB of uncached MMIO for the registers */
#define NIC_MMIO_SIZE 0x20000

/* Registers */
#define CTRL 0x0000
#define STATUS 0x0008
#define EERD 0x0014
#define ICR 0x00C0
#define IMC 0x00D8
#define RCTL 0x0100
#define TCTL 0x0400
#define TIPG 0x0410
#define RDBAL 0x2800
#define RDBAH 0x2804
#define RDLEN 0x2808
#define RDH 0x2810
#define RDT 0x2818
#define TDBAL 0x3800
#define TDBAH 0x3804
#define TDLEN 0x3808
#define TDH 0x3810
#define TDT 0x3818
#define MTA 0x5200
#define RAL0 0x5400
#define RAH0 0x5404

#define CTRL_ASDE (1u << 5)
#define CTRL_SLU (1u << 6)
#define CTRL_RST (1u << 26)
#define STATUS_LU (1u << 1)
#define RCTL_EN (1u << 1)
#define RCTL_BAM (1u << 15)  /* accept broadcast (ARP, DHCP) */
#define RCTL_SECRC (1u << 26) /* strip the CRC */
#define TCTL_EN (1u << 1)
#define TCTL_PSP (1u << 3)
#define TX_EOP 0x01
#define TX_IFCS 0x02
#define TX_RS 0x08
#define DESC_DD 0x01
#define RX_EOP 0x02

#define NRX 64
#define NTX 32
#define BUF_SIZE 2048

struct rx_desc {
    uint64_t addr;
    uint16_t len, csum;
    uint8_t status, errors;
    uint16_t special;
} __attribute__((packed));

struct tx_desc {
    uint64_t addr;
    uint16_t len;
    uint8_t cso, cmd, status, css;
    uint16_t special;
} __attribute__((packed));

static struct {
    volatile uint8_t *mmio;
    struct rx_desc *rx;
    struct tx_desc *tx;
    uint8_t *rxbuf[NRX], *txbuf[NTX];
    unsigned rx_next, tx_next;
    uint8_t mac[6];
    int e1000e;
} nic;

static const struct {
    uint16_t id;
    const char *name;
    int e1000e;
} models[] = {
    {0x100E, "82540EM", 0}, {0x100F, "82545EM", 0}, {0x1004, "82543GC", 0}, {0x100C, "82544GC", 0},
    {0x1011, "82545EM", 0}, {0x1026, "82545GM", 0}, {0x10D3, "82574L", 1},
};

static inline uint32_t rd(uint32_t off) { return *(volatile uint32_t *)(nic.mmio + off); }
static inline void wr(uint32_t off, uint32_t v) { *(volatile uint32_t *)(nic.mmio + off) = v; }
static inline void mb(void) { __asm__ volatile("mfence" ::: "memory"); }

static void delay_ms(uint64_t ms) {
    uint64_t until = ticks + ms + 1;
    while (ticks < until) {
        __asm__ volatile("sti; hlt; cli");
    }
}

/* EEPROM word, for cards that don't load the address registers themselves. */
static int eeprom_read(int addr, uint16_t *out) {
    uint32_t start = nic.e1000e ? ((uint32_t)addr << 2) | 1 : ((uint32_t)addr << 8) | 1;
    uint32_t done = nic.e1000e ? 1u << 1 : 1u << 4;
    wr(EERD, start);
    for (int i = 0; i < 100000; i++) {
        uint32_t v = rd(EERD);
        if (v & done) {
            *out = (uint16_t)(v >> 16);
            return 0;
        }
    }
    return -1;
}

static int map_regs(uint64_t bar) {
    for (uint64_t o = 0; o < NIC_MMIO_SIZE; o += PAGE_SIZE) {
        if (vmm_map(vmm_kernel_pml4(), NIC_WINDOW + o, bar + o, PTE_RW | PTE_NX | PTE_G | PTE_PCD | PTE_PWT)) return -1;
        invlpg(NIC_WINDOW + o);
    }
    nic.mmio = (volatile uint8_t *)NIC_WINDOW;
    return 0;
}

/* Two 2 KiB buffers per page, so no buffer crosses a page boundary. */
static int alloc_buffers(uint8_t **bufs, int n, uint64_t *phys) {
    for (int i = 0; i < n; i += 2) {
        uint64_t pa = pmm_alloc();
        if (!pa) return -1;
        phys[i] = pa;
        phys[i + 1] = pa + BUF_SIZE;
        bufs[i] = P2V(pa);
        bufs[i + 1] = (uint8_t *)P2V(pa) + BUF_SIZE;
    }
    return 0;
}

static int tx(const void *frame, size_t len) {
    if (len > BUF_SIZE || len < 14) return -E_INVAL;
    struct tx_desc *d = &nic.tx[nic.tx_next];
    if (d->cmd && !(d->status & DESC_DD)) return -E_LIMIT; /* ring full */
    if (len < 60) { /* pad short frames; the card would too, but with stale bytes */
        memset(nic.txbuf[nic.tx_next] + len, 0, 60 - len);
        memcpy(nic.txbuf[nic.tx_next], frame, len);
        len = 60;
    } else {
        memcpy(nic.txbuf[nic.tx_next], frame, len);
    }
    d->len = (uint16_t)len;
    d->status = 0;
    d->cmd = TX_EOP | TX_IFCS | TX_RS;
    mb();
    nic.tx_next = (nic.tx_next + 1) % NTX;
    wr(TDT, nic.tx_next);
    return 0;
}

static int rx(void *buf, size_t max) {
    struct rx_desc *d = &nic.rx[nic.rx_next];
    if (!(d->status & DESC_DD)) return 0;
    mb();
    int len = d->len;
    /* A frame bigger than one buffer, or with errors, is dropped. */
    int ok = (d->status & RX_EOP) && !d->errors && len >= 14 && len <= BUF_SIZE && (size_t)len <= max;
    if (ok) memcpy(buf, nic.rxbuf[nic.rx_next], (size_t)len);
    d->status = 0;
    unsigned done = nic.rx_next;
    nic.rx_next = (nic.rx_next + 1) % NRX;
    mb();
    wr(RDT, done); /* hand the descriptor back */
    return ok ? len : -1;
}

static int link_up(void) { return (rd(STATUS) & STATUS_LU) != 0; }

static struct nic_ops ops = {.tx = tx, .rx = rx, .link_up = link_up};

static int init_card(struct pci_dev *d, const char *model) {
    int is_io;
    uint64_t bar = pci_bar_ex(d, 0, &is_io);
    if (!bar || is_io) {
        kprintf("[net] %02x:%02x.%u: BAR0 is not memory\n", d->bus, d->dev, d->fn);
        return -1;
    }
    pci_enable_bus_master_mem(d);
    if (map_regs(bar)) return -1;

    wr(IMC, 0xFFFFFFFFu);
    wr(CTRL, rd(CTRL) | CTRL_RST);
    delay_ms(2);
    for (int i = 0; i < 1000 && (rd(CTRL) & CTRL_RST); i++) delay_ms(1);
    wr(IMC, 0xFFFFFFFFu);
    (void)rd(ICR);
    wr(CTRL, (rd(CTRL) | CTRL_SLU | CTRL_ASDE) & ~(1u << 3) /* LRST */ & ~(1u << 31) /* PHY_RST */);

    uint32_t lo = rd(RAL0), hi = rd(RAH0);
    if ((hi & (1u << 31)) && (lo || (hi & 0xFFFF))) {
        for (int i = 0; i < 4; i++) nic.mac[i] = (uint8_t)(lo >> (8 * i));
        nic.mac[4] = (uint8_t)hi;
        nic.mac[5] = (uint8_t)(hi >> 8);
    } else {
        for (int i = 0; i < 3; i++) {
            uint16_t w;
            if (eeprom_read(i, &w)) {
                kprintf("[net] can't read the card's address\n");
                return -1;
            }
            nic.mac[2 * i] = (uint8_t)w;
            nic.mac[2 * i + 1] = (uint8_t)(w >> 8);
        }
        wr(RAL0, (uint32_t)nic.mac[0] | (uint32_t)nic.mac[1] << 8 | (uint32_t)nic.mac[2] << 16 | (uint32_t)nic.mac[3] << 24);
        wr(RAH0, (uint32_t)nic.mac[4] | (uint32_t)nic.mac[5] << 8 | 1u << 31);
    }
    for (int i = 0; i < 128; i++) wr(MTA + 4 * i, 0); /* no multicast */

    uint64_t ring = pmm_alloc(); /* 64 rx + 32 tx descriptors = 1.5 KiB */
    if (!ring) return -1;
    nic.rx = P2V(ring);
    nic.tx = (struct tx_desc *)((uint8_t *)P2V(ring) + NRX * sizeof(struct rx_desc));
    uint64_t rxphys[NRX], txphys[NTX];
    if (alloc_buffers(nic.rxbuf, NRX, rxphys) || alloc_buffers(nic.txbuf, NTX, txphys)) return -1;
    for (int i = 0; i < NRX; i++) nic.rx[i].addr = rxphys[i];
    for (int i = 0; i < NTX; i++) nic.tx[i].addr = txphys[i];

    wr(RDBAL, (uint32_t)ring);
    wr(RDBAH, (uint32_t)(ring >> 32));
    wr(RDLEN, NRX * sizeof(struct rx_desc));
    wr(RDH, 0);
    wr(RDT, NRX - 1);
    wr(RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC); /* 2 KiB buffers, no promiscuous or multicast modes */

    uint64_t txring = ring + NRX * sizeof(struct rx_desc);
    wr(TDBAL, (uint32_t)txring);
    wr(TDBAH, (uint32_t)(txring >> 32));
    wr(TDLEN, NTX * sizeof(struct tx_desc));
    wr(TDH, 0);
    wr(TDT, 0);
    wr(TIPG, 0x0060200A);
    wr(TCTL, TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x40 << 12));

    memcpy(ops.mac, nic.mac, 6);
    kprintf("[net] %02x:%02x.%u Intel %s, address %02x:%02x:%02x:%02x:%02x:%02x, link %s\n", d->bus, d->dev, d->fn,
            model, nic.mac[0], nic.mac[1], nic.mac[2], nic.mac[3], nic.mac[4], nic.mac[5], link_up() ? "up" : "down");
    return 0;
}

struct nic_ops *e1000_init(void) {
    struct pci_dev *d;
    for (int i = 0; (d = pci_find_class(0x02, 0x00, PCI_ANY, i)); i++) {
        if (d->vendor != 0x8086) {
            kprintf("[net] %02x:%02x.%u %04x:%04x: network card with no driver\n", d->bus, d->dev, d->fn, d->vendor,
                    d->device);
            continue;
        }
        for (unsigned m = 0; m < ARRAY_LEN(models); m++) {
            if (models[m].id != d->device) continue;
            nic.e1000e = models[m].e1000e;
            if (!init_card(d, models[m].name)) return &ops;
        }
        kprintf("[net] %02x:%02x.%u Intel %04x: not a supported model\n", d->bus, d->dev, d->fn, d->device);
    }
    return 0;
}
