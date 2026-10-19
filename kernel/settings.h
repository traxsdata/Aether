/* settings.h - small persistent key/value settings in /system/settings (0.6). */
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "../include/abi.h"

void settings_init(void); /* after vfs_init_layout(): load and apply */
int settings_get(const char *key, char *out, size_t len); /* bytes, or -E_NOENT */
int settings_set(const char *key, const char *value);
/* Persist the network configuration (called by SYS_NET_CONFIG). */
void settings_save_net(const struct net_config *cfg);
