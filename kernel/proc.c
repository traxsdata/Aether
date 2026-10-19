/*
 * proc.c - processes, threads and a round-robin scheduler.
 *
 * Each process has its own address space (lower half) and a capability
 * mask; each of its threads has its own kernel stack with an unmapped guard
 * page below it. The kernel itself is pid 0 (the window manager loop runs
 * there).
 *
 * The kernel is not preemptible: interrupts are off whenever kernel code
 * runs, except while pid 0 is parked in `hlt`. Threads are preempted when
 * a timer or input interrupt arrives while they are in user mode.
 */
#include "proc.h"
#include "apps.h"
#include "cpu.h"
#include "elf.h"
#include "gdt.h"
#include "idt.h"
#include "input.h"
#include "kernel.h"
#include "linux.h"
#include "mm.h"
#include "net.h"
#include "afs.h"
#include "uaccess.h"
#include "wm.h"

#define KSTACK_PAGES 4
#define KSTACK_AREA 0xFFFFFF0000000000ull /* PML4 slot 510, created in proc_init */
#define TIME_SLICE_MS 10
#define MSR_FS_BASE 0xC0000100

struct thread *cur_thread;
static struct thread *threads[MAX_THREADS];
static struct proc *procs[MAX_PROCS];
static struct proc kernel_proc;
static struct thread kernel_thread;
static int next_pid = 1;
static unsigned slice;
static int need_resched;
static uint8_t fx_template[512] __attribute__((aligned(16)));
static uint64_t kstack_slots_used; /* bitmap, one slot per thread */

extern uint64_t kernel_stack_current;
extern void switch_to(uint64_t *save_rsp, uint64_t new_rsp);
extern char isr_return[];

/* Recently exited processes, so SYS_WAIT can report how they ended. */
static struct {
    int pid, parent, status, used;
} exited[32];
static unsigned exited_next;

void proc_init(void) {
    /* Enable SSE for user programs (the kernel itself never uses it). */
    write_cr0((read_cr0() & ~CR0_EM) | CR0_MP);
    write_cr4(read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT);
    __asm__ volatile("fninit; fxsave64 %0" : "=m"(fx_template));
    *(uint32_t *)&fx_template[24] = 0x1F80; /* MXCSR: all exceptions masked */

    /* Make sure the kernel-stack area's top-level entry exists before any
     * address space copies the kernel half. */
    if (vmm_map(vmm_kernel_pml4(), KSTACK_AREA, 0, 0)) panic("proc: no memory for kernel stacks");
    vmm_unmap(vmm_kernel_pml4(), KSTACK_AREA);

    kernel_proc.pid = 0;
    kernel_proc.state = PROC_READY;
    kernel_proc.caps = ~0u;
    kernel_proc.syscall_allow = ~0ull;
    kernel_proc.pml4 = vmm_kernel_pml4();
    kernel_proc.nthreads = 1;
    strlcpy(kernel_proc.name, "kernel", sizeof kernel_proc.name);
    kernel_thread.proc = &kernel_proc;
    kernel_thread.state = PROC_READY;
    procs[0] = &kernel_proc;
    threads[0] = &kernel_thread;
    cur_thread = &kernel_thread;
}

static int kstack_alloc(struct thread *t) {
    for (int slot = 0; slot < 64; slot++) {
        if (kstack_slots_used & (1ull << slot)) continue;
        /* Slot layout: [unmapped guard page][KSTACK_PAGES pages]. */
        uint64_t base = KSTACK_AREA + (uint64_t)slot * (KSTACK_PAGES + 1) * PAGE_SIZE;
        for (int i = 0; i < KSTACK_PAGES; i++) {
            uint64_t page = pmm_alloc();
            if (!page) {
                while (i--) {
                    uint64_t va = base + (uint64_t)(i + 1) * PAGE_SIZE;
                    pmm_free(vmm_translate(vmm_kernel_pml4(), va, 0) & PTE_ADDR);
                    vmm_unmap(vmm_kernel_pml4(), va);
                }
                return -1;
            }
            vmm_map(vmm_kernel_pml4(), base + (uint64_t)(i + 1) * PAGE_SIZE, page, PTE_RW | PTE_NX | PTE_G);
        }
        kstack_slots_used |= 1ull << slot;
        t->kstack_slot = slot;
        t->kstack_top = base + (KSTACK_PAGES + 1) * PAGE_SIZE;
        return 0;
    }
    return -1;
}

