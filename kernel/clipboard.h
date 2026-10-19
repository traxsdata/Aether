/* clipboard.h - the label-aware system clipboard (1.7).
 *
 * The clipboard is kernel memory, not a shared page: apps can only reach it
 * through SYS_CLIP_* with CAP_CLIPBOARD. Reading raises the reader's DLP taint
 * to the clipboard's label; writing never lowers the label below the writer's
 * current taint. The kernel clears it when the session locks, so a secret
 * copied before lunch is not waiting after an unlock.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

struct proc;

/* Store text. `label` is raised to at least the writer's taint. Returns 0,
 * -E_INVAL for an oversized/unknown label, or -E_NOMEM. */
int clipboard_store(struct proc *p, const void *data, size_t len, int label);
/* Copy up to `max` bytes out and report the label. Raises the reader's taint. */
int clipboard_read(struct proc *p, void *out, size_t max, size_t *len, int *label);
/* Length and label without consuming it. Still requires CAP_CLIPBOARD. */
int clipboard_info(size_t *len, int *label);
void clipboard_clear(void);
