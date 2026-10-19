/*
 * terminal.c - the æther terminal: a scrollback view, a line editor with
 * history, and a small set of built-in commands.
 */
#include "lib/ui.h"

#define W 940
#define H 470
#define PAD 14
#define LINE_H 18
#define MAX_LINES 400
#define LINE_LEN 120
#define HIST 16

#define C_BG 0x0B1120
#define C_FG 0xD7DEF5
#define C_DIM 0x7C87A8
#define C_ACCENT 0x38E1F5
#define C_OK 0x5CF2B0
#define C_WARN 0xFFC857
#define C_ERR 0xFF5C7A
#define C_PURPLE 0xB794FF

struct line {
    char text[LINE_LEN];
    uint32_t color;
    int prompt; /* draw the prompt before the text */
};

static struct window win;
static struct line lines[MAX_LINES];
static int nlines;     /* total lines ever written (ring buffer index) */
static int scroll;     /* lines scrolled back from the bottom */
static char input[LINE_LEN];
static int input_len, cursor;
static char history[HIST][LINE_LEN];
static int nhist, hist_pos;
static int cursor_on = 1, focused = 1;

static const char *PROMPT = "\xC3\xA6 > ";

/* ---- Output ---------------------------------------------------------------- */

static void out_color(uint32_t color, const char *fmt, ...) {
    char buf[512];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    int cols = (W - 2 * PAD) / gfx_font_w(FONT_SMALL);
    const char *p = buf;
    do { /* split on newlines and wrap long lines */
        struct line *l = &lines[nlines % MAX_LINES];
        int n = 0;
        while (p[n] && p[n] != '\n' && n < cols && n < LINE_LEN - 1) n++;
        memcpy(l->text, p, (size_t)n);
        l->text[n] = 0;
        l->color = color;
        l->prompt = 0;
        nlines++;
        p += n;
        if (*p == '\n') p++;
    } while (*p);
    scroll = 0;
}

#define out(...) out_color(C_FG, __VA_ARGS__)

/* ---- Drawing --------------------------------------------------------------- */

static void draw(void) {
    struct surface *s = &win.s;
    gfx_fill(s, 0, 0, s->w, s->h, C_BG);
    gfx_blend(s, 0, 0, s->w, 2, C_ACCENT, 40);
    int rows = (s->h - 2 * PAD) / LINE_H;
    int visible_lines = rows - 1; /* last row is the input line */
    int avail = nlines < MAX_LINES ? nlines : MAX_LINES;
    int first = nlines - visible_lines - scroll;
    if (first < nlines - avail) first = nlines - avail;
    if (first < 0) first = 0; /* fill from the top until the screen is full */
    int y = PAD;
    for (int i = 0; i < visible_lines && first + i < nlines; i++, y += LINE_H) {
        int idx = first + i;
        struct line *l = &lines[idx % MAX_LINES];
        int x = PAD;
        if (l->prompt) x = gfx_text(s, x, y, PROMPT, C_ACCENT, FONT_SMALL);
        gfx_text(s, x, y, l->text, l->color, FONT_SMALL);
    }
    if (scroll) {
        char msg[48];
        snprintf(msg, sizeof msg, "-- scrolled back %d lines (PgDn) --", scroll);
        gfx_text(s, PAD, y, msg, C_WARN, FONT_SMALL);
    } else {
        int x = gfx_text(s, PAD, y, PROMPT, C_ACCENT, FONT_SMALL);
        gfx_text(s, x, y, input, C_FG, FONT_SMALL);
        if (cursor_on && focused) {
            int cx = x + cursor * gfx_font_w(FONT_SMALL);
            gfx_blend(s, cx, y, gfx_font_w(FONT_SMALL), LINE_H - 2, C_ACCENT, 170);
            if (cursor < input_len) {
                char c[2] = {input[cursor], 0};
                gfx_text(s, cx, y, c, C_BG, FONT_SMALL);
            }
        }
    }
    window_present(&win);
}

/* ---- Commands ---------------------------------------------------------------- */

static const char *caps_str(uint32_t caps, char *buf, size_t n) {
    static const char *names[] = {"window", "log", "sysinfo", "spawn", "kill", "files", "pick", "storage", "net"};
    buf[0] = 0;
    size_t len = 0;
    for (int i = 0; i < 9; i++)
        if (caps & (1u << i)) len += (size_t)snprintf(buf + len, n - len, "%s%s", len ? "," : "", names[i]);
    if (!len) strlcpy(buf, "(none)", n);
    return buf;
}

static int parse_int(const char *s, int *out) {
    int v = 0, any = 0;
    while (*s == ' ') s++;
    while (*s >= '0' && *s <= '9') v = v * 10 + (*s++ - '0'), any = 1;
    *out = v;
    return any && *s == 0;
}

static void err(int64_t e) {
    static const char *msgs[] = {"", "permission denied", "bad address", "invalid argument", "out of memory",
                                 "not found", "limit reached (or the file is open)", "timed out", "already exists",
                                 "not a folder", "is a folder", "disk full", "disk error or failed integrity check",
                                 "refused by data loss prevention (or by you)", "storage is locked", "folder isn't empty",
                                 "network unavailable, host not found, or connection refused"};
    out_color(C_ERR, "error: %s", (-e > 0 && -e < (int64_t)(sizeof msgs / sizeof msgs[0])) ? msgs[-e] : "unknown");
}

