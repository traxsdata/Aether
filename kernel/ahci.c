/*
 * ahci.c - polling AHCI driver, written from the AHCI 1.3.1 and ATA8-ACS
 * specifications.
 *
 * Shape of the hardware: the HBA's registers live in the memory BAR "ABAR"
 * (BAR5): generic host control at 0x00, then one 0x80-byte register block
 * per port from 0x100. Each port DMAs from a command list (32 headers of
 * 32 bytes), each header points at a command table holding the command FIS
 * and a scatter/gather list (PRDT); the port writes received FISes (the
 * device's status) into a 256-byte area.
 *
 * This driver keeps it simple: one command slot per port, issued and
 * polled to completion, no interrupts, no NCQ. Data moves through a
 * per-port bounce buffer of pmm pages, so callers can hand in any kernel
 * virtual buffer (heap, stack, unaligned) and the device only ever sees
 * physical pages we own. Each command moves at most BOUNCE_PAGES pages;
 * longer requests are split.
 */
#include "ahci.h"
#include "blk.h"
#include "cpu.h"
#include "kernel.h"
#include "mm.h"
#include "pci.h"

/* ---- Register layout ---------------------------------------------------- */

/* Generic host control */
#define HBA_CAP 0x00
#define HBA_GHC 0x04
#define HBA_IS 0x08
#define HBA_PI 0x0C
#define HBA_VS 0x10
#define HBA_CAP2 0x24
#define HBA_BOHC 0x28

#define CAP_S64A (1u << 31) /* 64-bit DMA addresses */
#define CAP_SSS (1u << 27)  /* staggered spin-up */
#define GHC_AE (1u << 31)   /* AHCI enable */
#define GHC_IE (1u << 1)
#define CAP2_BOH (1u << 0)  /* BIOS/OS handoff supported */
#define BOHC_BOS (1u << 0)  /* BIOS owns */
#define BOHC_OOS (1u << 1)  /* OS owns */
#define BOHC_BB (1u << 4)   /* BIOS busy */

/* Per-port registers, offsets from the port's block */
#define PX_CLB 0x00
#define PX_CLBU 0x04
#define PX_FB 0x08
#define PX_FBU 0x0C
#define PX_IS 0x10
#define PX_IE 0x14
#define PX_CMD 0x18
#define PX_TFD 0x20
#define PX_SIG 0x24
#define PX_SSTS 0x28
#define PX_SCTL 0x2C
#define PX_SERR 0x30
#define PX_SACT 0x34
#define PX_CI 0x38

#define CMD_ST (1u << 0)
#define CMD_SUD (1u << 1)
#define CMD_POD (1u << 2)
#define CMD_FRE (1u << 4)
#define CMD_FR (1u << 14)
#define CMD_CR (1u << 15)

#define IS_TFES (1u << 30) /* task file error */
#define IS_HBFS (1u << 29) /* host bus fatal */
#define IS_HBDS (1u << 28) /* host bus data error */
#define IS_IFS (1u << 27)  /* interface fatal */
#define IS_FATAL (IS_TFES | IS_HBFS | IS_HBDS | IS_IFS)

#define ATA_ERR 0x01
#define ATA_DRQ 0x08
#define ATA_DF 0x20
#define ATA_BSY 0x80

#define SIG_ATA 0x00000101u
#define SIG_ATAPI 0xEB140101u

#define ATA_IDENTIFY 0xEC
#define ATA_READ_DMA_EXT 0x25
#define ATA_WRITE_DMA_EXT 0x35
#define ATA_FLUSH_CACHE 0xE7
#define ATA_FLUSH_CACHE_EXT 0xEA

#define FIS_REG_H2D 0x27

