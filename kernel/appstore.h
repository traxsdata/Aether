/* appstore.h - persistent app packages in /system/apps (0.6). */
#pragma once
#include <stdint.h>
#include "../include/abi.h"

/* Largest package the kernel accepts. PhotoCraft's stripped package is about
 * 70 MiB; the bytes live in physical pages (not the kernel heap) and every
 * install still needs a contiguous allocation of this size to succeed. */
#define APP_MAX_BYTES (128ull << 20)

/* Create /system/apps and register the packages stored there. Called after
 * the encrypted volume is unlocked (or temporary storage is ready). */
void appstore_init(void);

/* Install or replace a package already copied into physical pages. On
 * success the program table owns those pages; on failure the caller frees
 * them. `flags` is APP_INSTALL_*. */
int appstore_install(uint64_t package_phys, uint64_t len, uint32_t flags);

/* Remove a user-installed app. Refused while it is running. */
int appstore_remove(const char *name);

/* Fill `out` with up to `max` apps; returns the count. */
int appstore_list(struct app_info *out, int max);
int appstore_info(const char *name, struct app_info *out);
/* The app's package icon, for SYS_APP_ICON: 1 and the pixels/dimensions if it
 * has one, 0 if not. The pointer is the package's own memory and stays valid
 * until the package is replaced or removed. */
int appstore_icon(const char *name, const uint8_t **data, uint32_t *w, uint32_t *h);