static int strstr_simple(const char *h, const char *n) {
    size_t ln = strlen(n);
    for (; *h; h++)
        if (!strncmp(h, n, ln)) return 1;
    return 0;
}

/* ---- Files ----------------------------------------------------------------------- */

static char cwd[PATH_MAX] = "/home";

static const uint32_t label_colors[] = {C_DIM, C_OK, C_WARN, C_ERR};

/* Turns `arg` (absolute, or relative to cwd) into a clean absolute path; 0 if too long. */
static int make_path(const char *arg, char *out) {
    char tmp[PATH_MAX * 2];
    while (*arg == ' ') arg++;
    if (*arg == '/') strlcpy(tmp, arg, sizeof tmp);
    else snprintf(tmp, sizeof tmp, "%s/%s", cwd, arg);
    size_t n = 0;
    out[0] = 0;
    char *s = tmp;
    while (*s) {
        while (*s == '/') s++;
        char *e = s;
        while (*e && *e != '/') e++;
        size_t len = (size_t)(e - s);
        if (len == 0 || (len == 1 && s[0] == '.')) {
        } else if (len == 2 && s[0] == '.' && s[1] == '.') {
            while (n > 0 && out[n - 1] != '/') n--;
            if (n > 0) n--;
            out[n] = 0;
        } else {
            if (n + len + 2 >= PATH_MAX) return 0;
            out[n++] = '/';
            memcpy(out + n, s, len);
            n += len;
            out[n] = 0;
        }
        s = e;
    }
    if (!n) strlcpy(out, "/", PATH_MAX);
    return 1;
}

/* Splits "a b" into a and b (in place); returns b, or 0 if there's no second word. */
static char *second_word(char *arg) {
    while (*arg == ' ') arg++;
    char *b = arg;
    while (*b && *b != ' ') b++;
    if (!*b) return 0;
    *b++ = 0;
    while (*b == ' ') b++;
    return *b ? b : 0;
}

static void cmd_ls(const char *arg) {
    char path[PATH_MAX];
    if (!make_path(*arg ? arg : ".", path)) return err(-E_INVAL);
    int h = (int)sys_open(path, O_DIR);
    if (h < 0) return err(h);
    struct dir_entry de;
    int n = 0;
    for (uint32_t i = 0; sys_readdir(h, i, &de) == 1; i++, n++) {
        char size[24] = "";
        if (de.type == FT_FILE) snprintf(size, sizeof size, "%lu B", de.size);
        out_color(de.type == FT_DIR ? C_ACCENT : C_FG, "  %-28s%s  %10s   %s", de.name, de.type == FT_DIR ? "/" : " ", size,
                  label_name(de.label));
    }
    sys_close(h);
    if (!n) out_color(C_DIM, "  (empty)");
}

static void cmd_cd(const char *arg) {
    char path[PATH_MAX];
    if (!make_path(*arg ? arg : "/home", path)) return err(-E_INVAL);
    struct file_stat st;
    int64_t r = sys_stat(path, &st);
    if (r < 0) return err(r);
    if (st.type != FT_DIR) return err(-E_NOTDIR);
    strlcpy(cwd, path, sizeof cwd);
}

static void cmd_cat(const char *arg) {
    char path[PATH_MAX];
    if (!*arg || !make_path(arg, path)) {
        out("usage: cat <file>");
        return;
    }
    int h = (int)sys_open(path, O_READ);
    if (h < 0) return err(h);
    static char buf[4097];
    int64_t n = sys_read(h, buf, 4096);
    sys_close(h);
    if (n < 0) return err(n);
    buf[n] = 0;
    char *line = buf;
    for (char *p = buf;; p++) {
        if (*p == '\n' || !*p) {
            char end = *p;
            *p = 0;
            out("%s", line);
            if (!end) break;
            line = p + 1;
        }
    }
    if (n == 4096) out_color(C_DIM, "(showing the first 4 KB)");
}

static void cmd_write(char *arg, int append) {
    char *text = second_word(arg);
    char path[PATH_MAX];
    if (!text || !make_path(arg, path)) {
        out("usage: %s <file> <text>", append ? "append" : "write");
        return;
    }
    int h = (int)sys_open(path, O_WRITE | O_CREATE | (append ? O_APPEND : O_TRUNC));
    if (h < 0) return err(h);
    char line[LINE_LEN + 1];
    int n = snprintf(line, sizeof line, "%s\n", text);
    int64_t r = sys_write(h, line, (uint64_t)n);
    sys_close(h);
    if (r < 0) return err(r);
    out_color(C_OK, "%s %d bytes to %s", append ? "appended" : "wrote", n, path);
}

static void cmd_path_op(const char *arg, int64_t (*op)(const char *), const char *usage, const char *done) {
    char path[PATH_MAX];
    if (!*arg || !make_path(arg, path)) {
        out("usage: %s", usage);
        return;
    }
    int64_t r = op(path);
    if (r < 0) return err(r);
    out_color(C_OK, "%s %s", done, path);
}