/* Command header (one per slot in the command list) */
struct cmd_header {
    uint16_t flags; /* CFL[4:0] dwords, A bit 5, W bit 6, P bit 7, R 8, B 9, C 10, PMP[15:12] */
    uint16_t prdtl; /* PRDT entries */
    volatile uint32_t prdbc; /* bytes transferred, written by the HBA */
    uint32_t ctba, ctbau;
    uint32_t rsvd[4];
};
#define HDR_WRITE (1u << 6)

struct prd {
    uint32_t dba, dbau, rsvd;
    uint32_t dbc; /* byte count - 1 in [21:0], bit 31 interrupt on completion */
};

#define CT_CFIS 0x00
#define CT_PRDT 0x80
#define MAX_PRD ((PAGE_SIZE - CT_PRDT) / sizeof(struct prd)) /* 248 in a one-page table */
#define PRD_MAX_BYTES (4u << 20)                             /* 4 MiB per entry */

/* ---- Driver state ------------------------------------------------------- */

#define ABAR_WINDOW 0xFFFFFE0000000000ull /* each controller gets 64 KiB of uncached MMIO here */
#define ABAR_WINDOW_STRIDE 0x10000ull
#define MAX_CTRL 4
#define MAX_PORTS 32
#define BOUNCE_PAGES 64 /* 256 KiB = 512 sectors per command */
#define BOUNCE_SECTORS (BOUNCE_PAGES * (PAGE_SIZE / BLK_SECTOR))

#define TMO_SPINUP_MS 1000
#define TMO_STOP_MS 500
#define TMO_IDENTIFY_MS 5000
#define TMO_IO_MS 10000
#define TMO_FLUSH_MS 30000

struct ahci_ctrl {
    volatile uint8_t *mmio;
    uint32_t cap;
    struct pci_dev *pci;
};

struct ahci_port {
    struct ahci_ctrl *ctrl;
    volatile uint8_t *regs;
    int num;
    uint64_t clb, fb, ct; /* physical: command list, received FIS, command table */
    uint64_t bounce[BOUNCE_PAGES];
    uint8_t flush_cmd;
    char model[41];
    struct blkdev blk;
};

static struct ahci_ctrl ctrls[MAX_CTRL];
static int nctrl, nsata;
static struct ahci_port *sata[MAX_PORTS];

static inline uint32_t hba_rd(struct ahci_ctrl *c, uint32_t off) { return *(volatile uint32_t *)(c->mmio + off); }
static inline void hba_wr(struct ahci_ctrl *c, uint32_t off, uint32_t v) { *(volatile uint32_t *)(c->mmio + off) = v; }
static inline uint32_t px_rd(struct ahci_port *p, uint32_t off) { return *(volatile uint32_t *)(p->regs + off); }
static inline void px_wr(struct ahci_port *p, uint32_t off, uint32_t v) { *(volatile uint32_t *)(p->regs + off) = v; }
static inline void mb(void) { __asm__ volatile("mfence" ::: "memory"); }

/* ---- Timeouts ------------------------------------------------------------
 * With interrupts on, `ticks` counts milliseconds. The driver also runs at
 * boot (before sti) and inside syscalls (IF cleared on entry), where ticks
 * stand still, so then we count the PIT's own 1 ms periods directly: it
 * counts down and reloads, so a reading larger than the last one means a
 * period elapsed. Polling loops read it far more often than once per ms;
 * if one were ever slower, the timeout would only get longer, never shorter. */
struct tmo {
    int irq;
    uint64_t left, last_tick;
    uint16_t last_pit;
};

static uint16_t pit_count(void) {
    outb(0x43, 0x00); /* latch channel 0 */
    uint8_t lo = inb(0x40), hi = inb(0x40);
    return (uint16_t)(lo | (hi << 8));
}

static void tmo_start(struct tmo *t, uint64_t ms) {
    t->irq = (read_rflags() & 0x200) != 0;
    t->left = ms;
    t->last_tick = ticks;
    t->last_pit = pit_count();
}

