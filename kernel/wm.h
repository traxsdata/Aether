/* wm.h - the window manager and compositor (runs in the kernel task, pid 0). */
#pragma once
#include "../include/abi.h"
#include "../include/bootinfo.h"

struct proc;

void wm_init(const struct framebuffer *fb);
void wm_run(void) __attribute__((noreturn)); /* the kernel task's main loop */

/* Called from system calls (current process context, IF=0). */
int wm_create_window(struct proc *p, uint32_t w, uint32_t h, const char *title, struct win_info *out);
int wm_present(struct proc *p, int handle);
int wm_set_title(struct proc *p, int handle, const char *title);
int wm_win_flags(struct proc *p, int handle, uint32_t flags, uint32_t min_w, uint32_t min_h);
int wm_win_resize(struct proc *p, int handle, struct win_info *out);
void wm_display_info(struct display_info *out);
void wm_proc_exited(struct proc *p);
void wm_notify(const char *title, const char *msg);

/* System dialogs, drawn and answered entirely by the kernel. The calling
 * thread starts one, then blocks (WAIT_DIALOG) until its own dialog_done is
 * set; the answer is in that thread's dialog_result / dialog_ino /
 * dialog_name. A sibling thread can never consume the answer. */
int wm_dialog_label(struct proc *p, const char *file, int from, int to); /* 0, or -E_LIMIT if busy */
int wm_dialog_pick(struct proc *p, int mode, const char *suggested);
int wm_dialog_net(struct proc *p, const char *host, int port); /* 0 block, 1 allow once, 2 always allow */
/* A kernel-drawn yes/no gate for destructive operations. Returns 1 only when
 * the user deliberately confirms. */
int wm_dialog_confirm(struct proc *p, const char *title, const char *detail);
void wm_proc_changed(struct proc *p); /* its trust/taint badge needs redrawing */

/* Encrypted storage (storage.c) decides whether the desktop starts with the unlock screen. */
enum { LOCK_NONE, LOCK_SETUP, LOCK_UNLOCK, LOCK_SESSION };
void wm_set_lock(int mode, void (*unlocked)(void));
int wm_lock_session(void); /* 0, or -E_NOTSUP when storage is temporary */
