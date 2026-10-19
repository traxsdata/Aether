/*
 * firewall.c - which app may connect to which host.
 *
 * Inbound traffic needs no rules: nothing listens (net.c). Outbound, every
 * connection is checked here by app name, host name and port before a
 * single packet goes out. The first matching rule decides; then the
 * built-in web policy (below); with neither, the user is asked in a
 * kernel-drawn dialog (wm.c), and "Always allow" adds a rule. Rules are by name, not address, because names are what people
 * can judge ("browser wants example.com"), and the address a name resolves
 * to is checked separately (net_addr_allowed).
 *
 * The rules live in the encrypted volume at /system/firewall, a folder no
 * app can open (vfs.c refuses /system), so only the kernel and the
 * terminal's `fw` command (a system app, through SYS_FIREWALL) change them.
 */
#include "afs.h"
#include "kernel.h"
#include "net.h"

#define MAX_RULES 64
#define FW_MAGIC 0x31574641u /* "AFW1" */

static struct fw_rule rules[MAX_RULES];
static int nrules;
static uint32_t file_ino;

static int lower(int c) { return c >= 'A' && c <= 'Z' ? c + 32 : c; }

static int ieq(const char *a, const char *b) {
    for (; *a && *b; a++, b++)
        if (lower(*a) != lower(*b)) return 0;
    return *a == *b;
}

/* "*" matches anything; "*.example.com" matches names under example.com
 * (not example.com itself); anything else must match exactly. */
int fw_host_match(const char *pattern, const char *host) {
    if (!strcmp(pattern, "*")) return 1;
    if (pattern[0] == '*' && pattern[1] == '.') {
        size_t pl = strlen(pattern + 1), hl = strlen(host); /* ".example.com" */
        return hl > pl && ieq(host + hl - pl, pattern + 1);
    }
    return ieq(pattern, host);
}

/*
 * The built-in web policy: the browser may open https (443) and http (80)
 * connections to any host without asking. Asking for every site made the
 * dialog a reflex rather than a decision; the browser already puts plain
 * http behind its own warning and keeps nothing between sessions. Every
 * other check still applies: your rules come first (so `fw deny browser
 * tracker.example` or even `fw deny browser *` still wins), DLP still cuts
 * off an app that has read confidential data, and the address must still be
 * a public one, which stops a page from reaching your router or a metadata
 * service through DNS rebinding. Any other port, or any other app, asks.
 */
static const struct {
    const char *app;
    uint16_t port;
} web_policy[] = {{"browser", 443}, {"browser", 80}};

static uint32_t web_hits[ARRAY_LEN(web_policy)];

int fw_builtin(const char *app, uint16_t port) {
    for (size_t i = 0; i < ARRAY_LEN(web_policy); i++)
        if (!strcmp(web_policy[i].app, app) && web_policy[i].port == port) return (int)i;
    return -1;
}

int fw_builtin_get(int i, struct fw_rule *out) {
    if (i < 0 || i >= (int)ARRAY_LEN(web_policy)) return -E_NOENT;
    memset(out, 0, sizeof *out);
    strlcpy(out->app, web_policy[i].app, sizeof out->app);
    strlcpy(out->host, "*", sizeof out->host);
    out->port = web_policy[i].port;
    out->action = FW_ALLOW;
    out->hits = web_hits[i];
    return 0;
}

int fw_check(const char *app, const char *host, uint16_t port, int allow_builtin) {
    for (int i = 0; i < nrules; i++) {
        struct fw_rule *r = &rules[i];
        if (strcmp(r->app, "*") && !ieq(r->app, app)) continue;
        if (r->port && r->port != port) continue;
        if (!fw_host_match(r->host, host)) continue;
        r->hits++;
        return r->action == FW_ALLOW ? 1 : 0;
    }
    int b = allow_builtin ? fw_builtin(app, port) : -1;
    if (b >= 0) {
        web_hits[b]++;
        return 1;
    }
    return -1;
}

