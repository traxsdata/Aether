/*
 * blk.c - block device registry and a RAM disk.
 *
 * The RAM disk keeps an array of physical pages (from kmalloc) and reaches
 * them through the direct map, so it needs no contiguous memory.
 */
#include "blk.h"
#include "kernel.h"
#include "mm.h"

static struct blkdev *devs[BLK_MAX];
static int ndevs, nram;

void blk_register(struct blkdev *d) {
    if (ndevs == BLK_MAX) {
        kprintf("[blk] table full, dropping %s\n", d->name);
        return;
    }
    devs[ndevs++] = d;
    kprintf("[blk] %s: %lu sectors (%lu MiB)\n", d->name, d->sectors, d->sectors / 2048);
}

struct blkdev *blk_get(int idx) { return idx >= 0 && idx < ndevs ? devs[idx] : 0; }
int blk_count(void) { return ndevs; }

struct blkdev *blk_find(const char *name) {
    for (int i = 0; i < ndevs; i++)
        if (!strcmp(devs[i]->name, name)) return devs[i];
    return 0;
}

static int in_range(struct blkdev *d, uint64_t lba, uint32_t count) {
    return lba < d->sectors && count <= d->sectors - lba;
}

int blk_read(struct blkdev *d, uint64_t lba, uint32_t count, void *buf) {
    if (!d || !buf) return BLK_EINVAL;
    if (!count) return 0;
    if (!in_range(d, lba, count)) return BLK_ERANGE;
    return d->read(d, lba, count, buf);
}

int blk_write(struct blkdev *d, uint64_t lba, uint32_t count, const void *buf) {
    if (!d || !buf) return BLK_EINVAL;
    if (!count) return 0;
    if (!in_range(d, lba, count)) return BLK_ERANGE;
    return d->write(d, lba, count, buf);
}

int blk_flush(struct blkdev *d) {
    if (!d) return BLK_EINVAL;
    return d->flush ? d->flush(d) : 0;
}

/* ---- RAM disk ----------------------------------------------------------- */

#define SECTORS_PER_PAGE (PAGE_SIZE / BLK_SECTOR)

struct ramdisk {
    uint64_t npages;
    uint64_t *pages; /* physical addresses */
};

static int ram_rw(struct blkdev *d, uint64_t lba, uint32_t count, uint8_t *buf, int write) {
    struct ramdisk *r = d->priv;
    if (!in_range(d, lba, count)) return BLK_ERANGE;
    uint64_t off = lba * BLK_SECTOR, left = (uint64_t)count * BLK_SECTOR;
    while (left) {
        uint64_t in_page = off % PAGE_SIZE, n = MIN(left, PAGE_SIZE - in_page);
        uint8_t *p = (uint8_t *)P2V(r->pages[off / PAGE_SIZE]) + in_page;
        if (write) memcpy(p, buf, n);
        else memcpy(buf, p, n);
        buf += n, off += n, left -= n;
    }
    return 0;
}

static int ram_read(struct blkdev *d, uint64_t lba, uint32_t count, void *buf) { return ram_rw(d, lba, count, buf, 0); }
static int ram_write(struct blkdev *d, uint64_t lba, uint32_t count, const void *buf) {
    return ram_rw(d, lba, count, (uint8_t *)buf, 1);
}
static int ram_flush(struct blkdev *d) { return 0; }

struct blkdev *ramdisk_create(uint64_t sectors) {
    if (!sectors) return 0;
    uint64_t npages = (sectors + SECTORS_PER_PAGE - 1) / SECTORS_PER_PAGE;
    struct blkdev *d = kmalloc(sizeof *d);
    struct ramdisk *r = kmalloc(sizeof *r);
    uint64_t *pages = kmalloc(npages * sizeof(uint64_t));
    if (!d || !r || !pages) goto fail;
    for (uint64_t i = 0; i < npages; i++) {
        if (!(pages[i] = pmm_alloc())) {
            while (i--) pmm_free(pages[i]);
            goto fail;
        }
    }
    r->npages = npages;
    r->pages = pages;
    snprintf(d->name, sizeof d->name, "ram%d", nram++);
    strlcpy(d->model, "RAM disk", sizeof d->model);
    d->sectors = sectors;
    d->read = ram_read;
    d->write = ram_write;
    d->flush = ram_flush;
    d->priv = r;
    blk_register(d);
    return d;
fail:
    kfree(pages);
    kfree(r);
    kfree(d);
    return 0;
}

/* ---- Partitions --------------------------------------------------------- */

struct partition {
    struct blkdev *parent;
    uint64_t first;
};

static int part_read(struct blkdev *d, uint64_t lba, uint32_t count, void *buf) {
    struct partition *p = d->priv;
    return blk_read(p->parent, p->first + lba, count, buf);
}

static int part_write(struct blkdev *d, uint64_t lba, uint32_t count, const void *buf) {
    struct partition *p = d->priv;
    return blk_write(p->parent, p->first + lba, count, buf);
}

static int part_flush(struct blkdev *d) {
    struct partition *p = d->priv;
    return blk_flush(p->parent);
}

struct blkdev *blk_parent(struct blkdev *d) {
    return d && d->priv && d->read == part_read ? ((struct partition *)d->priv)->parent : 0;
}

struct blkdev *blk_partition(struct blkdev *parent, uint64_t first, uint64_t sectors, const char *name,
                             const char *model) {
    if (!parent || !sectors || first >= parent->sectors || sectors > parent->sectors - first) return 0;
    struct blkdev *d = kmalloc(sizeof *d);
    struct partition *p = kmalloc(sizeof *p);
    if (!d || !p) {
        kfree(d);
        kfree(p);
        return 0;
    }
    p->parent = parent;
    p->first = first;
    memset(d, 0, sizeof *d);
    strlcpy(d->name, name, sizeof d->name);
    strlcpy(d->model, model ? model : "", sizeof d->model);
    d->sectors = sectors;
    d->read = part_read;
    d->write = part_write;
    d->flush = part_flush;
    d->priv = p;
    blk_register(d);
    return d;
}
