/*
 * proc.h - processes, their threads, and the scheduler.
 *
 * A process owns everything that is shared: its address space, capabilities,
 * sandbox and DLP state, open files, windows and event queue. A thread is
 * what runs: registers, a kernel stack and what it's waiting for. Most
 * processes have exactly one thread; programs built for the Linux system
 * call layer (linux.c) can create more with clone().
 */
#pragma once
#include <stdint.h>
#include "../include/abi.h"

#define MAX_PROCS 32
#define MAX_THREADS 64
#define EVENT_QUEUE_LEN 128
#define USER_STACK_TOP 0x00007FFFFFFFE000ull
#define USER_STACK_PAGES 16
#define USER_WIN_BASE 0x0000600000000000ull
#define USER_WIN_SLOT 0x0000000004000000ull /* 64 MiB of address space per window */

enum { WAIT_NONE, WAIT_EVENT, WAIT_SLEEP, WAIT_CHILD, WAIT_DIALOG, WAIT_FUTEX, WAIT_NET };

#define MAX_FILES 32
#define USER_MMAP_BASE 0x0000100000000000ull
#define USER_MMAP_TOP 0x0000500000000000ull

struct file_handle {
    int used;
    uint32_t ino;
    uint32_t flags; /* O_* rights it was opened with */
    uint32_t type;  /* FT_* */
    uint64_t off;
    int dirty;
};

struct proc;

struct thread {
    uint8_t fxstate[512] __attribute__((aligned(16))); /* SSE/x87 registers while not running */
    struct proc *proc;
    int tid;   /* the first thread's tid is the process's pid */
    int state; /* PROC_READY / PROC_BLOCKED / PROC_ZOMBIE */
    uint64_t kstack_top;
    uint64_t saved_rsp;
    int kstack_slot;
    int idle; /* kernel task only: parked in hlt, safe to preempt */

    int wait_kind;
    int wait_pid;
    uint64_t wake_tick; /* 0 = no timeout */
    uint64_t futex_addr;
    int futex_woken;

    uint64_t fs_base;         /* thread-local storage pointer (Linux ABI) */
    uint64_t clear_child_tid; /* cleared and futex-woken when the thread exits */

    /* The system dialog this thread is waiting on. It is per thread, not per
     * process: a second thread must not be able to consume the answer meant
     * for the first one (network consent, a label lowering, a file choice). */
    int dialog_done, dialog_result;
    uint32_t dialog_ino;
    char dialog_name[NAME_MAX + 2];
};

struct proc {
    int pid, parent;
    int state; /* PROC_READY while alive, PROC_ZOMBIE once it has ended */
    uint32_t caps;
    char name[16];
    uint64_t pml4;
    int nthreads; /* threads not yet reaped */
    int abi;      /* ABI_* */

    struct event events[EVENT_QUEUE_LEN];
    unsigned ev_head, ev_tail;
    int wake_queued; /* an EV_WAKE is in the queue */

    int nwindows;
    int focus_on_first_window; /* user asked for this app: its first window may take focus */
    int update_pending; /* an app update was installed while this copy ran */

    /* Sandbox and data loss prevention. */
    uint32_t trust;          /* TRUST_* */
    uint32_t taint;          /* LABEL_*: highest label of anything read */
    int dlp_warned;
    uint64_t syscall_allow;  /* bit n: may make system call n */
    uint64_t mem_quota_pages, mem_pages; /* SYS_MEM_MAP / mmap / brk memory */
    uint64_t pt_pages; /* page tables made for its reservations: they count against the quota too */
    uint64_t mmap_next;
    uint64_t reserved_pages; /* address space promised but not yet touched */
    uint64_t brk_start, brk_end;
    uint32_t storage_ino;    /* CAP_STORAGE: its /apps/<name> folder, 0 until first use */
    struct file_handle files[MAX_FILES];

    /* Console output (Linux stdout/stderr) goes to the system log, rate-limited. */
    uint64_t log_window;
    int log_lines;
    char log_buf[160];
    int log_len;

    /* Cancelled system dialogs, by type (label, picker, network, confirm).
     * Three refusals stop an app from nagging; keeping them apart means a
     * refused network prompt doesn't take away the file picker. */
    int dialog_denials[4];
};

extern struct thread *cur_thread;
#define current (cur_thread->proc)

void proc_init(void); /* turns the boot thread into pid 0, the kernel task */
int proc_spawn(const char *name, uint32_t caps, uint64_t arg, int parent); /* pid or -E_* */
struct proc *proc_find(int pid);
void proc_kill(struct proc *p, int status, const char *reason);
__attribute__((noreturn)) void proc_kill_current(int status, const char *reason);
void proc_exit_current(int code);

/* Threads (Linux clone): a new thread in the current process that starts
 * with the caller's registers, except rax = 0, rsp = stack. tid or -E_*. */
struct isr_frame;
int thread_create(const struct isr_frame *f, uint64_t stack, uint64_t tls, int set_tls, uint64_t clear_tid);
__attribute__((noreturn)) void thread_exit_current(int code);
int futex_wake(struct proc *p, uint64_t addr, int n);
int futex_requeue(struct proc *p, uint64_t from, uint64_t to, int wake, int requeue);

void schedule(void);
void proc_tick(void);
void proc_maybe_preempt(void);
int proc_others_ready(void);
void proc_block(int wait_kind, int64_t timeout_ms); /* returns when woken or timed out */
void thread_wake(struct thread *t);
void proc_wake_kind(struct proc *p, int wait_kind); /* wake its threads waiting for this */
void proc_post_event(struct proc *p, const struct event *ev);
int proc_exit_status(int pid, int parent); /* EXIT_STILL_RUNNING if alive, -1 if unknown/not its child */
void proc_reap(void);          /* free dead threads and processes; kernel task only */
int proc_list(struct proc_info *out, int max);
int proc_count(void);
int proc_file_in_use(uint32_t ino); /* is any process holding this inode open? */
int proc_named_running(const char *name); /* is a live process of this app running? */
int proc_update_pending(const char *name); /* is a running copy older than its package? */
void proc_mark_update_pending(const char *name); /* its running copy is now out of date */

/* syscall.c */
void syscall_init(void);