static void save(void) {
    if (!afs_mounted()) return;
    if (!file_ino) {
        uint32_t dir;
        if (afs_lookup(AFS_ROOT, "system", &dir)) {
            int d = afs_ialloc(AFS_DIR, LABEL_PERSONAL, AFS_ROOT);
            if (d < 0 || afs_dir_add(AFS_ROOT, "system", (uint32_t)d, AFS_DIR)) return;
            dir = (uint32_t)d;
        }
        if (afs_lookup(dir, "firewall", &file_ino)) {
            int f = afs_ialloc(AFS_FILE, LABEL_PERSONAL, dir); /* which sites you use is personal */
            if (f < 0 || afs_dir_add(dir, "firewall", (uint32_t)f, AFS_FILE)) {
                file_ino = 0;
                return;
            }
            file_ino = (uint32_t)f;
        }
    }
    uint32_t hdr[2] = {FW_MAGIC, (uint32_t)nrules};
    afs_truncate(file_ino, 0);
    afs_write(file_ino, 0, hdr, sizeof hdr);
    afs_write(file_ino, sizeof hdr, rules, (uint64_t)nrules * sizeof rules[0]);
    afs_sync();
}

static int valid(const struct fw_rule *r) {
    if (!r->app[0] || !r->host[0] || (r->action != FW_ALLOW && r->action != FW_DENY)) return 0;
    if (!strcmp(r->host, "*")) return 1;
    const char *h = r->host[0] == '*' && r->host[1] == '.' ? r->host + 2 : r->host;
    return net_host_ok(h);
}

void fw_load(void) {
    nrules = 0;
    file_ino = 0;
    uint32_t dir, ino, hdr[2];
    if (afs_lookup(AFS_ROOT, "system", &dir) || afs_lookup(dir, "firewall", &ino)) return;
    file_ino = ino;
    if (afs_read(ino, 0, hdr, sizeof hdr) != sizeof hdr || hdr[0] != FW_MAGIC || hdr[1] > MAX_RULES) return;
    for (uint32_t i = 0; i < hdr[1]; i++) {
        struct fw_rule r;
        if (afs_read(ino, sizeof hdr + i * sizeof r, &r, sizeof r) != sizeof r) break;
        r.app[sizeof r.app - 1] = 0;
        r.host[sizeof r.host - 1] = 0;
        if (valid(&r)) rules[nrules++] = r;
    }
    kprintf("[fw] %d firewall rule%s loaded\n", nrules, nrules == 1 ? "" : "s");
}

int fw_add(const struct fw_rule *in) {
    if (!valid(in)) return -E_INVAL;
    struct fw_rule r = *in;
    for (char *s = r.host; *s; s++) *s = (char)lower(*s);
    if (strcmp(r.app, "*")) for (char *s = r.app; *s; s++) *s = (char)lower(*s);
    r.hits = 0;
    /* A new decision for the same app, host and port replaces the old one. */
    for (int i = 0; i < nrules; i++)
        if (ieq(rules[i].app, r.app) && !strcmp(rules[i].host, r.host) && rules[i].port == r.port) {
            rules[i] = r;
            save();
            return i;
        }
    if (nrules == MAX_RULES) return -E_LIMIT;
    /* Specific rules go before wildcard ones, so "deny *" can sit at the end. */
    int at = nrules;
    if (strcmp(r.host, "*") && strcmp(r.app, "*"))
        for (int i = 0; i < nrules; i++)
            if (!strcmp(rules[i].host, "*") || !strcmp(rules[i].app, "*")) {
                at = i;
                break;
            }
    for (int i = nrules; i > at; i--) rules[i] = rules[i - 1];
    rules[at] = r;
    nrules++;
    save();
    return at;
}

void fw_remember(const char *app, const char *host, uint16_t port, int allow) {
    struct fw_rule r;
    memset(&r, 0, sizeof r);
    strlcpy(r.app, app, sizeof r.app);
    strlcpy(r.host, host, sizeof r.host);
    r.port = port;
    r.action = allow ? FW_ALLOW : FW_DENY;
    if (fw_add(&r) >= 0) kprintf("[fw] rule added: %s %s %s:%u\n", allow ? "allow" : "deny", app, host, port);
}

int fw_count(void) { return nrules; }

int fw_get(int i, struct fw_rule *out) {
    if (i < 0 || i >= nrules) return -E_NOENT;
    *out = rules[i];
    return 0;
}

int fw_remove(int i) {
    if (i < 0 || i >= nrules) return -E_NOENT;
    for (int j = i; j < nrules - 1; j++) rules[j] = rules[j + 1];
    nrules--;
    save();
    return 0;
}
