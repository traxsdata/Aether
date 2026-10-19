/*
 * dlp.h - data loss prevention: labels on files and processes.
 *
 * The rules (docs/DLP.md has the full story):
 *   1. Every file has a label; every process carries a taint, the highest
 *      label of anything it has read. Taint only goes up while it runs.
 *   2. Writing never lowers a file's label: a process tainted Confidential
 *      that writes to a Personal file makes it Confidential. Labels follow
 *      the data, with no prompts.
 *   3. Lowering a label always asks the user, in a kernel-drawn dialog an
 *      app can't fake or click.
 *   4. Data leaving the protected store (the kernel log, the network;
 *      USB and printing later) is checked against the sender's taint.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

struct proc;

enum { DLP_EGRESS_LOG = 1, DLP_EGRESS_NET = 2 };

const char *dlp_label_name(int label);
uint32_t dlp_label_color(int label);
void dlp_taint(struct proc *p, int label, const char *what);
int dlp_egress(struct proc *p, int channel); /* 0 if allowed, -E_DENIED */
void dlp_audit(const char *fmt, ...);
size_t dlp_log_read(char *dst, size_t len);
