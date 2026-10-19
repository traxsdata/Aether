/* update.h - installation, A/B system slots and self-updates. */
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "../include/abi.h"

struct blkdev;
extern uint32_t boot_flags; /* BOOT_FLAG_*, from the loader */
extern uint32_t boot_slot;  /* A/B slot, or BOOT_NO_SLOT */

/* Scan the whole disks once, after the storage drivers are up. Builds a
 * partition view and adds the encrypted data partition as a block device. */
void update_init(void);

/* The AFS partition of the first installed disk, or 0. */
struct blkdev *update_data_partition(void);

/* Called once the kernel and its storage path are up: marks the booted slot
 * good, so the loader stops counting attempts against it. */
void update_boot_good(void);

/* Whole disks the installer may target. Returns a count. */
int update_list_disks(struct disk_info *out, int max);

/* Install a fresh system from the netboot installer. Both images have been
 * digest-checked by the caller; the kernel repeats the system image's own
 * header/hash checks. Returns 0 or -E_*. */
int update_install(int disk, const void *boot_image, uint64_t boot_len, const void *system_image, uint64_t system_len,
                   const char *passphrase, size_t passphrase_len);

/* Write a new system image to the inactive slot and make it the next boot.
 * Refused unless this system was itself booted from an installed slot. */
int update_install_system(const void *system_image, uint64_t system_len);

void update_info(struct update_info *out);
void system_reboot(void);