static int64_t do_mkdir(const char *p) { return sys_mkdir(p); }
static int64_t do_remove(const char *p) { return sys_remove(p); }

static void cmd_mv(char *arg) {
    char *to = second_word(arg);
    char a[PATH_MAX], b[PATH_MAX];
    if (!to || !make_path(arg, a) || !make_path(to, b)) {
        out("usage: mv <from> <to>");
        return;
    }
    int64_t r = sys_rename(a, b);
    if (r < 0) return err(r);
    out_color(C_OK, "moved %s to %s", a, b);
}

static void cmd_label(char *arg) {
    char *level = second_word(arg);
    char path[PATH_MAX];
    if (!*arg || !make_path(arg, path)) {
        out("usage: label <file> [public|personal|confidential|secret]");
        return;
    }
    struct file_stat st;
    int64_t r = sys_stat(path, &st);
    if (r < 0) return err(r);
    if (!level) {
        out_color(label_colors[st.label & 3], "%s is %s", path, label_name(st.label));
        return;
    }
    static const char *names[] = {"public", "personal", "confidential", "secret"};
    int l = -1;
    for (int i = 0; i < 4; i++)
        if (!strcmp(level, names[i])) l = i;
    if (l < 0) {
        out("labels: public, personal, confidential, secret");
        return;
    }
    if (l < (int)st.label) out_color(C_DIM, "Lowering a label needs your OK: answer the dialog on screen.");
    draw();
    r = sys_set_label(path, l);
    if (r < 0) return err(r);
    out_color(label_colors[l], "%s is now %s", path, label_name((uint32_t)l));
}

static void cmd_df(void) {
    struct fs_info fi;
    int64_t r = sys_fs_info(&fi);
    if (r < 0) return err(r);
    if (fi.kind == FS_NONE) {
        out_color(C_WARN, "storage is locked");
        return;
    }
    uint64_t total = fi.total_blocks * fi.block_size / 1024, avail = fi.free_blocks * fi.block_size / 1024;
    out("Device   %s", fi.device);
    out_color(fi.kind == FS_ENCRYPTED ? C_OK : C_WARN, "Storage  %s",
              fi.kind == FS_ENCRYPTED ? "encrypted (ChaCha20-Poly1305, key from your passphrase)"
                                      : "temporary: in memory, lost at shutdown (attach a blank disk to keep files)");
    out("Space    %lu KiB free of %lu KiB", avail, total);
    out("Files    %u free of %u", fi.free_inodes, fi.total_inodes);
    out_color(label_colors[fi.taint & 3], "This terminal holds %s data", label_name(fi.taint));
}

static void cmd_dlp(void) {
    static char buf[8192];
    int64_t n = sys_dlp_log(buf, sizeof buf - 1);
    if (n < 0) return err(n);
    buf[n] = 0;
    if (!n) {
        out_color(C_DIM, "The DLP audit log is empty.");
        return;
    }
    int count = 0;
    char *start = buf + n;
    while (start > buf && count < 21)
        if (*--start == '\n') count++;
    if (*start == '\n') start++;
    char *line = start;
    for (char *p = start; *p; p++)
        if (*p == '\n') {
            *p = 0;
            out_color(strstr_simple(line, "blocked") ? C_ERR : C_DIM, "%s", line);
            line = p + 1;
        }
}

static void ip_str(uint32_t ip, char *buf, size_t n) {
    uint8_t *b = (uint8_t *)&ip;
    snprintf(buf, n, "%u.%u.%u.%u", b[0], b[1], b[2], b[3]);
}

static void cmd_net(void) {
    struct net_info ni;
    int64_t r = sys_net_info(&ni);
    if (r < 0) {
        err(r);
        return;
    }
    if (!ni.mac[0] && !ni.mac[1] && !ni.mac[2] && !ni.mac[3] && !ni.mac[4] && !ni.mac[5]) {
        out_color(C_DIM, "No supported network card (Ã¦ther drives Intel PRO/1000: e1000, e1000e).");
        return;
    }
    out("card     %02x:%02x:%02x:%02x:%02x:%02x, link %s", ni.mac[0], ni.mac[1], ni.mac[2], ni.mac[3], ni.mac[4],
        ni.mac[5], ni.link ? "up" : "down");
    struct net_config nc;
    if (sys_net_config(NETCFG_GET, 0, &nc) == 0)
        out("mode     %s", nc.mode == NET_MODE_STATIC ? "static (set in Settings)" : "DHCP");
    if (!ni.configured) {
        out_color(C_DIM, "address  waiting for DHCP");
    } else {
        char a[16], m[16], g[16], d[16];
        ip_str(ni.ip, a, sizeof a), ip_str(ni.netmask, m, sizeof m), ip_str(ni.gateway, g, sizeof g),
            ip_str(ni.dns, d, sizeof d);
        out("address  %s / %s", a, m);
        out("router   %s    DNS %s", g, d);
    }
    out("traffic  %lu in, %lu out, %lu dropped;  %u connection%s open", ni.rx_packets, ni.tx_packets, ni.dropped,
        ni.connections, ni.connections == 1 ? "" : "s");
    out_color(C_OK, "inbound  refused: nothing listens, unsolicited packets get no answer");
}