static void kstack_free(struct thread *t) {
    uint64_t base = KSTACK_AREA + (uint64_t)t->kstack_slot * (KSTACK_PAGES + 1) * PAGE_SIZE;
    for (int i = 0; i < KSTACK_PAGES; i++) {
        uint64_t va = base + (uint64_t)(i + 1) * PAGE_SIZE;
        uint64_t phys = vmm_translate(vmm_kernel_pml4(), va, 0);
        if (phys) pmm_free(phys & PTE_ADDR);
        vmm_unmap(vmm_kernel_pml4(), va);
    }
    kstack_slots_used &= ~(1ull << t->kstack_slot);
    t->kstack_top = 0;
}

static int free_thread_slot(void) {
    for (int i = 1; i < MAX_THREADS; i++)
        if (!threads[i]) return i;
    return -1;
}

/* A new thread's kernel stack: the first switch_to "returns" through
 * isr_return straight into user mode with the registers in *f. */
static void thread_prepare(struct thread *t, const struct isr_frame *frame) {
    struct isr_frame *f = (struct isr_frame *)(t->kstack_top - sizeof *f);
    *f = *frame;
    uint64_t *sp = (uint64_t *)f;
    *--sp = (uint64_t)isr_return;
    for (int i = 0; i < 6; i++) *--sp = 0; /* rbp rbx r12 r13 r14 r15 */
    t->saved_rsp = (uint64_t)sp;
}

/* ---- Process lifecycle ------------------------------------------------- */

/* System calls an untrusted app may make at all; everything else is refused
 * before the call even looks at its capabilities (defence in depth). */
#define BIT(n) (1ull << (n))
static const uint64_t untrusted_syscalls =
    BIT(SYS_EXIT) | BIT(SYS_WIN_CREATE) | BIT(SYS_WIN_PRESENT) | BIT(SYS_EVENT_WAIT) | BIT(SYS_TIME) |
    BIT(SYS_YIELD) | BIT(SYS_WIN_TITLE) | BIT(SYS_SELF) | BIT(SYS_OPEN) | BIT(SYS_CLOSE) | BIT(SYS_READ) |
    BIT(SYS_WRITE) | BIT(SYS_SEEK) | BIT(SYS_STAT) | BIT(SYS_READDIR) | BIT(SYS_MKDIR) | BIT(SYS_REMOVE) |
    BIT(SYS_RENAME) | BIT(SYS_SET_LABEL) | BIT(SYS_PICK_FILE) | BIT(SYS_FS_INFO) | BIT(SYS_MEM_MAP) |
    BIT(SYS_MEM_UNMAP) | BIT(SYS_MEM_PROTECT) | BIT(SYS_DISPLAY_INFO) | BIT(SYS_EVENT_WAKE) | BIT(SYS_NET_CONNECT) |
    BIT(SYS_NET_SEND) | BIT(SYS_NET_RECV) | BIT(SYS_NET_CLOSE) | BIT(SYS_NET_INFO) | /* the network still needs CAP_NET */
    BIT(SYS_WIN_FLAGS) | BIT(SYS_WIN_RESIZE) | BIT(SYS_TRUNCATE) | BIT(SYS_NOTIFY) | BIT(SYS_CLIPBOARD);

