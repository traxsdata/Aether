/*
 * storage.c - at boot, find where the user's files live:
 *   - a disk holding an æther volume: ask for the passphrase (unlock screen)
 *   - a completely blank disk: offer to set it up, encrypted
 *   - a disk with anything else on it: leave it alone (never format data
 *     we don't recognise) and use temporary storage in memory
 *   - no disk: temporary storage in memory, wiped at shutdown
 */
#include "storage.h"
#include "../include/abi.h"
#include "afs.h"
#include "appstore.h"
#include "blk.h"
#include "crypto.h"
#include "kernel.h"
#include "settings.h"
#include "update.h"
#include "wm.h"
#include "../include/bootinfo.h"

#define RAMDISK_SECTORS (64ull * 1024 * 1024 / 512)

static struct blkdev *disk;
static int mode;
static void (*on_ready)(void);
extern uint32_t boot_flags;

static void start_temporary(const char *why) {
    struct blkdev *ram = ramdisk_create(RAMDISK_SECTORS);
    if (!ram || afs_format(ram, 0, 0)) panic("storage: can't create temporary storage");
    disk = ram;
    vfs_init_layout();
    settings_init();
    appstore_init();
    kprintf("[storage] using temporary storage: %s\n", why);
}

void storage_init(void (*ready)(void)) {
    on_ready = ready;
    if (crypto_selftest()) panic("cryptography self-test failed; refusing to store files");
    kprintf("[storage] crypto self-test passed\n");
    update_init();

    /* The netboot installer must never format or unlock the target disk
     * behind the user's back. It runs from memory and sees the disks only
     * through SYS_DISK_LIST / SYS_SYSTEM_INSTALL. */
    if (boot_flags & BOOT_FLAG_NETBOOT) {
        start_temporary("netboot installer");
        wm_notify("Netboot installer", "The target disk is untouched until you choose Install.");
        ready();
        return;
    }

    struct blkdev *data = update_data_partition();
    if ((boot_flags & BOOT_FLAG_INSTALLED) && !data) {
        /* Never fall back to formatting some other disk when the boot disk's
         * data partition is missing or unreadable. */
        start_temporary("installed system has no matching data volume");
        wm_notify("Storage not found", "The installed disk's data partition is missing; using memory.");
        ready();
        return;
    }
    int n = blk_count();
    for (int i = -1; i < n; i++) {
        struct blkdev *d = i < 0 ? data : blk_get(i);
        if (!d || !strncmp(d->name, "ram", 3)) continue;
        if (i >= 0 && d->name[0] && data && d != data) continue; /* installed data partition is authoritative */
        int kind = afs_probe(d);
        if (kind == AFS_PROBE_AFS || kind == AFS_PROBE_BLANK) {
            disk = d;
            mode = kind == AFS_PROBE_AFS ? LOCK_UNLOCK : LOCK_SETUP;
            kprintf("[storage] %s: %s\n", d->name, kind == AFS_PROBE_AFS ? "encrypted volume found" : "blank disk");
            wm_set_lock(mode, ready);
            return;
        }
        kprintf("[storage] %s: has other data on it; leaving it untouched\n", d->name);
        if (i < 0) break;
    }
    start_temporary(blk_count() ? "no blank or æther disk" : "no disk");
    wm_notify("Temporary storage", "No blank disk found. Files are kept in memory and lost at shutdown.");
    ready();
}

int storage_submit(const char *pw, size_t len) {
    int err = mode == LOCK_SETUP ? afs_format(disk, pw, len) : afs_unlock(disk, pw, len);
    if (err) return err;
    vfs_init_layout();
    settings_init();
    appstore_init();
    return 0;
}

const char *storage_device(void) { return disk ? disk->name : "none"; }