static void cmd_fw(char *arg) {
    char *words[5];
    int n = 0;
    while (*arg && n < 5) {
        while (*arg == ' ') *arg++ = 0;
        if (!*arg) break;
        words[n++] = arg;
        while (*arg && *arg != ' ') arg++;
    }
    if (n == 0) {
        int64_t count = sys_firewall(FW_COUNT, 0, 0);
        if (count < 0) {
            err(count);
            return;
        }
        if (!count) out_color(C_DIM, "No rules of your own yet.");
        for (int i = 0; i < count; i++) {
            struct fw_rule r;
            if (sys_firewall(FW_GET, &r, i) < 0) break;
            char port[8] = "any";
            if (r.port) snprintf(port, sizeof port, "%u", r.port);
            out_color(r.action == FW_ALLOW ? C_OK : C_ERR, "%2d  %-5s  %-12s %-32s port %-5s  %u hit%s", i,
                      r.action == FW_ALLOW ? "allow" : "deny", r.app, r.host, port, r.hits, r.hits == 1 ? "" : "s");
        }
        for (int i = 0;; i++) {
            struct fw_rule r;
            if (sys_firewall(FW_BUILTIN, &r, i) < 0) break;
            out_color(C_DIM, " -  allow  %-12s %-32s port %-5u  %u hit%s  (built in: web browsing)", r.app, r.host,
                      r.port, r.hits, r.hits == 1 ? "" : "s");
        }
        out_color(C_DIM, "Your rules come first, then the built-in ones; anything unmatched asks you.");
        out_color(C_DIM, "Inbound is always refused.");
        return;
    }
    if (!strcmp(words[0], "rm") && n == 2) {
        int i;
        int64_t r = parse_int(words[1], &i) ? sys_firewall(FW_REMOVE, 0, i) : -E_INVAL;
        if (r < 0) err(r);
        else out_color(C_OK, "removed rule %d", i);
        return;
    }
    if ((!strcmp(words[0], "allow") || !strcmp(words[0], "deny")) && (n == 3 || n == 4)) {
        struct fw_rule r;
        memset(&r, 0, sizeof r);
        strlcpy(r.app, words[1], sizeof r.app);
        strlcpy(r.host, words[2], sizeof r.host);
        int port = 0;
        if (n == 4 && (!parse_int(words[3], &port) || port > 65535)) {
            out_color(C_ERR, "port must be 1-65535");
            return;
        }
        r.port = (uint16_t)port;
        r.action = words[0][0] == 'a' ? FW_ALLOW : FW_DENY;
        int64_t res = sys_firewall(FW_ADD, &r, 0);
        if (res < 0) err(res);
        else out_color(C_OK, "rule %d: %s %s -> %s", (int)res, words[0], r.app, r.host);
        return;
    }
    out("usage: fw | fw allow <app> <host> [port] | fw deny <app> <host> [port] | fw rm <n>");
    out_color(C_DIM, "<app> may be * for every app; <host> may be * or *.example.com");
}

/* A plain HTTP/1.0 request, to check the network works end to end. */
static void cmd_get(char *arg) {
    while (*arg == ' ') arg++;
    if (!*arg) {
        out("usage: get <host> [path]");
        return;
    }
    char *path = arg;
    while (*path && *path != ' ') path++;
    if (*path) *path++ = 0;
    while (*path == ' ') path++;
    if (!*path) path = "/";
    out_color(C_DIM, "connecting to %s:80...", arg);
    draw();
    int64_t h = sys_net_connect(arg, 80, 15000);
    if (h < 0) {
        if (h == -E_DENIED) out_color(C_ERR, "blocked by the firewall (or by data loss prevention)");
        else err(h);
        return;
    }
    char req[300];
    int len = snprintf(req, sizeof req, "GET %s HTTP/1.0\r\nHost: %s\r\nUser-Agent: aether-terminal/0.5\r\n\r\n",
                       path, arg);
    for (int off = 0; off < len;) {
        int64_t r = sys_net_send((int)h, req + off, (uint64_t)(len - off));
        if (r < 0) {
            err(r);
            sys_net_close((int)h);
            return;
        }
        off += (int)r;
    }
    static char buf[4096];
    char line[160];
    int ll = 0, shown = 0, in_body = 0;
    uint64_t total = 0;
    for (;;) {
        int64_t r = sys_net_recv((int)h, buf, sizeof buf, 15000);
        if (r == 0) break;
        if (r < 0) {
            err(r);
            break;
        }
        total += (uint64_t)r;
        for (int i = 0; i < r && shown < 12; i++) { /* the status line and headers */
            char c = buf[i];
            if (c == '\n') {
                line[ll] = 0;
                if (!ll) in_body = 1;
                if (!in_body) out_color(shown ? C_DIM : C_OK, "%s", line);
                if (!in_body) shown++;
                ll = 0;
                if (in_body) shown = 12;
            } else if (c != '\r' && ll < (int)sizeof line - 1) {
                line[ll++] = (c >= 32 && c < 127) ? c : '?';
            }
        }
    }
    sys_net_close((int)h);
    out("%lu bytes received", total);
}