static int tmo_expired(struct tmo *t) {
    uint64_t elapsed;
    if (t->irq) {
        uint64_t now = ticks;
        elapsed = now - t->last_tick;
        t->last_tick = now;
    } else {
        uint16_t now = pit_count();
        elapsed = now > t->last_pit;
        t->last_pit = now;
    }
    t->left -= MIN(elapsed, t->left);
    return t->left == 0;
}

/* Wait until (reg & mask) == want; 0 on success, -1 on timeout. */
static int px_wait(struct ahci_port *p, uint32_t off, uint32_t mask, uint32_t want, uint64_t ms) {
    struct tmo t;
    tmo_start(&t, ms);
    for (;;) {
        if ((px_rd(p, off) & mask) == want) return 0;
        if (tmo_expired(&t)) return -1;
        __asm__ volatile("pause");
    }
}

static void delay_ms(uint64_t ms) {
    struct tmo t;
    tmo_start(&t, ms);
    while (!tmo_expired(&t)) __asm__ volatile("pause");
}

/* ---- Port control -------------------------------------------------------- */

static int port_stop(struct ahci_port *p) {
    uint32_t cmd = px_rd(p, PX_CMD);
    if (cmd & CMD_ST) px_wr(p, PX_CMD, cmd & ~CMD_ST);
    if (px_wait(p, PX_CMD, CMD_CR, 0, TMO_STOP_MS)) return -1;
    cmd = px_rd(p, PX_CMD);
    if (cmd & CMD_FRE) px_wr(p, PX_CMD, cmd & ~CMD_FRE);
    return px_wait(p, PX_CMD, CMD_FR, 0, TMO_STOP_MS);
}

/* COMRESET: re-initialise the link, then wait for the device to come back. */
static int port_comreset(struct ahci_port *p) {
    uint32_t sctl = px_rd(p, PX_SCTL);
    px_wr(p, PX_SCTL, (sctl & ~0xFu) | 1);
    delay_ms(2); /* spec: hold DET=1 for at least 1 ms */
    px_wr(p, PX_SCTL, sctl & ~0xFu);
    int r = px_wait(p, PX_SSTS, 0xF, 3, TMO_SPINUP_MS);
    px_wr(p, PX_SERR, 0xFFFFFFFFu);
    return r;
}

/* FRE on, wait for the drive to be idle, then ST on. */
static int port_start(struct ahci_port *p) {
    px_wr(p, PX_SERR, 0xFFFFFFFFu);
    px_wr(p, PX_IS, 0xFFFFFFFFu);
    px_wr(p, PX_CMD, px_rd(p, PX_CMD) | CMD_FRE);
    if (px_wait(p, PX_TFD, ATA_BSY | ATA_DRQ, 0, TMO_SPINUP_MS)) {
        if (port_comreset(p) || px_wait(p, PX_TFD, ATA_BSY | ATA_DRQ, 0, TMO_SPINUP_MS)) return -1;
    }
    px_wr(p, PX_CMD, px_rd(p, PX_CMD) | CMD_ST);
    return 0;
}

/* After an error or timeout: stop (which clears CI), clear errors, restart. */
static void port_recover(struct ahci_port *p) {
    if (port_stop(p)) port_comreset(p);
    if (port_start(p)) kprintf("[ahci] port %d: recovery failed\n", p->num);
}

static void set_addr(uint32_t *lo, uint32_t *hi, uint64_t phys) {
    *lo = (uint32_t)phys;
    *hi = (uint32_t)(phys >> 32);
}

/* Issue one command in slot 0 and poll it to completion. `bytes` of data
 * move through the bounce pages (0 for non-data commands). */
