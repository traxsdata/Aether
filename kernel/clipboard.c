/*
 * clipboard.c - one label-aware text buffer shared by apps.
 *
 * DLP rules apply at both ends: a Confidential process cannot put data on the
 * clipboard with a Public label, and a process that reads a Confidential entry
 * becomes Confidential itself. Exfiltration is still checked by the existing
 * log/network gates.
 */
#include "../include/abi.h"
#include "clipboard.h"
#include "dlp.h"
#include "kernel.h"
#include "proc.h"

static uint8_t text[CLIP_MAX];
static size_t text_len;
static int text_label;
static uint64_t generation;

int clipboard_store(struct proc *p, const void *data, size_t len, int label) {
    if (!p || (!data && len)) return -E_INVAL;
    if (len > CLIP_MAX) return -E_INVAL;
    if (label < 0 || label >= LABEL_COUNT) return -E_INVAL;
    int effective = MAX(label, (int)p->taint);
    if (len) memcpy(text, data, len);
    text_len = len;
    text_label = effective;
    generation++;
    dlp_taint(p, effective, "clipboard copy");
    dlp_audit("%s copied %lu bytes to the clipboard (%s)", p->name, (unsigned long)len, dlp_label_name(effective));
    return 0;
}

int clipboard_read(struct proc *p, void *out, size_t max, size_t *len, int *label) {
    if (!p || (!out && max)) return -E_INVAL;
    size_t n = MIN(text_len, max);
    if (n) memcpy(out, text, n);
    if (len) *len = n;
    if (label) *label = text_len ? text_label : LABEL_PUBLIC;
    if (text_len) {
        dlp_taint(p, text_label, "clipboard paste");
        dlp_audit("%s pasted %lu clipboard bytes (%s)", p->name, (unsigned long)n, dlp_label_name(text_label));
    }
    return 0;
}

int clipboard_info(size_t *len, int *label) {
    if (len) *len = text_len;
    if (label) *label = text_len ? text_label : LABEL_PUBLIC;
    return 0;
}

void clipboard_clear(void) {
    memset(text, 0, sizeof text); /* don't leave a copy in free kernel memory */
    text_len = 0;
    text_label = LABEL_PUBLIC;
    generation++;
}