static void cmd_apps(void) {
    struct app_info list[32];
    int64_t n = sys_app_list(list, 32);
    if (n < 0) return err(n);
    out_color(C_DIM, "  NAME          VERSION    KIND      STATE       TITLE");
    for (int i = 0; i < n; i++) {
        struct app_info *a = &list[i];
        char kind[16];
        snprintf(kind, sizeof kind, "%s%s", a->system ? "system" : a->installed ? "installed" : "built-in",
                 a->abi == ABI_LINUX ? "+Rust" : "");
        char state[32] = "";
        if (a->running) snprintf(state, sizeof state, "running%s", a->update_pending ? ", update" : "");
        out("  %-13s %-10s %-9s %-11s %s", a->name, a->version[0] ? a->version : "-", kind, state,
            a->title);
    }
    out_color(C_DIM, "%ld app%s. Install and update them in App Store (`store`).", n, n == 1 ? "" : "s");
}

static void cmd_help(void) {
    out_color(C_ACCENT, "Built-in commands");
    out("  help             this list");
    out("  about            what \xC3\xA6ther is");
    out("  sysinfo          CPU, memory and active protections");
    out("  ps               running processes and their capabilities");
    out("  run <app>        start an app (calculator, notes, files, settings, store, terminal)");
    out("  apps             installed apps, versions and pending updates");
    out("  kill <pid>       stop a process");
    out("  caps             this terminal's own capabilities");
    out("  sectest          attack the system and check every attack is blocked");
    out("  test [suite]     run the unit tests: lib memory crypto files dlp sandbox syscall runner net settings attacks");
    out("  log              recent kernel log");
    out_color(C_ACCENT, "Files (encrypted on disk, labelled for data loss prevention)");
    out("  ls [dir], cd [dir], pwd, cat <file>, mkdir <dir>, rm <path>, mv <from> <to>");
    out("  write <file> <text>, append <file> <text>");
    out("  label <file> [level]   show or set: public, personal, confidential, secret");
    out("  df               storage and encryption status;  dlp   the DLP audit log");
    out_color(C_ACCENT, "Network (outgoing only; the firewall decides every connection)");
    out("  net              address, router, DNS and traffic");
    out("  fw               firewall rules;  fw allow|deny <app> <host> [port];  fw rm <n>");
    out("  get <host> [path]   fetch a page over plain HTTP (port 80) to test the connection");
    out("  mem, uptime, date, echo <text>, clear, exit");
    out_color(C_DIM, "Keys: Up/Down history, PgUp/PgDn scroll, Ctrl+Alt+T new terminal, Alt+Tab switch window");
}

static void cmd_about(void) {
    out_color(C_ACCENT, "\xC3\xA6ther 0.6");
    out("A from-scratch operating system written entirely by Claude.");
    out("Security is the foundation, not an add-on:");
    out_color(C_OK, "  * every app is an isolated process with its own address space");
    out_color(C_OK, "  * apps get only the capabilities they need (see `ps` and `caps`)");
    out_color(C_OK, "  * memory is never writable and executable at once (W^X, NX)");
    out_color(C_OK, "  * the kernel can't run or touch app memory by accident (SMEP/SMAP)");
    out_color(C_OK, "  * every pointer an app hands the kernel is checked");
    out_color(C_OK, "  * your files are encrypted and authenticated on disk");
    out_color(C_OK, "  * DLP labels follow your data; lowering one always asks you");
    out_color(C_OK, "  * untrusted apps see only the files you pick for them");
    out_color(C_OK, "  * nothing can connect in; every outgoing connection passes the firewall");
    out("0.6: Calculator, Notes, Files and Probe are Rust apps; Settings manages the network and");
    out("firewall, and App Store installs and updates Craft apps from GitHub without a reboot.");
}

static void cmd_sysinfo(void) {
    struct sys_info si;
    int64_t r = sys_sysinfo(&si);
    if (r < 0) return err(r);
    out("CPU      %s", si.cpu_brand[0] ? si.cpu_brand : si.cpu_vendor);
    out("Memory   %lu MiB free of %lu MiB", si.mem_free_kib / 1024, si.mem_total_kib / 1024);
    out("Screen   %ux%u", si.screen_w, si.screen_h);
    out("Uptime   %lu s, %u processes", si.uptime_ms / 1000, si.nproc);
    out("Protections:");
    struct { uint32_t bit; const char *name, *what; } f[] = {
        {CPU_FEAT_NX, "NX  ", "data can't be executed"},
        {CPU_FEAT_SMEP, "SMEP", "kernel can't execute app memory"},
        {CPU_FEAT_SMAP, "SMAP", "kernel can't read app memory by accident"},
        {CPU_FEAT_UMIP, "UMIP", "apps can't read kernel table addresses"},
        {CPU_FEAT_RDRAND, "RDRAND", "hardware randomness for stack canaries"},
    };
    for (unsigned i = 0; i < sizeof f / sizeof f[0]; i++) {
        int on = (si.cpu_features & f[i].bit) != 0;
        out_color(on ? C_OK : C_WARN, "  [%s] %-6s %s", on ? "on " : "n/a", f[i].name, f[i].what);
    }
}

