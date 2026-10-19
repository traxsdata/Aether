/*
 * blk.h - block devices: a fixed table of drives addressed in 512-byte
 * sectors. Drivers fill in a struct blkdev and register it; everything
 * above (filesystems, the self-test) only sees this interface.
 */
#pragma once
#include <stdint.h>

#define BLK_SECTOR 512
#define BLK_MAX 16

#define BLK_EIO (-1)      /* device reported an error */
#define BLK_ETIMEDOUT (-2) /* device didn't answer in time */
#define BLK_ERANGE (-3)   /* lba/count outside the device */
#define BLK_ENOMEM (-4)
#define BLK_EINVAL (-5)

struct blkdev {
    char name[16];
    char model[41]; /* drive model, or the partition's GPT name */
    uint64_t sectors; /* 512-byte sectors */
    /* 0 ok, <0 error (BLK_E*). buf is any kernel virtual buffer, no alignment needed. */
    int (*read)(struct blkdev *d, uint64_t lba, uint32_t count, void *buf);
    int (*write)(struct blkdev *d, uint64_t lba, uint32_t count, const void *buf);
    int (*flush)(struct blkdev *d);
    void *priv;
};

void blk_register(struct blkdev *d);
struct blkdev *blk_get(int idx); /* 0 if idx is out of range */
struct blkdev *blk_find(const char *name);
int blk_count(void);

/* Range-checked wrappers: reject lba+count beyond the device, count 0 is a no-op. */
int blk_read(struct blkdev *d, uint64_t lba, uint32_t count, void *buf);
int blk_write(struct blkdev *d, uint64_t lba, uint32_t count, const void *buf);
int blk_flush(struct blkdev *d);

/* RAM disk of `sectors` sectors, zero-filled, registered as "ram0", "ram1"... 0 on OOM. */
struct blkdev *ramdisk_create(uint64_t sectors);

/* A byte-for-byte view of a partition of `parent`, for filesystems that
 * expect to start at block 0. `first` is in 512-byte sectors. */
struct blkdev *blk_partition(struct blkdev *parent, uint64_t first, uint64_t sectors, const char *name,
                             const char *model);
/* The whole disk behind a partition, or 0 for a whole disk. */
struct blkdev *blk_parent(struct blkdev *d);