static int port_exec(struct ahci_port *p, uint8_t command, uint64_t lba, uint16_t count, uint32_t bytes, int write,
                     uint64_t timeout_ms) {
    if (px_wait(p, PX_TFD, ATA_BSY | ATA_DRQ, 0, TMO_IO_MS)) {
        port_recover(p);
        if (px_rd(p, PX_TFD) & (ATA_BSY | ATA_DRQ)) return BLK_ETIMEDOUT;
    }

    uint8_t *ct = P2V(p->ct);
    memset(ct, 0, CT_PRDT);
    uint8_t *fis = ct + CT_CFIS;
    fis[0] = FIS_REG_H2D;
    fis[1] = 0x80; /* C: this is a command */
    fis[2] = command;
    fis[4] = (uint8_t)lba, fis[5] = (uint8_t)(lba >> 8), fis[6] = (uint8_t)(lba >> 16);
    fis[7] = command == ATA_IDENTIFY ? 0 : 0x40; /* device: LBA mode */
    fis[8] = (uint8_t)(lba >> 24), fis[9] = (uint8_t)(lba >> 32), fis[10] = (uint8_t)(lba >> 40);
    fis[12] = (uint8_t)count, fis[13] = (uint8_t)(count >> 8);

    /* Scatter/gather: merge physically adjacent bounce pages, ≤4 MiB per entry. */
    struct prd *prd = (struct prd *)(ct + CT_PRDT);
    uint32_t nprd = 0, left = bytes;
    for (int i = 0; left; i++) {
        uint32_t n = MIN(left, (uint32_t)PAGE_SIZE);
        uint64_t pa = p->bounce[i];
        struct prd *last = nprd ? &prd[nprd - 1] : 0;
        uint64_t last_end = last ? (((uint64_t)last->dbau << 32) | last->dba) + (last->dbc & 0x3FFFFF) + 1 : 0;
        if (last && last_end == pa && (last->dbc & 0x3FFFFF) + 1 + n <= PRD_MAX_BYTES) {
            last->dbc += n;
        } else {
            if (nprd == MAX_PRD) return BLK_EINVAL; /* can't happen with BOUNCE_PAGES < MAX_PRD */
            set_addr(&prd[nprd].dba, &prd[nprd].dbau, pa);
            prd[nprd].rsvd = 0;
            prd[nprd].dbc = n - 1; /* n is even, so bit 0 of dbc is 1 as the spec requires */
            nprd++;
        }
        left -= n;
    }

    struct cmd_header *h = P2V(p->clb);
    memset(h, 0, sizeof *h);
    h->flags = (uint16_t)(5 | (write ? HDR_WRITE : 0)); /* 20-byte FIS = 5 dwords */
    h->prdtl = (uint16_t)nprd;
    set_addr(&h->ctba, &h->ctbau, p->ct);

    px_wr(p, PX_IS, 0xFFFFFFFFu);
    mb();
    px_wr(p, PX_CI, 1);

    struct tmo t;
    tmo_start(&t, timeout_ms);
    for (;;) {
        uint32_t is = px_rd(p, PX_IS);
        if (is & IS_FATAL) break;
        if (!(px_rd(p, PX_CI) & 1)) break;
        if (tmo_expired(&t)) {
            kprintf("[ahci] port %d: command %02x timed out (tfd %x is %x)\n", p->num, command, px_rd(p, PX_TFD),
                    px_rd(p, PX_IS));
            port_recover(p);
            return BLK_ETIMEDOUT;
        }
        __asm__ volatile("pause");
    }
    mb();
    uint32_t is = px_rd(p, PX_IS), tfd = px_rd(p, PX_TFD);
    if ((is & IS_FATAL) || (tfd & (ATA_ERR | ATA_DF))) {
        kprintf("[ahci] port %d: command %02x lba %lu failed: status %02x error %02x is %x serr %x\n", p->num, command,
                lba, tfd & 0xFF, (tfd >> 8) & 0xFF, is, px_rd(p, PX_SERR));
        port_recover(p);
        return BLK_EIO;
    }
    if (bytes && h->prdbc != bytes) {
        kprintf("[ahci] port %d: short transfer %u of %u bytes\n", p->num, h->prdbc, bytes);
        return BLK_EIO;
    }
    return 0;
}