static void cmd_ps(void) {
    struct proc_info list[32];
    int64_t n = sys_proc_list(list, 32);
    if (n < 0) return err(n);
    static const char *trust[] = {"system", "trusted", "untrusted"};
    out_color(C_DIM, "  PID  STATE    MEMORY    NAME        TRUST      DATA          CAPABILITIES");
    for (int i = 0; i < n; i++) {
        char caps[80];
        const char *st = list[i].state == PROC_READY ? "ready" : list[i].state == PROC_BLOCKED ? "waiting" : "exiting";
        out_color(list[i].trust == TRUST_UNTRUSTED ? C_WARN : C_FG, "  %3u  %-7s  %5lu KiB %-11s %-10s %-13s %s", list[i].pid,
                  st, list[i].mem_kib, list[i].name, list[i].pid == 0 ? "kernel" : trust[list[i].trust % 3],
                  label_name(list[i].taint), list[i].pid == 0 ? "(all)" : caps_str(list[i].caps, caps, sizeof caps));
    }
}

static void cmd_mem(void) {
    struct sys_info si;
    int64_t r = sys_sysinfo(&si);
    if (r < 0) return err(r);
    uint64_t used = si.mem_total_kib - si.mem_free_kib;
    int bar = (int)(used * 40 / (si.mem_total_kib ? si.mem_total_kib : 1));
    char meter[48];
    for (int i = 0; i < 40; i++) meter[i] = i < bar ? '#' : '.';
    meter[40] = 0;
    out("[%s] %lu / %lu MiB used", meter, used / 1024, si.mem_total_kib / 1024);
}

static void cmd_date(int uptime_only) {
    struct time_info t;
    sys_time(&t);
    uint64_t s = t.uptime_ms / 1000;
    if (uptime_only) {
        out("up %lu h %lu min %lu s", s / 3600, (s / 60) % 60, s % 60);
        return;
    }
    out("%04u-%02u-%02u %02u:%02u:%02u (from the hardware clock)", t.year, t.month, t.day, t.hour, t.minute, t.second);
}

static void cmd_log(void) {
    static char buf[4096];
    int64_t n = sys_klog_read(buf, sizeof buf - 1);
    if (n < 0) return err(n);
    buf[n] = 0;
    /* Show the last ~20 lines. */
    int count = 0;
    char *start = buf + n;
    while (start > buf && count < 21)
        if (*--start == '\n') count++;
    if (*start == '\n') start++;
    char *line = start;
    for (char *p = start; *p; p++) {
        if (*p == '\n') {
            *p = 0;
            out_color(C_DIM, "%s", line);
            line = p + 1;
        }
    }
}

static void cmd_run(const char *name) {
    while (*name == ' ') name++;
    if (!*name) {
        out("usage: run <calculator|terminal>");
        return;
    }
    int64_t pid = sys_spawn(name, ~0u, 0);
    if (pid < 0) return err(pid);
    out_color(C_OK, "started %s (pid %ld)", name, pid);
}

static const struct {
    const char *what;
    int expect; /* exit status that means "blocked" */
    const char *how;
} attacks[] = {
    {"read kernel memory", 0x100 + 14, "page fault: kernel pages are supervisor-only"},
    {"overwrite own code", 0x100 + 14, "page fault: code pages are read-only (W^X)"},
    {"execute code on the stack", 0x100 + 14, "page fault: stack is no-execute (NX)"},
    {"run a privileged instruction", 0x100 + 13, "general protection fault: ring 3"},
    {"access hardware I/O ports", 0x100 + 13, "general protection fault: no I/O permission"},
    {"use a capability it lacks", 42, "kernel refused: missing CAP_SYSINFO"},
    {"pass a kernel pointer to a syscall", 42, "kernel refused: pointer validation"},
    {"spawn a more privileged program", 42, "kernel refused: missing CAP_SPAWN"},
    {"open your files by name", 42, "kernel refused: untrusted apps get no paths"},
    {"climb out of its own folder (..)", 42, "kernel refused: '..' is never accepted"},
    {"make writable memory executable", 42, "kernel refused: W^X, no new code"},
    {"take more memory than its quota", 42, "kernel refused: 8 MiB quota"},
};
#define NATTACKS (int)(sizeof attacks / sizeof attacks[0])

/* Runs attack i in the probe: 1 if it was blocked, 0 if not, <0 if the probe couldn't start. */
static int run_attack(int i) {
    int64_t pid = sys_spawn("probe", CAP_STORAGE, (uint64_t)i + 1);
    if (pid < 0) return (int)pid;
    return sys_wait((int)pid, 3000) == attacks[i].expect;
}

