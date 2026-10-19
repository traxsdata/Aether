/*
 * settings.c - a tiny key/value store for the Settings app.
 *
 * It lives at /system/settings, in the kernel-only /system folder, and is
 * deliberately plain text (one "key=value" line per setting): small, easy
 * to audit, and no parser to attack. Keys the kernel owns today are the
 * network configuration; future settings can be added without an ABI change.
 */
#include "settings.h"
#include "afs.h"
#include "kernel.h"
#include "mm.h"
#include "net.h"
#include "storage.h"

#define MAX_ENTRIES 32
#define SETTINGS_MAX_BYTES 4096
static const char magic[] = "AESET1\n";

static struct {
    char key[SETTINGS_KEY_MAX];
    char value[SETTINGS_VALUE_MAX];
} entries[MAX_ENTRIES];
static int nentries;
static uint32_t file_ino;

static int key_ok(const char *key) {
    size_t n = strnlen(key, SETTINGS_KEY_MAX);
    if (!n || n >= SETTINGS_KEY_MAX) return 0;
    for (size_t i = 0; i < n; i++) {
        char c = key[i];
        if (!((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '.' || c == '_' || c == '-')) return 0;
    }
    return 1;
}

static int find(const char *key) {
    for (int i = 0; i < nentries; i++)
        if (!strcmp(entries[i].key, key)) return i;
    return -1;
}

int settings_get(const char *key, char *out, size_t len) {
    int i = find(key);
    if (i < 0) return -E_NOENT;
    size_t n = strlen(entries[i].value);
    if (n >= len) n = len ? len - 1 : 0;
    if (len) {
        memcpy(out, entries[i].value, n);
        out[n] = 0;
    }
    return (int)n;
}

static int save(void) {
    if (!afs_mounted()) return -E_LOCKED;
    uint32_t sys = vfs_system();
    if (!sys) return -E_LOCKED;
    char text[SETTINGS_MAX_BYTES];
    size_t n = strlen(magic);
    memcpy(text, magic, n);
    for (int i = 0; i < nentries; i++) {
        int w = snprintf(text + n, sizeof text - n, "%s=%s\n", entries[i].key, entries[i].value);
        if (w <= 0 || (size_t)w >= sizeof text - n) return -E_LIMIT;
        n += (size_t)w;
    }
    if (!file_ino) {
        uint32_t ino;
        if (!afs_lookup(sys, "settings", &ino)) file_ino = ino;
        else {
            int f = afs_ialloc(AFS_FILE, LABEL_PERSONAL, sys);
            if (f < 0) return f;
            int err = afs_dir_add(sys, "settings", (uint32_t)f, AFS_FILE);
            if (err) {
                afs_ifree((uint32_t)f);
                return err;
            }
            file_ino = (uint32_t)f;
        }
    }
    int err = afs_truncate(file_ino, 0);
    if (err) return err;
    int64_t wrote = afs_write(file_ino, 0, text, n);
    if (wrote != (int64_t)n) return wrote < 0 ? (int)wrote : -E_NOSPACE;
    afs_sync();
    return 0;
}

static int value_ok(const char *value) {
    if (strlen(value) >= SETTINGS_VALUE_MAX) return 0;
    for (const unsigned char *p = (const unsigned char *)value; *p; p++)
        if (*p < 32 || *p == 127) return 0; /* no newlines or other control characters */
    return 1;
}

int settings_set(const char *key, const char *value) {
    if (!key_ok(key) || !value_ok(value)) return -E_INVAL;
    int i = find(key);
    /* Refuse an entry that cannot fit in the small settings file, instead of
     * silently dropping it (or every later entry) on the next save. */
    size_t total = strlen(magic);
    for (int k = 0; k < nentries; k++)
        if (k != i) total += strlen(entries[k].key) + strlen(entries[k].value) + 2;
    total += strlen(key) + strlen(value) + 2;
    if (total >= SETTINGS_MAX_BYTES) return -E_LIMIT;
    if (i < 0) {
        if (nentries >= MAX_ENTRIES) return -E_LIMIT;
        i = nentries++;
        strlcpy(entries[i].key, key, sizeof entries[i].key);
    }
    char old[SETTINGS_VALUE_MAX];
    strlcpy(old, entries[i].value, sizeof old);
    strlcpy(entries[i].value, value, sizeof entries[i].value);
    int err = save();
    if (err) strlcpy(entries[i].value, old, sizeof entries[i].value); /* keep memory and disk in step */
    return err;
}

static void apply_net(void) {
    char mode[SETTINGS_VALUE_MAX];
    if (settings_get("net.mode", mode, sizeof mode) < 0) return;
    struct net_config cfg;
    memset(&cfg, 0, sizeof cfg);
    if (!strcmp(mode, "static")) {
        char ip[32], mask[32], gw[32], dns[32];
        if (settings_get("net.ip", ip, sizeof ip) < 0 || settings_get("net.mask", mask, sizeof mask) < 0) return;
        if (settings_get("net.gateway", gw, sizeof gw) < 0) gw[0] = 0;
        if (settings_get("net.dns", dns, sizeof dns) < 0) dns[0] = 0;
        cfg.mode = NET_MODE_STATIC;
        if (net_parse_ipv4(ip, &cfg.ip) || net_parse_ipv4(mask, &cfg.netmask)) return;
        if (gw[0] && net_parse_ipv4(gw, &cfg.gateway)) return;
        if (dns[0] && net_parse_ipv4(dns, &cfg.dns)) return;
    } else {
        cfg.mode = NET_MODE_DHCP;
    }
    net_apply_config(&cfg);
}

void settings_save_net(const struct net_config *cfg) {
    char buf[32];
    settings_set("net.mode", cfg->mode == NET_MODE_STATIC ? "static" : "dhcp");
    if (cfg->mode != NET_MODE_STATIC) return;
    uint8_t *p = (uint8_t *)&cfg->ip;
    snprintf(buf, sizeof buf, "%u.%u.%u.%u", p[0], p[1], p[2], p[3]);
    settings_set("net.ip", buf);
    p = (uint8_t *)&cfg->netmask;
    snprintf(buf, sizeof buf, "%u.%u.%u.%u", p[0], p[1], p[2], p[3]);
    settings_set("net.mask", buf);
    p = (uint8_t *)&cfg->gateway;
    snprintf(buf, sizeof buf, "%u.%u.%u.%u", p[0], p[1], p[2], p[3]);
    settings_set("net.gateway", buf);
    p = (uint8_t *)&cfg->dns;
    snprintf(buf, sizeof buf, "%u.%u.%u.%u", p[0], p[1], p[2], p[3]);
    settings_set("net.dns", buf);
}

void settings_init(void) {
    nentries = 0;
    file_ino = 0;
    uint32_t sys = vfs_system(), ino;
    if (!sys || afs_lookup(sys, "settings", &ino)) return;
    file_ino = ino;
    struct afs_inode n;
    if (afs_iget(ino, &n) || n.size == 0 || n.size > SETTINGS_MAX_BYTES) return;
    char *text = kmalloc(n.size + 1);
    if (!text) return;
    if (afs_read(ino, 0, text, n.size) != (int64_t)n.size) {
        kfree(text);
        return;
    }
    text[n.size] = 0;
    char *line = text;
    if (!strncmp(line, magic, strlen(magic))) line += strlen(magic);
    while (*line) {
        char *end = line;
        while (*end && *end != '\n') end++;
        int more = *end == '\n';
        *end = 0;
        char *eq = line;
        while (*eq && *eq != '=') eq++;
        if (*eq == '=') {
            *eq = 0;
            if (key_ok(line) && value_ok(eq + 1) && find(line) < 0 && nentries < MAX_ENTRIES) {
                strlcpy(entries[nentries].key, line, sizeof entries[nentries].key);
                strlcpy(entries[nentries].value, eq + 1, sizeof entries[nentries].value);
                nentries++;
            }
        }
        line = more ? end + 1 : end;
    }
    kfree(text);
    if (nentries) kprintf("[settings] %d setting%s loaded\n", nentries, nentries == 1 ? "" : "s");
    apply_net();
}