int proc_spawn(const char *name, uint32_t caps, uint64_t arg, int parent) {
    const struct program *prog = program_find(name);
    if (!prog) return -E_NOENT;
    if (!program_verify(prog)) return -E_IO; /* damaged package */
    struct proc *par = proc_find(parent);
    int slot = -1, tslot = free_thread_slot();
    for (int i = 1; i < MAX_PROCS; i++)
        if (!procs[i]) {
            slot = i;
            break;
        }
    if (slot < 0 || tslot < 0) return -E_LIMIT;

    struct proc *p = kmalloc(sizeof *p);
    struct thread *t = kmalloc(sizeof *t);
    if (!p || !t) {
        kfree(p);
        kfree(t);
        return -E_NOMEM;
    }
    t->proc = p;
    p->pml4 = vmm_new_address_space();
    if (!p->pml4 || kstack_alloc(t)) goto fail;

    strlcpy(p->name, prog->name, sizeof p->name);
    p->abi = prog->abi;
    /* Trust never rises from parent to child, and data labels carry over. */
    p->trust = MAX(prog->trust, par && par->pid ? par->trust : TRUST_SYSTEM);
    p->taint = par && par->pid ? par->taint : LABEL_PUBLIC;
    p->syscall_allow = p->trust == TRUST_UNTRUSTED ? untrusted_syscalls : ~0ull;
    p->mem_quota_pages = (uint64_t)(prog->mem_quota_mib ? prog->mem_quota_mib : 64) * 256;
    p->mmap_next = USER_MMAP_BASE + ((random_u64() & 0xFFFFF) << 12); /* randomized heap base */

    struct elf_info ei;
    int err = elf_load(p->pml4, prog->image, prog->size, p->abi == ABI_LINUX, &ei);
    if (err) {
        kprintf("[proc] %s: refusing to load: invalid or unsafe ELF image\n", name);
        goto fail;
    }

    struct isr_frame f;
    memset(&f, 0, sizeof f);
    f.rip = ei.entry;
    f.cs = SEL_UCODE;
    f.rflags = 0x202; /* IF set, everything else clear (IOPL 0) */
    f.ss = SEL_UDATA;
    if (p->abi == ABI_LINUX) {
        if (linux_setup(p, prog, &ei, arg, &f.rsp)) {
            kprintf("[proc] %s: not enough memory to start\n", name);
            goto fail;
        }
    } else {
        for (int i = 0; i < USER_STACK_PAGES; i++) {
            uint64_t page = pmm_alloc();
            if (!page ||
                vmm_map(p->pml4, USER_STACK_TOP - (uint64_t)(i + 1) * PAGE_SIZE, page, PTE_US | PTE_RW | PTE_NX)) {
                if (page) pmm_free(page);
                goto fail;
            }
        }
        f.rsp = USER_STACK_TOP;
        f.rdi = arg;
    }

    p->pid = next_pid++;
    p->parent = parent;
    p->caps = caps & prog->caps; /* a caller can only ask for what the manifest allows */
    p->update_pending = 0;
    /* Starting an app is a user action (the dock, the menu, App Store, or a
     * trusted app), so its first window may take focus. Later windows may
     * not: a background app must never steal the keyboard. */
    p->focus_on_first_window = 1;
    p->state = PROC_READY;
    p->nthreads = 1;
    t->tid = p->pid;
    memcpy(t->fxstate, fx_template, sizeof fx_template);
    thread_prepare(t, &f);
    t->state = PROC_READY;
    procs[slot] = p;
    threads[tslot] = t;
    static const char *trust_names[] = {"system", "trusted", "untrusted"};
    kprintf("[proc] started %s (pid %d, caps %x, %s%s)\n", p->name, p->pid, p->caps, trust_names[p->trust],
            p->abi == ABI_LINUX ? ", Linux ABI" : "");
    return p->pid;

fail:
    if (t->kstack_top) kstack_free(t);
    if (p->pml4) vmm_destroy_address_space(p->pml4);
    kfree(p);
    kfree(t);
    return -E_NOMEM;
}