static void cmd_sectest(void) {
    out_color(C_ACCENT, "Security self-test: a hostile untrusted program (probe) tries %d attacks.", NATTACKS);
    int passed = 0;
    for (int i = 0; i < NATTACKS; i++) {
        int ok = run_attack(i);
        if (ok < 0) return err(ok);
        passed += ok;
        out_color(ok ? C_OK : C_ERR, "  %s %-34s %s", ok ? "[BLOCKED]" : "[FAILED] ", attacks[i].what,
                  ok ? attacks[i].how : "attack was not stopped as expected");
        draw();
    }
    out_color(passed == NATTACKS ? C_OK : C_ERR, "%d of %d attacks blocked.", passed, NATTACKS);
}

/* `test [suite]`: the kernel's unit tests, then the sandbox attacks from user mode. */
static void cmd_test(const char *suite) {
    static char report[8192];
    while (*suite == ' ') suite++;
    int kernel = strcmp(suite, "attacks") != 0, attacks_too = !*suite || !kernel;
    int runner_too = !*suite || !strcmp(suite, "runner");
    int passed = 0, failed = 0;
    out_color(C_ACCENT, "Running %s unit tests...", *suite ? suite : "all");
    draw();
    if (kernel) {
        int64_t r = sys_selftest(suite, report, sizeof report);
        if (r == -E_NOENT) {
            out_color(C_ERR, "no test suite called \"%s\"", suite);
            out("suites: lib memory crypto files dlp sandbox syscall runner net settings attacks");
            return;
        }
        if (r < 0) return err(r);
        passed += (int)(r >> 16);
        failed += (int)(r & 0xFFFF);
        for (char *line = report; *line;) {
            char *end = line;
            while (*end && *end != '\n') end++;
            int more = *end != 0;
            *end = 0;
            out_color(line[0] == 'o' ? C_OK : C_ERR, "  %s", line);
            line = more ? end + 1 : end;
        }
    }
    if (attacks_too) {
        for (int i = 0; i < NATTACKS; i++) {
            int ok = run_attack(i);
            if (ok < 0) return err(ok);
            ok ? passed++ : failed++;
            out_color(ok ? C_OK : C_ERR, "  %s attacks  %s is blocked", ok ? "ok  " : "FAIL", attacks[i].what);
        }
    }
    if (runner_too) {
        /* An ordinary Rust program built for Linux, if it's installed
         * (tools/build-linuxtest.sh): its own 26 checks, judged by exit code. */
        int64_t pid = sys_spawn("linuxtest", ~0u, 0);
        if (pid == -E_NOENT) {
            out_color(C_DIM, "  skip runner   Rust app on the Linux layer (linuxtest isn't installed)");
        } else {
            int64_t status = pid < 0 ? pid : sys_wait((int)pid, 60000);
            int ok = status == 0;
            ok ? passed++ : failed++;
            out_color(ok ? C_OK : C_ERR, "  %s runner   Rust app on the Linux layer: threads, files, memory, sandbox",
                      ok ? "ok  " : "FAIL");
        }
    }
    char summary[80];
    snprintf(summary, sizeof summary, "test: %d passed, %d failed", passed, failed);
    sys_log(summary); /* for the headless test run (tests/unit.txt) */
    out_color(failed ? C_ERR : C_OK, "%d of %d tests passed.", passed, passed + failed);
}

static void run_command(char *cmd) {
    while (*cmd == ' ') cmd++;
    char *arg = cmd;
    while (*arg && *arg != ' ') arg++;
    if (*arg) *arg++ = 0;

    if (!*cmd) return;
    if (!strcmp(cmd, "help")) cmd_help();
    else if (!strcmp(cmd, "about")) cmd_about();
    else if (!strcmp(cmd, "clear")) nlines = 0;
    else if (!strcmp(cmd, "echo")) out("%s", arg);
    else if (!strcmp(cmd, "sysinfo")) cmd_sysinfo();
    else if (!strcmp(cmd, "ps")) cmd_ps();
    else if (!strcmp(cmd, "apps")) cmd_apps();
    else if (!strcmp(cmd, "mem")) cmd_mem();
    else if (!strcmp(cmd, "uptime")) cmd_date(1);
    else if (!strcmp(cmd, "date")) cmd_date(0);
    else if (!strcmp(cmd, "log")) cmd_log();
    else if (!strcmp(cmd, "run")) cmd_run(arg);
    else if (!strcmp(cmd, "calc") || !strcmp(cmd, "calculator")) cmd_run("calculator");
    else if (!strcmp(cmd, "sectest")) cmd_sectest();
    else if (!strcmp(cmd, "test")) cmd_test(arg);
    else if (!strcmp(cmd, "notes")) cmd_run("notes");
    else if (!strcmp(cmd, "files") || !strcmp(cmd, "explorer")) cmd_run("explorer");
    else if (!strcmp(cmd, "monitor")) cmd_run("monitor");
    else if (!strcmp(cmd, "settings")) cmd_run("settings");
    else if (!strcmp(cmd, "store") || !strcmp(cmd, "installer")) cmd_run("installer");
    else if (!strcmp(cmd, "ls")) cmd_ls(arg);
    else if (!strcmp(cmd, "cd")) cmd_cd(arg);
    else if (!strcmp(cmd, "pwd")) out("%s", cwd);
    else if (!strcmp(cmd, "cat")) cmd_cat(arg);
    else if (!strcmp(cmd, "write")) cmd_write(arg, 0);
    else if (!strcmp(cmd, "append")) cmd_write(arg, 1);
    else if (!strcmp(cmd, "mkdir")) cmd_path_op(arg, do_mkdir, "mkdir <folder>", "created");
    else if (!strcmp(cmd, "rm")) cmd_path_op(arg, do_remove, "rm <file or empty folder>", "removed");
    else if (!strcmp(cmd, "mv")) cmd_mv(arg);
    else if (!strcmp(cmd, "label")) cmd_label(arg);
    else if (!strcmp(cmd, "df")) cmd_df();
    else if (!strcmp(cmd, "dlp")) cmd_dlp();
    else if (!strcmp(cmd, "net")) cmd_net();
    else if (!strcmp(cmd, "fw")) cmd_fw(arg);
    else if (!strcmp(cmd, "get")) cmd_get(arg);
    else if (!strcmp(cmd, "exit")) sys_exit(0);
    else if (!strcmp(cmd, "caps")) {
        char caps[64];
        out("This terminal (pid %d) holds: %s", sys_pid(), caps_str(sys_caps(), caps, sizeof caps));
    } else if (!strcmp(cmd, "kill")) {
        int pid;
        if (!parse_int(arg, &pid)) {
            out("usage: kill <pid>");
            return;
        }
        int64_t r = sys_kill(pid);
        if (r < 0) err(r);
        else out_color(C_OK, "stopped pid %d", pid);
    } else {
        out_color(C_ERR, "unknown command: %s (try `help`)", cmd);
    }
}

