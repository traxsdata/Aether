/* storage.h - choosing, unlocking and preparing the encrypted volume at boot. */
#pragma once
#include <stddef.h>
#include <stdint.h>

void storage_init(void (*ready)(void)); /* calls ready() now, or once the user unlocks */
int storage_submit(const char *pw, size_t len); /* from the unlock screen: 0, or -E_* */
const char *storage_device(void);
void vfs_init_layout(void); /* vfs.c */
uint32_t vfs_home(void);
uint32_t vfs_system(void); /* the kernel's own folder: apps can't open it */