/* Copy between a caller's buffer and the (non-contiguous) bounce pages. */
static void bounce_copy(struct ahci_port *p, uint8_t *buf, uint32_t bytes, int to_bounce) {
    for (int i = 0; bytes; i++) {
        uint32_t n = MIN(bytes, (uint32_t)PAGE_SIZE);
        if (to_bounce) memcpy(P2V(p->bounce[i]), buf, n);
        else memcpy(buf, P2V(p->bounce[i]), n);
        buf += n, bytes -= n;
    }
}

static int ahci_rw(struct blkdev *d, uint64_t lba, uint32_t count, uint8_t *buf, int write) {
    struct ahci_port *p = d->priv;
    if (lba >= d->sectors || count > d->sectors - lba) return BLK_ERANGE;
    while (count) {
        uint32_t n = MIN(count, (uint32_t)BOUNCE_SECTORS), bytes = n * BLK_SECTOR;
        if (write) bounce_copy(p, buf, bytes, 1);
        int r = port_exec(p, write ? ATA_WRITE_DMA_EXT : ATA_READ_DMA_EXT, lba, (uint16_t)n, bytes, write, TMO_IO_MS);
        if (r) return r;
        if (!write) bounce_copy(p, buf, bytes, 0);
        lba += n, count -= n, buf += bytes;
    }
    return 0;
}

static int ahci_read(struct blkdev *d, uint64_t lba, uint32_t count, void *buf) { return ahci_rw(d, lba, count, buf, 0); }
static int ahci_write(struct blkdev *d, uint64_t lba, uint32_t count, const void *buf) {
    return ahci_rw(d, lba, count, (uint8_t *)buf, 1);
}
static int ahci_flush(struct blkdev *d) {
    struct ahci_port *p = d->priv;
    return port_exec(p, p->flush_cmd, 0, 0, 0, 0, TMO_FLUSH_MS);
}

/* ---- Bring-up ------------------------------------------------------------ */

static int phys_ok(struct ahci_ctrl *c, uint64_t pa) { return pa && ((c->cap & CAP_S64A) || pa + PAGE_SIZE <= (1ull << 32)); }

static void port_free(struct ahci_port *p) {
    if (p->clb) pmm_free(p->clb);
    if (p->fb) pmm_free(p->fb);
    if (p->ct) pmm_free(p->ct);
    for (int i = 0; i < BOUNCE_PAGES; i++)
        if (p->bounce[i]) pmm_free(p->bounce[i]);
    kfree(p);
}

/* IDENTIFY strings are big-endian within each 16-bit word, space padded. */
static void ata_string(char *dst, const uint16_t *words, int nwords) {
    for (int i = 0; i < nwords; i++) {
        dst[2 * i] = (char)(words[i] >> 8);
        dst[2 * i + 1] = (char)words[i];
    }
    int n = 2 * nwords;
    while (n > 0 && (dst[n - 1] == ' ' || dst[n - 1] == 0)) n--;
    dst[n] = 0;
}

static int port_identify(struct ahci_port *p) {
    int r = port_exec(p, ATA_IDENTIFY, 0, 0, 512, 0, TMO_IDENTIFY_MS);
    if (r) return r;
    const uint16_t *id = P2V(p->bounce[0]);
    ata_string(p->model, id + 27, 20);
    char serial[21];
    ata_string(serial, id + 10, 10);
    if (!(id[83] & (1u << 10))) {
        kprintf("[ahci] port %d: %s has no LBA48, not supported\n", p->num, p->model);
        return BLK_EINVAL;
    }
    /* Logical sector size (word 106 valid when bit 14 set and bit 15 clear). */
    if ((id[106] & 0xC000) == 0x4000 && (id[106] & (1u << 12))) {
        uint32_t words = id[117] | ((uint32_t)id[118] << 16);
        if (words != 256) {
            kprintf("[ahci] port %d: %s has %u-byte sectors, not supported\n", p->num, p->model, words * 2);
            return BLK_EINVAL;
        }
    }
    uint64_t sectors = 0;
    for (int i = 3; i >= 0; i--) sectors = (sectors << 16) | id[100 + i];
    p->blk.sectors = sectors;
    p->flush_cmd = (id[83] & (1u << 13)) ? ATA_FLUSH_CACHE_EXT : ATA_FLUSH_CACHE;
    kprintf("[ahci] port %d: \"%s\" serial \"%s\", %lu sectors (%lu MiB)\n", p->num, p->model, serial, sectors,
            sectors / 2048);
    return 0;
}