int thread_create(const struct isr_frame *frame, uint64_t stack, uint64_t tls, int set_tls, uint64_t clear_tid) {
    struct proc *p = current;
    /* A thread starts by returning through iretq with this stack. It must be
     * canonical user memory: a non-canonical value faults in the kernel. */
    if (!stack || stack >= USER_TOP) return -E_INVAL;
    int tslot = free_thread_slot();
    if (tslot < 0 || p->nthreads >= 32) return -E_LIMIT;
    struct thread *t = kmalloc(sizeof *t);
    if (!t) return -E_NOMEM;
    if (kstack_alloc(t)) {
        kfree(t);
        return -E_NOMEM;
    }
    t->proc = p;
    t->tid = next_pid++;
    t->fs_base = set_tls ? tls : cur_thread->fs_base;
    t->clear_child_tid = clear_tid;
    /* The new thread continues from the same system call with the caller's
     * registers, and sees 0 as its result. */
    __asm__ volatile("fxsave64 %0" : "=m"(t->fxstate));
    struct isr_frame f = *frame;
    f.rax = 0;
    f.rsp = stack;
    thread_prepare(t, &f);
    t->state = PROC_READY;
    p->nthreads++;
    threads[tslot] = t;
    return t->tid;
}

struct proc *proc_find(int pid) {
    for (int i = 0; i < MAX_PROCS; i++)
        if (procs[i] && procs[i]->pid == pid && procs[i]->state != PROC_ZOMBIE) return procs[i];
    return 0;
}

int proc_exit_status(int pid, int parent) {
    struct proc *p = proc_find(pid);
    if (p) return p->parent == parent ? EXIT_STILL_RUNNING : -1;
    for (unsigned i = 0; i < ARRAY_LEN(exited); i++)
        if (exited[i].used && exited[i].pid == pid && exited[i].parent == parent) {
            exited[i].used = 0; /* one waiter consumes the status */
            return exited[i].status;
        }
    return -1;
}

void proc_kill(struct proc *p, int status, const char *reason) {
    if (!p || p->pid == 0 || p->state == PROC_ZOMBIE) return;
    if (reason) {
        kprintf("[proc] %s (pid %d) %s\n", p->name, p->pid, reason);
        wm_notify(p->name, reason);
    }
    wm_proc_exited(p);
    net_proc_exit(p);
    for (int i = 0; i < MAX_FILES; i++) p->files[i].used = 0;
    afs_sync();
    p->state = PROC_ZOMBIE;
    /* None of its threads runs again. They are parked at a scheduling point
     * in the kernel (or were preempted in user mode), holding nothing. */
    for (int i = 1; i < MAX_THREADS; i++)
        if (threads[i] && threads[i]->proc == p) threads[i]->state = PROC_ZOMBIE;
    unsigned slot = exited_next++ % ARRAY_LEN(exited);
    exited[slot].pid = p->pid;
    exited[slot].parent = p->parent;
    exited[slot].status = status;
    exited[slot].used = 1;
    for (int i = 0; i < MAX_THREADS; i++)
        if (threads[i] && threads[i]->wait_kind == WAIT_CHILD && threads[i]->wait_pid == p->pid) thread_wake(threads[i]);
}

__attribute__((noreturn)) void proc_kill_current(int status, const char *reason) {
    proc_kill(current, status, reason);
    schedule();
    panic("dead process was scheduled");
}

void proc_exit_current(int code) {
    kprintf("[proc] %s (pid %d) exited with code %d\n", current->name, current->pid, code);
    proc_kill_current(code & 0xFF, 0);
}

__attribute__((noreturn)) void thread_exit_current(int code) {
    struct thread *t = cur_thread;
    int alive = 0;
    for (int i = 1; i < MAX_THREADS; i++)
        if (threads[i] && threads[i] != t && threads[i]->proc == t->proc && threads[i]->state != PROC_ZOMBIE) alive++;
    if (!alive) {
        proc_exit_current(code); /* the last thread out ends the process */
        __builtin_unreachable();
    }
    /* Linux: tell whoever joins this thread that it's gone. */
    if (t->clear_child_tid) {
        uint32_t zero = 0;
        if (!copy_out(t->clear_child_tid, &zero, sizeof zero)) futex_wake(t->proc, t->clear_child_tid, 1);
    }
    t->state = PROC_ZOMBIE;
    schedule();
    panic("dead thread was scheduled");
}

