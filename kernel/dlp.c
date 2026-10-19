/* dlp.c - labels, taint tracking, egress checks and the audit log. */
#include "dlp.h"
#include "../include/abi.h"
#include "kernel.h"
#include "proc.h"
#include "rtc.h"
#include "wm.h"

static const char *names[LABEL_COUNT] = {"Public", "Personal", "Confidential", "Secret"};
static const uint32_t colors[LABEL_COUNT] = {0x8C96B8, 0x5CF2B0, 0xFFB547, 0xFF5C7A};

const char *dlp_label_name(int label) { return label >= 0 && label < LABEL_COUNT ? names[label] : "?"; }
uint32_t dlp_label_color(int label) { return label >= 0 && label < LABEL_COUNT ? colors[label] : 0xFFFFFF; }

static char ring[8192];
static size_t ring_len;

void dlp_audit(const char *fmt, ...) {
    char line[202];
    struct rtc_time t;
    rtc_now(&t);
    int n = snprintf(line, sizeof line, "%02u:%02u:%02u ", t.hour, t.minute, t.second);
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(line + n, sizeof line - (size_t)n - 2, fmt, ap);
    va_end(ap);
    size_t len = strlen(line);
    line[len++] = '\n';
    line[len] = 0;
    if (ring_len + len > sizeof ring) { /* drop the oldest lines */
        size_t drop = ring_len + len - sizeof ring;
        while (drop < ring_len && ring[drop - 1] != '\n') drop++;
        memmove(ring, ring + drop, ring_len - drop);
        ring_len -= drop;
    }
    memcpy(ring + ring_len, line, len);
    ring_len += len;
    kprintf("[dlp] %s", line + n);
}

size_t dlp_log_read(char *dst, size_t len) {
    size_t start = ring_len > len ? ring_len - len : 0;
    while (start && start < ring_len && ring[start - 1] != '\n') start++;
    memcpy(dst, ring + start, ring_len - start);
    return ring_len - start;
}

void dlp_taint(struct proc *p, int label, const char *what) {
    if (label <= (int)p->taint) return;
    dlp_audit("%s (pid %d) now holds %s data (read %s)", p->name, p->pid, names[label], what);
    p->taint = (uint32_t)label;
    wm_proc_changed(p);
}

int dlp_egress(struct proc *p, int channel) {
    if ((channel == DLP_EGRESS_LOG || channel == DLP_EGRESS_NET) && p->taint >= LABEL_CONFIDENTIAL) {
        if (!p->dlp_warned) {
            dlp_audit("blocked: %s tried to send %s data to %s", p->name, names[p->taint],
                      channel == DLP_EGRESS_NET ? "the network" : "the system log");
            p->dlp_warned = 1;
        }
        return -E_DENIED;
    }
    return 0;
}