static void port_init(struct ahci_ctrl *c, int num) {
    volatile uint8_t *regs = c->mmio + 0x100 + num * 0x80;
    uint32_t ssts = *(volatile uint32_t *)(regs + PX_SSTS);
    if ((ssts & 0xF) != 3) return; /* no device, or no link */

    struct ahci_port *p = kmalloc(sizeof *p);
    if (!p) return;
    p->ctrl = c, p->regs = regs, p->num = num;
    if (port_stop(p)) {
        kprintf("[ahci] port %d: won't stop, skipping\n", num);
        kfree(p);
        return;
    }

    p->clb = pmm_alloc(), p->fb = pmm_alloc(), p->ct = pmm_alloc();
    int ok = phys_ok(c, p->clb) && phys_ok(c, p->fb) && phys_ok(c, p->ct);
    for (int i = 0; i < BOUNCE_PAGES; i++) ok = phys_ok(c, p->bounce[i] = pmm_alloc()) && ok;
    if (!ok) {
        kprintf("[ahci] port %d: no DMA-reachable memory\n", num);
        port_free(p);
        return;
    }
    px_wr(p, PX_CLB, (uint32_t)p->clb);
    px_wr(p, PX_CLBU, (uint32_t)(p->clb >> 32));
    px_wr(p, PX_FB, (uint32_t)p->fb);
    px_wr(p, PX_FBU, (uint32_t)(p->fb >> 32));
    px_wr(p, PX_IE, 0); /* we poll */
    uint32_t cmd = px_rd(p, PX_CMD);
    if (c->cap & CAP_SSS) cmd |= CMD_SUD;
    px_wr(p, PX_CMD, cmd | CMD_POD);

    if (port_start(p)) {
        kprintf("[ahci] port %d: drive stays busy, skipping\n", num);
        port_stop(p);
        port_free(p);
        return;
    }
    uint32_t sig = px_rd(p, PX_SIG);
    if (sig != SIG_ATA) {
        kprintf("[ahci] port %d: %s (sig %x), not driven\n", num,
                sig == SIG_ATAPI ? "ATAPI" : "non-ATA device", sig);
        port_stop(p);
        port_free(p);
        return;
    }
    if (port_identify(p)) {
        port_stop(p);
        port_free(p);
        return;
    }

    snprintf(p->blk.name, sizeof p->blk.name, "sata%d", nsata);
    strlcpy(p->blk.model, p->model, sizeof p->blk.model);
    p->blk.read = ahci_read;
    p->blk.write = ahci_write;
    p->blk.flush = ahci_flush;
    p->blk.priv = p;
    sata[nsata++] = p;
    blk_register(&p->blk);
}

/* BIOS/OS handoff (AHCI 1.3.1 §10.6.3): ask for ownership, give the BIOS
 * 25 ms to let go, and up to 2 s more if it says it's busy. */