/* ---- Line editing -------------------------------------------------------------- */

static void submit(void) {
    struct line *l = &lines[nlines % MAX_LINES];
    strlcpy(l->text, input, sizeof l->text);
    l->color = C_FG;
    l->prompt = 1;
    nlines++;
    if (input_len && (nhist == 0 || strcmp(history[(nhist - 1) % HIST], input))) {
        strlcpy(history[nhist % HIST], input, LINE_LEN);
        nhist++;
    }
    hist_pos = nhist;
    char cmd[LINE_LEN];
    strlcpy(cmd, input, sizeof cmd);
    input_len = cursor = 0;
    input[0] = 0;
    run_command(cmd);
}

static void set_input(const char *s) {
    strlcpy(input, s, sizeof input);
    input_len = cursor = (int)strlen(input);
}

static void key(uint32_t k) {
    int max = (W - 2 * PAD) / gfx_font_w(FONT_SMALL) - 6;
    if (k == '\n') {
        submit();
    } else if (k == '\b') {
        if (cursor > 0) {
            memmove(input + cursor - 1, input + cursor, (size_t)(input_len - cursor + 1));
            cursor--, input_len--;
        }
    } else if (k == KEY_DELETE) {
        if (cursor < input_len) {
            memmove(input + cursor, input + cursor + 1, (size_t)(input_len - cursor));
            input_len--;
        }
    } else if (k == KEY_LEFT) {
        if (cursor > 0) cursor--;
    } else if (k == KEY_RIGHT) {
        if (cursor < input_len) cursor++;
    } else if (k == KEY_HOME) {
        cursor = 0;
    } else if (k == KEY_END) {
        cursor = input_len;
    } else if (k == KEY_UP) {
        if (hist_pos > 0 && hist_pos > nhist - HIST) set_input(history[--hist_pos % HIST]);
    } else if (k == KEY_DOWN) {
        if (hist_pos < nhist - 1) set_input(history[++hist_pos % HIST]);
        else hist_pos = nhist, set_input("");
    } else if (k == KEY_PGUP) {
        int avail = nlines < MAX_LINES ? nlines : MAX_LINES;
        scroll += 10;
        if (scroll > avail - 5) scroll = avail > 5 ? avail - 5 : 0;
    } else if (k == KEY_PGDN) {
        scroll = scroll > 10 ? scroll - 10 : 0;
    } else if (k >= 32 && k < 127 && input_len < max && input_len < LINE_LEN - 1) {
        memmove(input + cursor + 1, input + cursor, (size_t)(input_len - cursor + 1));
        input[cursor++] = (char)k;
        input_len++;
    }
    cursor_on = 1;
}

int main(void) {
    if (window_open(&win, W, H, "Terminal") < 0) return 1;
    out_color(C_ACCENT, "\xC3\xA6ther terminal");
    out_color(C_DIM, "Type `help` for commands, `sectest` to watch the kernel block 12 attacks, `notes` for Notes.");
    out("");
    draw();
    for (;;) {
        struct event ev;
        int64_t got = sys_event_wait(&ev, 530);
        if (got == 0) {
            cursor_on = !cursor_on; /* blink */
        } else if (ev.type == EV_KEY) {
            key(ev.key);
        } else if (ev.type == EV_FOCUS || ev.type == EV_UNFOCUS) {
            focused = ev.type == EV_FOCUS;
        } else if (ev.type == EV_CLOSE) {
            return 0;
        } else {
            continue; /* mouse moves: nothing to redraw */
        }
        draw();
    }
}