void proc_reap(void) {
    for (int i = 1; i < MAX_THREADS; i++) {
        struct thread *t = threads[i];
        if (!t || t->state != PROC_ZOMBIE || t == cur_thread) continue;
        kstack_free(t);
        t->proc->nthreads--;
        threads[i] = 0;
        kfree(t);
    }
    for (int i = 1; i < MAX_PROCS; i++) {
        struct proc *p = procs[i];
        if (!p || p->state != PROC_ZOMBIE || p->nthreads > 0) continue;
        vmm_destroy_address_space(p->pml4);
        procs[i] = 0;
        kfree(p);
    }
}

/* ---- Futexes (Linux threads): wait queues keyed by an address ---------- */

int futex_wake(struct proc *p, uint64_t addr, int n) {
    int woken = 0;
    for (int i = 1; i < MAX_THREADS && woken < n; i++) {
        struct thread *t = threads[i];
        if (t && t->proc == p && t->state == PROC_BLOCKED && t->wait_kind == WAIT_FUTEX && t->futex_addr == addr) {
            t->futex_woken = 1;
            thread_wake(t);
            woken++;
        }
    }
    return woken;
}

int futex_requeue(struct proc *p, uint64_t from, uint64_t to, int wake, int requeue) {
    int woken = futex_wake(p, from, wake), moved = 0;
    for (int i = 1; i < MAX_THREADS && moved < requeue; i++) {
        struct thread *t = threads[i];
        if (t && t->proc == p && t->state == PROC_BLOCKED && t->wait_kind == WAIT_FUTEX && t->futex_addr == from) {
            t->futex_addr = to;
            moved++;
        }
    }
    return woken + moved;
}

/* ---- Scheduling -------------------------------------------------------- */

static void switch_to_thread(struct thread *next) {
    struct thread *prev = cur_thread;
    if (next == prev) return;
    __asm__ volatile("fxsave64 %0" : "=m"(prev->fxstate));
    cur_thread = next;
    if (next->proc->pid != 0) {
        tss_set_kernel_stack(next->kstack_top);
        kernel_stack_current = next->kstack_top;
        wrmsr(MSR_FS_BASE, next->fs_base);
    }
    if (read_cr3() != next->proc->pml4) write_cr3(next->proc->pml4);
    __asm__ volatile("fxrstor64 %0" : : "m"(next->fxstate));
    slice = 0;
    need_resched = 0;
    switch_to(&prev->saved_rsp, next->saved_rsp);
}

void schedule(void) {
    int cur_idx = 0;
    for (int i = 0; i < MAX_THREADS; i++)
        if (threads[i] == cur_thread) cur_idx = i;
    /* Input waiting: the window manager (pid 0) goes first. */
    if (input_pending && cur_thread != &kernel_thread) {
        switch_to_thread(&kernel_thread);
        return;
    }
    for (int k = 1; k <= MAX_THREADS; k++) {
        struct thread *t = threads[(cur_idx + k) % MAX_THREADS];
        if (t && t->state == PROC_READY) {
            if (t == cur_thread) return;
            switch_to_thread(t);
            return;
        }
    }
    if (cur_thread->state != PROC_READY) switch_to_thread(&kernel_thread);
}

int proc_others_ready(void) {
    for (int i = 1; i < MAX_THREADS; i++)
        if (threads[i] && threads[i]->state == PROC_READY) return 1;
    return 0;
}

void proc_tick(void) {
    for (int i = 1; i < MAX_THREADS; i++) {
        struct thread *t = threads[i];
        if (t && t->state == PROC_BLOCKED && t->wake_tick && ticks >= t->wake_tick) thread_wake(t);
    }
    if (++slice >= TIME_SLICE_MS) need_resched = 1;
}

void proc_maybe_preempt(void) {
    if (need_resched || (input_pending && cur_thread != &kernel_thread) || (cur_thread->idle && proc_others_ready()))
        schedule();
}