static void bios_handoff(struct ahci_ctrl *c) {
    if (!(hba_rd(c, HBA_CAP2) & CAP2_BOH)) return;
    hba_wr(c, HBA_BOHC, hba_rd(c, HBA_BOHC) | BOHC_OOS);
    struct tmo t;
    tmo_start(&t, 25);
    while ((hba_rd(c, HBA_BOHC) & BOHC_BOS) && !tmo_expired(&t)) __asm__ volatile("pause");
    if (hba_rd(c, HBA_BOHC) & BOHC_BB) {
        tmo_start(&t, 2000);
        while ((hba_rd(c, HBA_BOHC) & BOHC_BB) && !tmo_expired(&t)) __asm__ volatile("pause");
    }
    kprintf("[ahci] BIOS handoff %s\n", (hba_rd(c, HBA_BOHC) & (BOHC_BOS | BOHC_BB)) ? "timed out" : "done");
}

static int map_abar(struct ahci_ctrl *c, uint64_t abar) {
    uint64_t virt = ABAR_WINDOW + (uint64_t)nctrl * ABAR_WINDOW_STRIDE;
    uint64_t base = abar & ~(PAGE_SIZE - 1), size = ALIGN_UP((abar - base) + 0x1100, PAGE_SIZE);
    for (uint64_t o = 0; o < size; o += PAGE_SIZE) {
        if (vmm_map(vmm_kernel_pml4(), virt + o, base + o, PTE_RW | PTE_NX | PTE_G | PTE_PCD | PTE_PWT)) return -1;
        invlpg(virt + o);
    }
    c->mmio = (volatile uint8_t *)(virt + (abar - base));
    return 0;
}

static void ctrl_init(struct pci_dev *d) {
    if (nctrl == MAX_CTRL) return;
    int is_io;
    uint64_t abar = pci_bar_ex(d, 5, &is_io);
    if (!abar || is_io) {
        kprintf("[ahci] %02x:%02x.%u: BAR5 not a memory BAR\n", d->bus, d->dev, d->fn);
        return;
    }
    struct ahci_ctrl *c = &ctrls[nctrl];
    c->pci = d;
    pci_enable_bus_master_mem(d);
    if (map_abar(c, abar)) {
        kprintf("[ahci] out of memory mapping ABAR\n");
        return;
    }
    nctrl++;

    hba_wr(c, HBA_GHC, hba_rd(c, HBA_GHC) | GHC_AE);
    bios_handoff(c);
    hba_wr(c, HBA_GHC, (hba_rd(c, HBA_GHC) | GHC_AE) & ~GHC_IE);
    c->cap = hba_rd(c, HBA_CAP);
    uint32_t vs = hba_rd(c, HBA_VS), pi = hba_rd(c, HBA_PI);
    char ver[16]; /* VS: major in [31:16], minor as two BCD-ish bytes: 0x0301 = 1.3.1 */
    if (vs & 0xFF) snprintf(ver, sizeof ver, "%u.%u.%u", vs >> 16, (vs >> 8) & 0xFF, vs & 0xFF);
    else snprintf(ver, sizeof ver, "%u.%u", vs >> 16, (vs >> 8) & 0xFF);
    kprintf("[ahci] %02x:%02x.%u AHCI %s, ABAR %lx, %u ports (pi %x), %u slots, %s DMA\n", d->bus, d->dev, d->fn, ver,
            abar, (c->cap & 0x1F) + 1, pi, ((c->cap >> 8) & 0x1F) + 1, (c->cap & CAP_S64A) ? "64-bit" : "32-bit");
    for (int i = 0; i < MAX_PORTS; i++)
        if (pi & (1u << i)) port_init(c, i);
    hba_wr(c, HBA_IS, 0xFFFFFFFFu);
}

void ahci_init(void) {
    struct pci_dev *d;
    for (int i = 0; (d = pci_find_class(0x01, 0x06, 0x01, i)); i++) ctrl_init(d);
    for (int i = 0; (d = pci_find_class(0x01, 0x08, 0x02, i)); i++)
        kprintf("[nvme] %02x:%02x.%u %04x:%04x BAR0 %lx: NVMe controller present\n", d->bus, d->dev,
                d->fn, d->vendor, d->device, pci_bar(d, 0));
    if (!nctrl) kprintf("[ahci] no AHCI controller\n");
    else kprintf("[ahci] %d SATA drive%s\n", nsata, nsata == 1 ? "" : "s");
}