void proc_block(int wait_kind, int64_t timeout_ms) {
    cur_thread->state = PROC_BLOCKED;
    cur_thread->wait_kind = wait_kind;
    cur_thread->wake_tick = timeout_ms < 0 ? 0 : ticks + (uint64_t)timeout_ms + 1;
    schedule();
}

void thread_wake(struct thread *t) {
    if (t->state != PROC_BLOCKED) return;
    t->state = PROC_READY;
    t->wait_kind = WAIT_NONE;
    t->wake_tick = 0;
}

void proc_wake_kind(struct proc *p, int wait_kind) {
    for (int i = 1; i < MAX_THREADS; i++)
        if (threads[i] && threads[i]->proc == p && threads[i]->wait_kind == wait_kind) thread_wake(threads[i]);
}

void proc_post_event(struct proc *p, const struct event *ev) {
    if (!p || p->state == PROC_ZOMBIE) return;
    unsigned n = p->ev_head - p->ev_tail;
    /* Coalesce consecutive mouse moves so a busy app doesn't fall behind. */
    if (ev->type == EV_MOUSE_MOVE && n > 0) {
        struct event *last = &p->events[(p->ev_head - 1) % EVENT_QUEUE_LEN];
        if (last->type == EV_MOUSE_MOVE && last->window == ev->window) {
            *last = *ev;
            return;
        }
    }
    if (n >= EVENT_QUEUE_LEN) return; /* full: drop */
    p->events[p->ev_head % EVENT_QUEUE_LEN] = *ev;
    p->ev_head++;
    proc_wake_kind(p, WAIT_EVENT);
}

int proc_count(void) {
    int n = 0;
    for (int i = 0; i < MAX_PROCS; i++)
        if (procs[i] && procs[i]->state != PROC_ZOMBIE) n++;
    return n;
}

int proc_list(struct proc_info *out, int max) {
    int n = 0;
    for (int i = 0; i < MAX_PROCS && n < max; i++) {
        struct proc *p = procs[i];
        if (!p || p->state == PROC_ZOMBIE) continue;
        /* A process is running if any of its threads can run. */
        uint32_t state = PROC_BLOCKED;
        for (int t = 0; t < MAX_THREADS; t++)
            if (threads[t] && threads[t]->proc == p && threads[t]->state == PROC_READY) state = PROC_READY;
        out[n].pid = (uint32_t)p->pid;
        out[n].parent = (uint32_t)p->parent;
        out[n].state = state;
        out[n].caps = p->caps;
        out[n].mem_kib = p->pid ? vmm_user_pages(p->pml4) * 4 : 0;
        strlcpy(out[n].name, p->name, sizeof out[n].name);
        out[n].trust = p->trust;
        out[n].taint = p->taint;
        n++;
    }
    return n;
}

int proc_named_running(const char *name) {
    for (int i = 1; i < MAX_PROCS; i++) {
        struct proc *p = procs[i];
        if (p && p->state != PROC_ZOMBIE && !strcmp(p->name, name)) return 1;
    }
    return 0;
}

int proc_update_pending(const char *name) {
    for (int i = 1; i < MAX_PROCS; i++) {
        struct proc *p = procs[i];
        if (p && p->state != PROC_ZOMBIE && p->update_pending && !strcmp(p->name, name)) return 1;
    }
    return 0;
}

void proc_mark_update_pending(const char *name) {
    for (int i = 1; i < MAX_PROCS; i++) {
        struct proc *p = procs[i];
        if (!p || p->state == PROC_ZOMBIE || strcmp(p->name, name)) continue;
        p->update_pending = 1;
        wm_proc_changed(p);
    }
}

int proc_file_in_use(uint32_t ino) {
    for (int i = 1; i < MAX_PROCS; i++) {
        struct proc *p = procs[i];
        if (!p || p->state == PROC_ZOMBIE) continue;
        for (int f = 0; f < MAX_FILES; f++)
            if (p->files[f].used && p->files[f].ino == ino) return 1;
    }
    return 0;
}