/* ---- Self-test ----------------------------------------------------------- */

static int contains(const char *s, const char *needle) {
    size_t n = strlen(needle);
    for (; *s; s++)
        if (!strncmp(s, needle, n)) return 1;
    return 0;
}

#define ST_SECTORS 8
#define ST_BYTES (ST_SECTORS * BLK_SECTOR)

static int selftest_one(struct ahci_port *p) {
    struct blkdev *d = &p->blk;
    if (d->sectors < 2 * ST_SECTORS) return -1;
    /* Odd offsets into the heap blocks: the bounce path must not care. */
    uint8_t *mem = kmalloc(3 * ST_BYTES + 16);
    if (!mem) return BLK_ENOMEM;
    uint8_t *saved = mem + 1, *pat = saved + ST_BYTES + 1, *back = pat + ST_BYTES + 1;
    uint64_t last = d->sectors - ST_SECTORS;
    const char *step = "read lba 0-7";
    int r = blk_read(d, 0, ST_SECTORS, back);
    if (!r) step = "save last 8", r = blk_read(d, last, ST_SECTORS, saved);
    if (!r) {
        uint64_t seed = random_u64();
        for (uint32_t i = 0; i < ST_BYTES; i++) pat[i] = (uint8_t)(seed >> ((i % 8) * 8)) ^ (uint8_t)(i * 31 + (i >> 9));
        step = "write pattern", r = blk_write(d, last, ST_SECTORS, pat);
        if (!r) step = "flush", r = blk_flush(d);
        if (!r) step = "read back", r = blk_read(d, last, ST_SECTORS, back);
        if (!r && memcmp(pat, back, ST_BYTES)) step = "compare", r = BLK_EIO;
        /* Always try to put the original data back, even after a failure. */
        int rr = blk_write(d, last, ST_SECTORS, saved);
        if (!rr) rr = blk_flush(d);
        if (!rr) rr = blk_read(d, last, ST_SECTORS, back);
        if (!rr && memcmp(saved, back, ST_BYTES)) rr = BLK_EIO;
        if (!r && rr) step = "restore", r = rr;
    }
    /* One read spanning several bounce-buffer loads must end with the same 8 sectors. */
    uint32_t big = (uint32_t)MIN(2 * BOUNCE_SECTORS + ST_SECTORS + 3, d->sectors);
    uint8_t *bigbuf = r ? 0 : kmalloc((uint64_t)big * BLK_SECTOR + 1);
    if (bigbuf) {
        step = "multi-chunk read", r = blk_read(d, d->sectors - big, big, bigbuf + 1);
        if (!r && memcmp(bigbuf + 1 + (uint64_t)(big - ST_SECTORS) * BLK_SECTOR, saved, ST_BYTES)) r = BLK_EIO;
        kfree(bigbuf);
    }
    kfree(mem);
    if (r) kprintf("ahci: selftest FAILED on %s at step \"%s\" (error %d)\n", d->name, step, r);
    else kprintf("ahci: selftest ok on %s (read lba 0-7, wrote+verified+restored lba %lu-%lu)\n", d->name, last,
                 last + ST_SECTORS - 1);
    return r;
}

int ahci_selftest(void) {
    int fails = 0;
    for (int i = 0; i < nsata; i++) {
        if (!contains(sata[i]->model, "QEMU")) {
            kprintf("ahci: selftest skipped on %s (\"%s\" is not a QEMU disk; real disks are never written)\n",
                    sata[i]->blk.name, sata[i]->model);
            continue;
        }
        if (selftest_one(sata[i])) fails++;
    }
    return fails ? -1 : 0;
}
