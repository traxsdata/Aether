// Aether Gate 4 host spike: a client-side libwayland ABI replacement.
//
// This library implements the *client* half of libwayland-client.so.0 and
// embeds a small in-process "fake compositor".  Nothing is sent over a Unix
// socket.  Requests made by the application through wl_proxy_marshal_* are
// decoded and answered synchronously by queueing protocol events; the display
// fd is an eventfd used only for poll() readiness.
//
// It exists to measure what winit/Smithay-client-toolkit and Mesa's Wayland
// Vulkan WSI actually need before we design the Aether display endpoint.  It
// is not a Wayland server and is not used as Aether evidence.

#define _GNU_SOURCE
#include <errno.h>
#include <dlfcn.h>
#include <execinfo.h>
#include <signal.h>
#include <fcntl.h>
#include <limits.h>
#include <linux/memfd.h>
#include <poll.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include <sys/syscall.h>

#define EXPORT __attribute__((visibility("default")))

/* ------------------------------------------------------------------ */
/* Aether display endpoint ABI (kernel/src/display.rs)                 */
/* ------------------------------------------------------------------ */

#define AETHER_DISPLAY_MAGIC 0x41445450u
#define AETHER_DISPLAY_VERSION 1
#define AETHER_REQ_CREATE 1
#define AETHER_REQ_PRESENT 2
#define AETHER_REQ_DESTROY 3
#define AETHER_REQ_SET_TITLE 4

#define AETHER_EV_CONFIGURE 1
#define AETHER_EV_KEY 2
#define AETHER_EV_POINTER_MOTION 3
#define AETHER_EV_POINTER_BUTTON 4
#define AETHER_EV_POINTER_AXIS 5
#define AETHER_EV_ENTER 6
#define AETHER_EV_LEAVE 7
#define AETHER_EV_CLOSE 8
#define AETHER_EV_FRAME 9

struct aether_display_request {
    uint32_t magic;
    uint16_t version;
    uint16_t kind;
    uint32_t length;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
    uint32_t format;
    uint64_t buffer;
    uint32_t flags;
    uint32_t reserved;
};

struct aether_display_event {
    uint32_t kind;
    uint32_t code;
    int32_t x;
    int32_t y;
    uint32_t state;
    uint32_t serial;
    uint32_t modifiers;
    uint32_t reserved;
} __attribute__((packed));

_Static_assert(sizeof(struct aether_display_request) == 48, "request ABI");
_Static_assert(sizeof(struct aether_display_event) == 32, "event ABI");

/* ------------------------------------------------------------------ */
/* Protocol ABI structures (mirrors wayland-util.h)                    */
/* ------------------------------------------------------------------ */

struct wl_message {
    const char *name;
    const char *signature;
    const struct wl_interface *const *types;
};

struct wl_interface {
    const char *name;
    int version;
    int method_count;
    const struct wl_message *methods;
    int event_count;
    const struct wl_message *events;
};

struct wl_list {
    struct wl_list *prev;
    struct wl_list *next;
};

struct wl_array {
    size_t size;
    size_t alloc;
    void *data;
};

union wl_argument {
    int32_t i;
    uint32_t u;
    int32_t f;
    const char *s;
    void *o;
    uint32_t n;
    const struct wl_array *a;
    int h;
};

typedef int (*wl_dispatcher_func_t)(const void *implementation, void *proxy,
                                    uint32_t opcode,
                                    const struct wl_message *message,
                                    const union wl_argument *args);
typedef void (*wl_log_func_t)(const char *format, va_list args);

struct wl_proxy;
struct wl_display;
struct wl_event_queue;

/* ------------------------------------------------------------------ */
/* Core interface tables                                               */
/* ------------------------------------------------------------------ */

#define MSG(n, s) { n, s, NULL }

static const struct wl_message wl_display_requests[] = {
    MSG("sync", "n"), MSG("get_registry", "n"),
};
static const struct wl_message wl_display_events[] = {
    MSG("error", "ous"), MSG("delete_id", "u"),
};
static const struct wl_message wl_registry_requests[] = {
    MSG("bind", "usun"),
};
static const struct wl_message wl_registry_events[] = {
    MSG("global", "usu"), MSG("global_remove", "u"),
};
static const struct wl_message wl_callback_events[] = { MSG("done", "u") };
static const struct wl_message wl_compositor_requests[] = {
    MSG("create_surface", "n"), MSG("create_region", "n"),
};
static const struct wl_message wl_surface_requests[] = {
    MSG("destroy", ""), MSG("attach", "?oii"), MSG("damage", "iiii"),
    MSG("frame", "n"), MSG("set_opaque_region", "?o"),
    MSG("set_input_region", "?o"), MSG("commit", ""),
    MSG("set_buffer_transform", "i"), MSG("set_buffer_scale", "i"),
    MSG("damage_buffer", "iiii"), MSG("offset", "ii"),
};
static const struct wl_message wl_surface_events[] = {
    MSG("enter", "o"), MSG("leave", "o"),
    MSG("preferred_buffer_scale", "i"),
    MSG("preferred_buffer_transform", "u"),
};
static const struct wl_message wl_region_requests[] = {
    MSG("destroy", ""), MSG("add", "iiii"), MSG("subtract", "iiii"),
};
static const struct wl_message wl_shm_requests[] = { MSG("create_pool", "nhi") };
static const struct wl_message wl_shm_events[] = { MSG("format", "u") };
static const struct wl_message wl_shm_pool_requests[] = {
    MSG("create_buffer", "niiiii"), MSG("destroy", ""), MSG("resize", "i"),
};
static const struct wl_message wl_buffer_requests[] = { MSG("destroy", "") };
static const struct wl_message wl_buffer_events[] = { MSG("release", "") };
static const struct wl_message wl_output_requests[] = { MSG("release", "") };
static const struct wl_message wl_output_events[] = {
    MSG("geometry", "iiiiissi"), MSG("mode", "uiii"), MSG("done", ""),
    MSG("scale", "i"), MSG("name", "s"), MSG("description", "s"),
};
static const struct wl_message wl_seat_requests[] = {
    MSG("get_pointer", "n"), MSG("get_keyboard", "n"),
    MSG("get_touch", "n"), MSG("release", ""),
};
static const struct wl_message wl_seat_events[] = {
    MSG("capabilities", "u"), MSG("name", "s"),
};
static const struct wl_message wl_keyboard_requests[] = { MSG("release", "") };
static const struct wl_message wl_keyboard_events[] = {
    MSG("keymap", "uhu"), MSG("enter", "uoa"), MSG("leave", "uo"),
    MSG("key", "uuuu"), MSG("modifiers", "uuuuu"),
    MSG("repeat_info", "ii"),
};
static const struct wl_message wl_pointer_requests[] = {
    MSG("set_cursor", "?oii"), MSG("release", ""),
};
static const struct wl_message wl_pointer_events[] = {
    MSG("enter", "uoff"), MSG("leave", "uo"), MSG("motion", "uff"),
    MSG("button", "uuuu"), MSG("axis", "uuf"), MSG("frame", ""),
    MSG("axis_source", "u"), MSG("axis_stop", "uu"),
    MSG("axis_discrete", "uui"), MSG("axis_value120", "uii"),
};
static const struct wl_message wl_touch_requests[] = { MSG("release", "") };
static const struct wl_message wl_touch_events[] = {
    MSG("down", "uuoiff"), MSG("up", "uui"), MSG("motion", "uiff"),
    MSG("frame", ""), MSG("cancel", ""), MSG("shape", "iff"),
    MSG("orientation", "if"),
};
static const struct wl_message wl_data_device_manager_requests[] = {
    MSG("create_data_source", "n"), MSG("get_data_device", "no"),
};
static const struct wl_message wl_data_device_requests[] = {
    MSG("start_drag", "?oo?ou"), MSG("set_selection", "?ou"),
    MSG("release", ""),
};
static const struct wl_message wl_data_device_events[] = {
    MSG("data_offer", "n"), MSG("enter", "uoff?o"), MSG("leave", ""),
    MSG("motion", "uff"), MSG("drop", ""), MSG("selection", "?o"),
};
static const struct wl_message wl_data_source_requests[] = {
    MSG("offer", "s"), MSG("destroy", ""), MSG("set_actions", "u"),
};
static const struct wl_message wl_data_source_events[] = {
    MSG("target", "?s"), MSG("send", "sh"), MSG("cancelled", ""),
    MSG("dnd_drop_performed", ""), MSG("dnd_finished", ""),
    MSG("action", "u"),
};
static const struct wl_message wl_data_offer_requests[] = {
    MSG("accept", "u?s"), MSG("receive", "sh"), MSG("destroy", ""),
    MSG("finish", ""), MSG("set_actions", "u"),
};
static const struct wl_message wl_data_offer_events[] = {
    MSG("offer", "s"), MSG("source_actions", "u"), MSG("action", "u"),
};
static const struct wl_message wl_subcompositor_requests[] = {
    MSG("destroy", ""), MSG("get_subsurface", "noo"),
};
static const struct wl_message wl_subsurface_requests[] = {
    MSG("destroy", ""), MSG("set_position", "ii"), MSG("place_above", "o"),
    MSG("place_below", "o"), MSG("set_sync", ""), MSG("set_desync", ""),
};
static const struct wl_message wl_shell_requests[] = { MSG("get_shell_surface", "no") };
static const struct wl_message wl_shell_surface_requests[] = {
    MSG("pong", "u"), MSG("move", "ou"), MSG("resize", "ouu"),
    MSG("set_toplevel", ""), MSG("set_transient", "oiiu"),
    MSG("set_fullscreen", "uu?o"), MSG("set_popup", "ouoiiu"),
};
static const struct wl_message wl_shell_surface_events[] = {
    MSG("ping", "u"), MSG("configure", "uii"), MSG("popup_done", ""),
};

#define IFACE(n, v, rq, ev) { n, v, (int)(sizeof(rq) / sizeof((rq)[0])), rq, \
                              (int)(sizeof(ev) / sizeof((ev)[0])), ev }
#define IFACE0(n, v, rq) { n, v, (int)(sizeof(rq) / sizeof((rq)[0])), rq, 0, NULL }

EXPORT const struct wl_interface wl_display_interface =
    IFACE("wl_display", 1, wl_display_requests, wl_display_events);
EXPORT const struct wl_interface wl_registry_interface =
    IFACE("wl_registry", 1, wl_registry_requests, wl_registry_events);
EXPORT const struct wl_interface wl_callback_interface =
    { "wl_callback", 1, 0, NULL, 1, wl_callback_events };
EXPORT const struct wl_interface wl_compositor_interface =
    IFACE0("wl_compositor", 6, wl_compositor_requests);
EXPORT const struct wl_interface wl_surface_interface =
    IFACE("wl_surface", 6, wl_surface_requests, wl_surface_events);
EXPORT const struct wl_interface wl_region_interface =
    IFACE0("wl_region", 1, wl_region_requests);
EXPORT const struct wl_interface wl_shm_interface =
    IFACE("wl_shm", 1, wl_shm_requests, wl_shm_events);
EXPORT const struct wl_interface wl_shm_pool_interface =
    IFACE0("wl_shm_pool", 1, wl_shm_pool_requests);
EXPORT const struct wl_interface wl_buffer_interface =
    IFACE("wl_buffer", 1, wl_buffer_requests, wl_buffer_events);
EXPORT const struct wl_interface wl_output_interface =
    IFACE("wl_output", 4, wl_output_requests, wl_output_events);
EXPORT const struct wl_interface wl_seat_interface =
    IFACE("wl_seat", 7, wl_seat_requests, wl_seat_events);
EXPORT const struct wl_interface wl_keyboard_interface =
    IFACE("wl_keyboard", 7, wl_keyboard_requests, wl_keyboard_events);
EXPORT const struct wl_interface wl_pointer_interface =
    IFACE("wl_pointer", 7, wl_pointer_requests, wl_pointer_events);
EXPORT const struct wl_interface wl_touch_interface =
    IFACE("wl_touch", 7, wl_touch_requests, wl_touch_events);
EXPORT const struct wl_interface wl_data_device_manager_interface =
    IFACE0("wl_data_device_manager", 3, wl_data_device_manager_requests);
EXPORT const struct wl_interface wl_data_device_interface =
    IFACE("wl_data_device", 3, wl_data_device_requests, wl_data_device_events);
EXPORT const struct wl_interface wl_data_source_interface =
    IFACE("wl_data_source", 3, wl_data_source_requests, wl_data_source_events);
EXPORT const struct wl_interface wl_data_offer_interface =
    IFACE("wl_data_offer", 3, wl_data_offer_requests, wl_data_offer_events);
EXPORT const struct wl_interface wl_subcompositor_interface =
    IFACE0("wl_subcompositor", 1, wl_subcompositor_requests);
EXPORT const struct wl_interface wl_subsurface_interface =
    IFACE0("wl_subsurface", 1, wl_subsurface_requests);
EXPORT const struct wl_interface wl_shell_interface =
    IFACE0("wl_shell", 1, wl_shell_requests);
EXPORT const struct wl_interface wl_shell_surface_interface =
    IFACE("wl_shell_surface", 1, wl_shell_surface_requests,
          wl_shell_surface_events);
/* wl_fixes: only the destroy request; the error event is unused here. */
static const struct wl_message wl_fixes_requests[] = { MSG("destroy", "") };
EXPORT const struct wl_interface wl_fixes_interface =
    IFACE0("wl_fixes", 1, wl_fixes_requests);

/* ------------------------------------------------------------------ */
/* Internal state                                                      */
/* ------------------------------------------------------------------ */

#define MAX_ARGS 8
#define FAKE_WIDTH 1440
#define FAKE_HEIGHT 900

struct wl_event {
    struct wl_event *next;
    struct wl_proxy *target;
    uint32_t opcode;
    union wl_argument args[MAX_ARGS];
};

struct wl_event_queue {
    struct wl_event *head;
    struct wl_event *tail;
    char *name;
    struct wl_display *display;
    struct wl_event_queue *next;
};

struct wl_proxy {
    struct wl_display *display;
    const struct wl_interface *interface;
    uint32_t id;
    uint32_t version;
    int is_wrapper;
    int kind;
    struct wl_event_queue *queue;
    void *user_data;
    void *tag;
    const void **listener;
    void *listener_data;
    wl_dispatcher_func_t dispatcher;
    const void *dispatcher_impl;
    void *state;
    struct wl_proxy *next;
};

struct wl_display {
    struct wl_proxy proxy; /* must be first: clients cast display to proxy */
    int fd;
    int wake_fd;
    pthread_t pump;
    int pump_started;
    volatile int shutdown;
    pthread_mutex_t read_lock;
    uint32_t serial;
    uint32_t next_id;
    int error;
    void *user_data;
    pthread_mutex_t lock;
    struct wl_event_queue default_queue;
    struct wl_event_queue *queues;
    struct wl_proxy *proxies;
    int aether;
    int pending_configure;
    int width;
    int height;
    struct wl_proxy *main_surface;
    struct wl_proxy *cursor_surface;
    struct wl_proxy *xdg;
    struct wl_proxy *toplevel;
    struct wl_proxy *keyboard;
    struct wl_proxy *pointer;
    struct wl_proxy *output;
};

enum proxy_kind { KIND_NONE, KIND_SURFACE, KIND_SHM_POOL, KIND_BUFFER, KIND_XDG,
                  KIND_TOPLEVEL };

struct shm_pool {
    int fd;
    void *map;
    size_t size;
    int refs;
    int dead;
};

struct buffer_state {
    struct shm_pool *pool;
    int32_t offset;
    int32_t width;
    int32_t height;
    int32_t stride;
    uint32_t format;
};

struct xdg_state {
    struct wl_proxy *surface;  /* wl_surface */
    struct wl_proxy *toplevel;
    int configured;
};

struct surface_state {
    struct wl_proxy *pending_buffer;
    struct wl_proxy *frame_callback;
    struct wl_proxy *xdg;
};

static uint64_t g_frames;
static uint64_t g_bytes;
static uint64_t g_checksum;
static int g_last_w, g_last_h, g_last_stride;
static uint32_t g_last_format;
static wl_log_func_t g_log_handler;

static void signal_fd(struct wl_display *dpy);
static int aether_read_events(struct wl_display *dpy);

static void stack_handler(int sig, siginfo_t *si, void *uc) {
    void *bt[64];
    int n;
    (void)sig;
    (void)si;
    (void)uc;
    n = backtrace(bt, 64);
    const char *msg = "HOST_SHIM_STACK\n";
    ssize_t r = write(2, msg, strlen(msg));
    (void)r;
    backtrace_symbols_fd(bt, n, 2);
}

static void crash_handler(int sig) {
    void *bt[64];
    int n = backtrace(bt, 64);
    const char *msg = "HOST_SHIM_CRASH\n";
    ssize_t r = write(2, msg, strlen(msg));
    (void)r;
    backtrace_symbols_fd(bt, n, 2);
    _exit(120);
}

static void *wake_thread(void *arg) {
    struct wl_display *dpy = arg;
    for (int i = 0; i < 100; i++) {
        struct timespec ts = { 0, 100 * 1000 * 1000 };
        nanosleep(&ts, NULL);
        signal_fd(dpy);
    }
    return NULL;
}

static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static void shim_log(const char *fmt, ...) {
    va_list ap;
    if (getenv("G4_SHIM_TID")) {
        char tn[32] = "?";
        pthread_getname_np(pthread_self(), tn, sizeof(tn));
        fprintf(stderr, "[tid=%ld name=%s] ", (long)syscall(SYS_gettid), tn);
    }
    va_start(ap, fmt);
    if (g_log_handler) {
        g_log_handler(fmt, ap);
    } else {
        vfprintf(stderr, fmt, ap);
    }
    va_end(ap);
}

static void signal_fd(struct wl_display *dpy) {
    uint64_t one = 1;
    ssize_t r;
    int fd = (dpy->wake_fd >= 0) ? dpy->wake_fd : dpy->fd;
    if (fd < 0)
        return;
    do {
        r = write(fd, &one, sizeof(one));
    } while (r < 0 && errno == EINTR);
    (void)r; /* EAGAIN only means the counter is already readable */
}

static void drain_fd(struct wl_display *dpy) {
    uint64_t value;
    ssize_t r;
    int fd = (dpy->wake_fd >= 0) ? dpy->wake_fd : dpy->fd;
    if (fd < 0)
        return;
    do {
        r = read(fd, &value, sizeof(value));
    } while (r < 0 && errno == EINTR);
}

static struct wl_proxy *proxy_new(struct wl_display *dpy,
                                  const struct wl_interface *iface,
                                  uint32_t version,
                                  struct wl_event_queue *queue) {
    struct wl_proxy *p = calloc(1, sizeof(*p));
    if (!p)
        return NULL;
    p->display = dpy;
    p->interface = iface;
    p->id = dpy->next_id++;
    p->version = version ? version : (iface ? (uint32_t)iface->version : 1);
    if (iface && p->version > (uint32_t)iface->version)
        p->version = (uint32_t)iface->version;
    p->queue = queue ? queue : &dpy->default_queue;
    p->next = dpy->proxies;
    dpy->proxies = p;
    return p;
}

static const char *iface_name(const struct wl_proxy *p) {
    return (p && p->interface && p->interface->name) ? p->interface->name : "";
}

static int queue_has_events(const struct wl_event_queue *q) {
    return q && q->head != NULL;
}

static void queue_event_raw(struct wl_event_queue *q, struct wl_proxy *target,
                            uint32_t opcode, const union wl_argument *args,
                            int nargs) {
    struct wl_event *ev;
    if (!q || !target)
        return;
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_EMIT iface=%s opcode=%u id=%u target=%p wrapper=%d queue=%p\n",
                 iface_name(target), opcode, target->id, (void *)target,
                 target->is_wrapper, (void *)q);
    ev = calloc(1, sizeof(*ev));
    if (!ev)
        return;
    ev->target = target;
    ev->opcode = opcode;
    if (args && nargs > 0)
        memcpy(ev->args, args, (size_t)(nargs > MAX_ARGS ? MAX_ARGS : nargs) *
                                   sizeof(union wl_argument));
    if (q->tail)
        q->tail->next = ev;
    else
        q->head = ev;
    q->tail = ev;
    signal_fd(target->display);
}

/* Build an event from nargs already-encoded argument words. */
static void emit_n(struct wl_proxy *target, uint32_t opcode, int nargs, ...) {
    union wl_argument args[MAX_ARGS];
    va_list ap;
    int n = 0;
    memset(args, 0, sizeof(args));
    if (!target)
        return;
    if (nargs > MAX_ARGS)
        nargs = MAX_ARGS;
    va_start(ap, nargs);
    while (n < nargs) {
        /* Every arg is passed as uintptr_t and stored in the 8-byte slot. */
        uintptr_t word = va_arg(ap, uintptr_t);
        memcpy(&args[n], &word, sizeof(word));
        n++;
    }
    va_end(ap);
    queue_event_raw(target->queue, target, opcode, args, n);
}

/* Broadcasts an event to every queue of a display.
 *
 * A buffer release has to reach the queue the buffer's owner actually
 * dispatches. The Vulkan Wayland WSI owns the swapchain buffers but runs on
 * the client's display connection, and it dispatches a queue that the shim
 * cannot associate with the buffer proxy. The release listener is
 * idempotent, so broadcasting the release is safe: a second delivery only
 * clears the same busy flag again.
 */
static void queue_event_broadcast(struct wl_display *dpy,
                                  struct wl_proxy *target, uint32_t opcode,
                                  const union wl_argument *args, int nargs) {
    struct wl_event_queue *q;
    if (!dpy || !target)
        return;
    for (q = &dpy->default_queue; q; q = q->next)
        queue_event_raw(q, target, opcode, args, nargs);
}

static struct wl_event *queue_pop(struct wl_event_queue *q) {
    struct wl_event *ev = q->head;
    if (!ev)
        return NULL;
    q->head = ev->next;
    if (!q->head)
        q->tail = NULL;
    ev->next = NULL;
    return ev;
}

static void drop_events_for(struct wl_display *dpy, struct wl_proxy *target) {
    struct wl_event_queue *q;
    for (q = &dpy->default_queue; q; q = q->next) {
        struct wl_event **pp = &q->head;
        q->tail = NULL;
        while (*pp) {
            struct wl_event *ev = *pp;
            if (ev->target == target) {
                *pp = ev->next;
                free(ev);
            } else {
                q->tail = ev;
                pp = &ev->next;
            }
        }
    }
    signal_fd(dpy);
}

static void invoke_event(struct wl_proxy *p, uint32_t opcode,
                         union wl_argument *args) {
    const struct wl_message *msg = NULL;
    if (p->interface && p->interface->events &&
        opcode < (uint32_t)p->interface->event_count)
        msg = &p->interface->events[opcode];
    if (!msg) {
        static const struct wl_message unknown = { "", "", NULL };
        msg = &unknown;
    }
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_INVOKE iface=%s opcode=%u dispatcher=%p listener=%p\n",
                 iface_name(p), opcode, (void *)p->dispatcher,
                 (void *)(p->listener ? p->listener[opcode] : NULL));
    if (p->dispatcher) {
        int rc = p->dispatcher(p->dispatcher_impl, p, opcode, msg, args);
        if (getenv("G4_SHIM_TRACE"))
            shim_log("HOST_SHIM_INVOKE_DONE opcode=%u rc=%d\n", opcode, rc);
    } else if (p->listener && p->listener[opcode]) {
        uintptr_t raw[MAX_ARGS];
        typedef void (*fn_t)(void *, void *, uintptr_t, uintptr_t, uintptr_t,
                             uintptr_t, uintptr_t, uintptr_t, uintptr_t,
                             uintptr_t);
        fn_t fn = (fn_t)p->listener[opcode];
        for (int i = 0; i < MAX_ARGS; i++)
            memcpy(&raw[i], &args[i], sizeof(raw[i]));
        fn(p->listener_data, p, raw[0], raw[1], raw[2], raw[3], raw[4],
           raw[5], raw[6], raw[7]);
    }
}

/* ------------------------------------------------------------------ */
/* Presentation capture                                                */
/* ------------------------------------------------------------------ */

static int aether_write_request(struct wl_display *dpy,
                                  struct aether_display_request *req,
                                  const void *payload, size_t length) {
    if (!dpy || !dpy->aether || dpy->fd < 0 || length > 64)
        return -1;
    uint8_t buffer[sizeof(*req) + 64];
    req->magic = AETHER_DISPLAY_MAGIC;
    req->version = AETHER_DISPLAY_VERSION;
    req->length = (uint32_t)length;
    memcpy(buffer, req, sizeof(*req));
    if (length && payload)
        memcpy(buffer + sizeof(*req), payload, length);
    size_t total = sizeof(*req) + length;
    size_t done = 0;
    while (done < total) {
        ssize_t n = write(dpy->fd, buffer + done, total - done);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (n == 0)
            return -1;
        done += (size_t)n;
    }
    return (int)sizeof(*req);
}

static void aether_create(struct wl_display *dpy) {
    struct aether_display_request req;
    memset(&req, 0, sizeof(req));
    req.kind = AETHER_REQ_CREATE;
    req.width = (uint32_t)dpy->width;
    req.height = (uint32_t)dpy->height;
    req.format = 1; /* ARGB8888 */
    const char *title = "GridCraft";
    aether_write_request(dpy, &req, title, strlen(title));
    if (getenv("G4_SHIM_TRACE")) shim_log("AETHER_BRIDGE_CREATE size=%dx%d\n", dpy->width, dpy->height);
}

static void aether_title(struct wl_display *dpy, const char *title) {
    if (!dpy || !dpy->aether)
        return;
    struct aether_display_request req;
    memset(&req, 0, sizeof(req));
    req.kind = AETHER_REQ_SET_TITLE;
    size_t length = title ? strlen(title) : 0;
    if (length > 64)
        length = 64;
    aether_write_request(dpy, &req, title, length);
}

static void aether_destroy(struct wl_display *dpy) {
    if (!dpy || !dpy->aether || dpy->fd < 0)
        return;
    struct aether_display_request req;
    memset(&req, 0, sizeof(req));
    req.kind = AETHER_REQ_DESTROY;
    aether_write_request(dpy, &req, NULL, 0);
}

static void present_buffer(struct wl_display *dpy,
                           struct buffer_state *buf) {
    if (!buf || !buf->pool || !buf->pool->map)
        return;
    int32_t w = buf->width, h = buf->height, stride = buf->stride;
    if (w <= 0 || h <= 0 || stride <= 0 || w > 16384 || h > 16384)
        return;
    size_t need = (size_t)(h - 1) * (size_t)stride + (size_t)w * 4u;
    if ((size_t)buf->offset > buf->pool->size ||
        need > buf->pool->size - (size_t)buf->offset)
        return;
    const uint8_t *base = (const uint8_t *)buf->pool->map + buf->offset;

    if (dpy->aether) {
        struct aether_display_request req;
        memset(&req, 0, sizeof(req));
        req.kind = AETHER_REQ_PRESENT;
        req.width = (uint32_t)w;
        req.height = (uint32_t)h;
        req.stride = (uint32_t)stride;
        req.format = (buf->format == 0) ? 1 : 0; /* ARGB8888 / XRGB8888 */
        req.buffer = (uint64_t)(uintptr_t)base;
        int result = aether_write_request(dpy, &req, NULL, 0);
        if (result < 0) {
            if (getenv("G4_SHIM_TRACE")) shim_log("AETHER_BRIDGE_PRESENT_FAILED errno=%d\n", errno);
            return;
        }
        g_frames++;
        g_last_w = w;
        g_last_h = h;
        g_last_stride = stride;
        g_last_format = buf->format;
        if (getenv("G4_SHIM_TRACE")) shim_log("AETHER_BRIDGE_PRESENT frames=%llu size=%dx%d stride=%d format=%u\n",
                 (unsigned long long)g_frames, w, h, stride, buf->format);
        return;
    }

    uint64_t sum = 1469598103934665603ULL; /* FNV-1a 64 */
    size_t bytes = 0;
    for (int32_t y = 0; y < h; y++) {
        const uint8_t *row = base + (size_t)y * (size_t)stride;
        for (int32_t x = 0; x < w; x++) {
            sum ^= row[(size_t)x * 4 + 0];
            sum *= 1099511628211ULL;
            sum ^= row[(size_t)x * 4 + 1];
            sum *= 1099511628211ULL;
            sum ^= row[(size_t)x * 4 + 2];
            sum *= 1099511628211ULL;
        }
        bytes += (size_t)w * 4u;
    }

    g_frames++;
    g_bytes += bytes;
    g_checksum = sum;
    g_last_w = w;
    g_last_h = h;
    g_last_stride = stride;
    g_last_format = buf->format;

    const char *path = getenv("G4_SHIM_PPM");
    if (!path || !*path)
        path = "/tmp/opencode/g4-shim-frame.ppm";
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
    if (fd >= 0) {
        char header[96];
        int hn = snprintf(header, sizeof(header), "P6\n%d %d\n255\n", w, h);
        ssize_t off = 0;
        while (off < hn) {
            ssize_t r = write(fd, header + off, (size_t)(hn - off));
            if (r <= 0)
                break;
            off += r;
        }
        uint8_t *rgb = malloc((size_t)w * 3u);
        if (rgb) {
            for (int32_t y = 0; y < h; y++) {
                const uint8_t *row = base + (size_t)y * (size_t)stride;
                for (int32_t x = 0; x < w; x++) {
                    /* ARGB8888/XRGB8888 are B,G,R,A/X in memory (LE). */
                    rgb[(size_t)x * 3 + 0] = row[(size_t)x * 4 + 2];
                    rgb[(size_t)x * 3 + 1] = row[(size_t)x * 4 + 1];
                    rgb[(size_t)x * 3 + 2] = row[(size_t)x * 4 + 0];
                }
                ssize_t left = (ssize_t)w * 3;
                const uint8_t *src = rgb;
                while (left > 0) {
                    ssize_t r = write(fd, src, (size_t)left);
                    if (r <= 0)
                        break;
                    src += r;
                    left -= r;
                }
            }
            free(rgb);
        }
        close(fd);
    }

    shim_log("HOST_SHIM_PRESENT frames=%llu bytes=%llu checksum=0x%016llx "
             "size=%dx%d stride=%d format=%u\n",
             (unsigned long long)g_frames, (unsigned long long)g_bytes,
             (unsigned long long)sum, w, h, stride, buf->format);
    (void)dpy;
}

/* ------------------------------------------------------------------ */
/* Fake compositor request handling                                    */
/* ------------------------------------------------------------------ */

static void send_xdg_configure(struct wl_proxy *xdg) {
    static struct wl_array empty_states = { 0, 0, NULL };
    struct xdg_state *xs = xdg ? xdg->state : NULL;
    if (!xs)
        return;
    xs->configured = 1;
    uint32_t serial = ++xdg->display->serial;
    if (xs->toplevel) {
        /* xdg_toplevel.configure(width, height, states) */
        union wl_argument args[MAX_ARGS];
        memset(args, 0, sizeof(args));
        args[0].i = xdg->display->aether ? xdg->display->width : FAKE_WIDTH;
        args[1].i = xdg->display->aether ? xdg->display->height : FAKE_HEIGHT;
        args[2].a = &empty_states;
        /* The configure is broadcast to the display's queues. A client that
         * keeps one queue per proxy only dispatches the queue of the proxy
         * that carries the event, and it may only start dispatching that
         * queue once the window is registered. A single delivery can
         * therefore be missed and a client that waits for the configure
         * would never draw. The configure is idempotent and the client
         * acknowledges every copy it receives. */
        queue_event_broadcast(xdg->display, xs->toplevel, 0, args, 3);
    }
    union wl_argument args[MAX_ARGS];
    memset(args, 0, sizeof(args));
    args[0].u = serial;
    queue_event_broadcast(xdg->display, xdg, 0, args, 1);
    if (xdg->display->main_surface && xdg->display->output) {
        union wl_argument surface_args[MAX_ARGS];
        memset(surface_args, 0, sizeof(surface_args));
        surface_args[0].o = xdg->display->output;
        queue_event_broadcast(xdg->display, xdg->display->main_surface, 0,
                              surface_args, 1);
    }
}

static void send_keyboard(struct wl_proxy *kb) {
    const uint8_t *km_start = NULL, *km_end = NULL;
    extern const uint8_t _binary_keymap_xkb_start[];
    extern const uint8_t _binary_keymap_xkb_end[];
    km_start = _binary_keymap_xkb_start;
    km_end = _binary_keymap_xkb_end;
    if (!km_start || !km_end || km_end <= km_start)
        return;
    size_t len = (size_t)(km_end - km_start);

    int fd = (int)syscall(SYS_memfd_create, "aether-xkb-keymap", MFD_CLOEXEC);
    if (fd < 0)
        return;
    if (ftruncate(fd, (off_t)len) != 0 ||
        write(fd, km_start, len) != (ssize_t)len) {
        close(fd);
        return;
    }
    lseek(fd, 0, SEEK_SET);
    emit_n(kb, 0, 3, (uintptr_t)1, (uintptr_t)fd, (uintptr_t)len); /* keymap */
    if (kb->version >= 4)
        emit_n(kb, 5, 2, (uintptr_t)25, (uintptr_t)600); /* repeat_info */
    /* The client takes ownership of the fd when it dispatches the event. */
}

static void send_seat(struct wl_proxy *seat) {
    emit_n(seat, 0, 1, (uintptr_t)3); /* caps: pointer|keyboard */
    if (seat->version >= 2)
        emit_n(seat, 1, 1, (uintptr_t)"seat0");
}

static void send_output(struct wl_proxy *out) {
    emit_n(out, 0, 8, (uintptr_t)0, (uintptr_t)0,
           (uintptr_t)(out->display->aether ? out->display->width : FAKE_WIDTH),
           (uintptr_t)(out->display->aether ? out->display->height : FAKE_HEIGHT),
           (uintptr_t)0, (uintptr_t)"Aether",
           (uintptr_t)"Wayland shim", (uintptr_t)0);
    emit_n(out, 1, 4, (uintptr_t)3,
           (uintptr_t)(out->display->aether ? out->display->width : FAKE_WIDTH),
           (uintptr_t)(out->display->aether ? out->display->height : FAKE_HEIGHT),
           (uintptr_t)60000);
    emit_n(out, 3, 1, (uintptr_t)1); /* scale */
    if (out->version >= 4) {
        emit_n(out, 4, 1, (uintptr_t)"shim-0");
        emit_n(out, 5, 1, (uintptr_t)"Aether host Wayland shim");
    }
    emit_n(out, 2, 0); /* done */
}

static void send_globals(struct wl_proxy *registry) {
    emit_n(registry, 0, 3, (uintptr_t)1, (uintptr_t)"wl_compositor",
           (uintptr_t)4);
    emit_n(registry, 0, 3, (uintptr_t)2, (uintptr_t)"wl_shm", (uintptr_t)1);
    emit_n(registry, 0, 3, (uintptr_t)3, (uintptr_t)"wl_seat", (uintptr_t)7);
    emit_n(registry, 0, 3, (uintptr_t)4, (uintptr_t)"wl_output", (uintptr_t)4);
    emit_n(registry, 0, 3, (uintptr_t)5, (uintptr_t)"xdg_wm_base",
           (uintptr_t)3);
}

static void send_shm_formats(struct wl_proxy *shm) {
    emit_n(shm, 0, 1, (uintptr_t)0); /* ARGB8888 */
    emit_n(shm, 0, 1, (uintptr_t)1); /* XRGB8888 */
}

static int32_t bridge_fixed(double value) {
    return (int32_t)(value * 256.0);
}

static uint32_t bridge_evdev(uint32_t code) {
    if (code >= 'a' && code <= 'z')
        return 30u + (code - 'a');
    if (code >= 'A' && code <= 'Z')
        return 30u + (code - 'A');
    if (code >= '1' && code <= '9')
        return 2u + (code - '1');
    if (code == '0')
        return 11;
    if (code == ' ')
        return 57;
    switch (code) {
    case 0x1000: return 28; /* Enter */
    case 0x1001: return 14; /* Backspace */
    case 0x1002: return 111; /* Delete */
    case 0x1003: return 15; /* Tab */
    case 0x1004: return 1;  /* Escape */
    case 0x1005: return 103; /* Up */
    case 0x1006: return 108; /* Down */
    case 0x1007: return 105; /* Left */
    case 0x1008: return 106; /* Right */
    default:
        if ((code & 0xff00) == 0x1100)
            return 59u + (code & 0xff) - 1u; /* F1.. */
        return 0;
    }
}

static void aether_focus(struct wl_display *dpy, int entered,
                         int32_t x, int32_t y) {
    static struct wl_array empty_keys = { 0, 0, NULL };
    uint32_t serial = ++dpy->serial;
    if (dpy->keyboard && dpy->main_surface) {
        if (entered)
            emit_n(dpy->keyboard, 1, 3, (uintptr_t)serial,
                   (uintptr_t)dpy->main_surface, (uintptr_t)&empty_keys);
        else
            emit_n(dpy->keyboard, 2, 2, (uintptr_t)serial,
                   (uintptr_t)dpy->main_surface);
    }
    if (dpy->pointer && dpy->main_surface) {
        if (entered)
            emit_n(dpy->pointer, 0, 5, (uintptr_t)serial,
                   (uintptr_t)dpy->main_surface,
                   (uintptr_t)bridge_fixed(x), (uintptr_t)bridge_fixed(y));
        else
            emit_n(dpy->pointer, 1, 2, (uintptr_t)serial,
                   (uintptr_t)dpy->main_surface);
        emit_n(dpy->pointer, 5, 0); /* frame */
    }
}

static void aether_key(struct wl_display *dpy,
                       const struct aether_display_event *ev) {
    if (!dpy->keyboard)
        return;
    uint32_t serial = ++dpy->serial;
    uint32_t key = bridge_evdev(ev->code);
    if (key == 0)
        return;
    uint32_t depressed = 0;
    if (ev->modifiers & 1) depressed |= 1;   /* shift */
    if (ev->modifiers & 2) depressed |= 4;   /* ctrl */
    if (ev->modifiers & 4) depressed |= 8;   /* alt */
    if (ev->modifiers & 8) depressed |= 64;  /* super */
    emit_n(dpy->keyboard, 4, 5, (uintptr_t)serial, (uintptr_t)depressed,
           (uintptr_t)0, (uintptr_t)0, (uintptr_t)0);
    emit_n(dpy->keyboard, 3, 4, (uintptr_t)serial, (uintptr_t)ev->serial,
           (uintptr_t)key, (uintptr_t)1);
    emit_n(dpy->keyboard, 3, 4, (uintptr_t)serial, (uintptr_t)ev->serial,
           (uintptr_t)key, (uintptr_t)0);
}

static int aether_read_events(struct wl_display *dpy) {
    int result = 0;
    if (!dpy || !dpy->aether)
        return 0;
    /* The display fd has one consumer at a time.  In Aether mode a pump thread
     * and the client may both notice readability, so serialize the actual
     * reads; whichever wins translates the event and queues the Wayland
     * record for the shared display state. */
    pthread_mutex_lock(&dpy->read_lock);
    for (;;) {
        struct aether_display_event ev;
        ssize_t n = read(dpy->fd, &ev, sizeof(ev));
        if (n < 0) {
            if (errno == EINTR)
                continue;
            if (errno == EAGAIN || errno == EWOULDBLOCK)
                break;
            result = -1;
            break;
        }
        if (n == 0)
            break;
        if ((size_t)n != sizeof(ev))
            continue;
        /* Translating an event queues Wayland events for the application, so
         * it serializes against marshaling threads under the display lock.
         * The read itself stays outside the lock: it may block on the fd. */
        pthread_mutex_lock(&dpy->lock);
        switch (ev.kind) {
        case AETHER_EV_CONFIGURE:
            dpy->width = (int)ev.x;
            dpy->height = (int)ev.y;
            if (dpy->xdg)
                send_xdg_configure(dpy->xdg);
            else
                dpy->pending_configure = 1;
            break;
        case AETHER_EV_ENTER:
        case AETHER_EV_POINTER_MOTION:
            aether_focus(dpy, 1, ev.x, ev.y);
            if (dpy->pointer) {
                emit_n(dpy->pointer, 2, 3, (uintptr_t)ev.serial,
                       (uintptr_t)bridge_fixed(ev.x),
                       (uintptr_t)bridge_fixed(ev.y));
                emit_n(dpy->pointer, 5, 0);
            }
            break;
        case AETHER_EV_LEAVE:
            aether_focus(dpy, 0, ev.x, ev.y);
            break;
        case AETHER_EV_KEY:
            aether_key(dpy, &ev);
            break;
        case AETHER_EV_POINTER_BUTTON:
            if (dpy->pointer) {
                uint32_t button = 0x110u + (ev.code > 0 ? ev.code - 1u : 0u);
                emit_n(dpy->pointer, 3, 4, (uintptr_t)ev.serial,
                       (uintptr_t)ev.serial, (uintptr_t)button,
                       (uintptr_t)(ev.state ? 1u : 0u));
                emit_n(dpy->pointer, 5, 0);
            }
            break;
        case AETHER_EV_POINTER_AXIS:
            if (dpy->pointer) {
                emit_n(dpy->pointer, 4, 3, (uintptr_t)ev.serial,
                       (uintptr_t)0, (uintptr_t)(intptr_t)(ev.x * 256));
                emit_n(dpy->pointer, 5, 0);
            }
            break;
        case AETHER_EV_CLOSE:
            if (dpy->toplevel)
                emit_n(dpy->toplevel, 1, 0); /* xdg_toplevel.close */
            break;
        default:
            break;
        }
        pthread_mutex_unlock(&dpy->lock);
    }
    pthread_mutex_unlock(&dpy->read_lock);
    return result;
}

/* Aether mode: the kernel endpoint is the only real event source.  Poll it and
 * translate records as they arrive, then signal the local wake eventfd so a
 * client blocked in poll(wl_display_get_fd()) runs its dispatch loop.  The pump
 * deliberately does not poll the wake fd, so it can never consume a wake that
 * another thread queued for the client. */
static void *aether_pump_thread(void *arg) {
    struct wl_display *dpy = arg;
    while (!dpy->shutdown) {
        struct pollfd pfd = { .fd = dpy->fd, .events = POLLIN };
        int r = poll(&pfd, 1, 50);
        if (r < 0 && errno != EINTR)
            break;
        if (dpy->shutdown)
            break;
        if (r > 0 && (pfd.revents & (POLLIN | POLLERR | POLLHUP))) {
            aether_read_events(dpy);
            signal_fd(dpy);
        }
    }
    return NULL;
}

static void xdg_state_free(struct wl_proxy *p) {
    free(p->state);
    p->state = NULL;
}

static void release_pool_ref(struct shm_pool *pool) {
    if (!pool)
        return;
    pool->refs--;
    if (pool->refs <= 0 && pool->dead) {
        if (pool->map && pool->map != MAP_FAILED)
            munmap(pool->map, pool->size);
        if (pool->fd >= 0)
            close(pool->fd);
        free(pool);
    }
}

static void free_proxy_state(struct wl_proxy *p) {
    if (!p || !p->state)
        return;
    if (p->kind == KIND_BUFFER) {
        struct buffer_state *bs = p->state;
        release_pool_ref(bs->pool);
        free(bs);
    } else if (p->kind == KIND_SHM_POOL) {
        struct shm_pool *pool = p->state;
        pool->dead = 1;
        release_pool_ref(pool);
    } else {
        free(p->state);
    }
    p->state = NULL;
}

static void handle_request(struct wl_proxy *proxy, uint32_t opcode,
                           const struct wl_interface *new_iface,
                           uint32_t version, union wl_argument *args,
                           struct wl_proxy *created) {
    const char *name = iface_name(proxy);
    struct wl_display *dpy = proxy->display;

    if (created) {
        if (strcmp(name, "wl_shm") == 0 && opcode == 0) {
            struct shm_pool *pool = calloc(1, sizeof(*pool));
            if (pool) {
                int fd = dup(args[1].h);
                pool->fd = fd;
                pool->refs = 1; /* the pool itself owns one reference */
                pool->size = (size_t)(uint32_t)args[2].i;
                if (fd >= 0 && pool->size > 0) {
                    struct stat st;
                    if (fstat(fd, &st) == 0 && (size_t)st.st_size > 0 &&
                        (size_t)st.st_size < (1u << 30))
                        pool->size = (size_t)st.st_size;
                    pool->map = mmap(NULL, pool->size, PROT_READ, MAP_SHARED,
                                     fd, 0);
                    if (pool->map == MAP_FAILED)
                        pool->map = NULL;
                }
                created->state = pool;
                created->kind = KIND_SHM_POOL;
            }
            return;
        }
        if (strcmp(name, "wl_shm_pool") == 0 && opcode == 0) {
            struct shm_pool *pool = proxy->state;
            struct buffer_state *bs = calloc(1, sizeof(*bs));
            if (bs) {
                bs->pool = pool;
                bs->offset = args[1].i;
                bs->width = args[2].i;
                bs->height = args[3].i;
                bs->stride = args[4].i;
                bs->format = args[5].u;
                if (pool)
                    pool->refs++;
                created->state = bs;
                created->kind = KIND_BUFFER;
                if (getenv("G4_SHIM_TRACE")) shim_log("AETHER_BRIDGE_BUFFER id=%u queue=%p %dx%d stride=%d fmt=%u\n",
                         created->id, (void *)created->queue, bs->width,
                         bs->height, bs->stride, bs->format);
                queue_event_raw(&dpy->default_queue, created, 0, NULL, 0);
                queue_event_raw(created->queue, created, 0, NULL, 0);
            }
            return;
        }
        if (strcmp(name, "wl_compositor") == 0 && opcode == 0) {
            struct surface_state *ss = calloc(1, sizeof(*ss));
            if (ss)
                created->state = ss;
            created->kind = KIND_SURFACE;
            if (!dpy->main_surface)
                dpy->main_surface = created;
            return;
        }
        if (strcmp(name, "xdg_wm_base") == 0 && opcode == 2) {
            struct xdg_state *xs = calloc(1, sizeof(*xs));
            if (xs) {
                xs->surface = args[1].o;
                dpy->main_surface = args[1].o;
                created->state = xs;
                created->kind = KIND_XDG;
                if (xs->surface)
                    ((struct surface_state *)xs->surface->state)->xdg = created;
                dpy->xdg = created;
            }
            return;
        }
        if (strcmp(name, "xdg_surface") == 0 && opcode == 1) { /* get_toplevel */
            struct xdg_state *xs = proxy->state;
            if (xs)
                xs->toplevel = created;
            dpy->toplevel = created;
            if (dpy->xdg && dpy->pending_configure) {
                dpy->pending_configure = 0;
                send_xdg_configure(dpy->xdg);
            }
            created->state = NULL;
            created->interface = created->interface; /* keep Rust table */
            return;
        }
        if (strcmp(name, "wl_surface") == 0 && opcode == 3) {
            /* frame(): the fake compositor has no vblank, so complete the
             * callback on the next dispatch instead of waiting for a commit. */
            struct surface_state *ss = proxy->state;
            if (ss)
                ss->frame_callback = created;
            emit_n(created, 0, 1, (uintptr_t)(uint32_t)now_ms());
            if (ss)
                ss->frame_callback = NULL;
            return;
        }
    }

    if (strcmp(name, "wl_display") == 0) {
        if (opcode == 0) { /* sync */
            if (getenv("G4_SHIM_TRACE")) shim_log("AETHER_BRIDGE_SYNC created=%p queue=%p\n",
                     (void *)created, created ? (void *)created->queue : NULL);
            if (created)
            {
                if (getenv("G4_SHIM_TRACE"))
                    shim_log("HOST_SHIM_SYNC_DONE id=%u\n", created->id);
                emit_n(created, 0, 1, (uintptr_t)(uint32_t)now_ms());
            }
        } else if (opcode == 1) { /* get_registry */
            if (created)
                send_globals(created);
        }
        return;
    }
    if (strcmp(name, "wl_registry") == 0 && opcode == 0) {
        /* bind(name, interface, version, new_id) */
        if (!created)
            return;
        const char *iname = iface_name(created);
        if (strcmp(iname, "wl_shm") == 0)
            send_shm_formats(created);
        else if (strcmp(iname, "wl_seat") == 0)
            send_seat(created);
        else if (strcmp(iname, "wl_output") == 0) {
            dpy->output = created;
            send_output(created);
        }
        return;
    }
    if (strcmp(name, "wl_surface") == 0) {
        struct surface_state *ss = proxy->state;
        if (getenv("G4_SHIM_TRACE")) shim_log("AETHER_BRIDGE_SURFACE id=%u wrapper=%d opcode=%u state=%p\n",
                 proxy->id, proxy->is_wrapper, opcode, (void *)ss);
        if (!ss)
            return;
        if (opcode == 0) { /* destroy */
            drop_events_for(dpy, proxy);
            free(ss);
            proxy->state = NULL;
        } else if (opcode == 1) { /* attach */
            ss->pending_buffer = args[0].o;
            if (getenv("G4_SHIM_TRACE")) shim_log("AETHER_BRIDGE_ATTACH surface=%u buffer=%p\n",
                     proxy->id, args[0].o);
        } else if (opcode == 6) { /* commit */
            if (getenv("G4_SHIM_TRACE")) shim_log("AETHER_BRIDGE_COMMIT surface=%u pending=%p\n",
                     proxy->id, (void *)ss->pending_buffer);
            if (ss->pending_buffer) {
                struct buffer_state *bs = ss->pending_buffer->state;
                if (proxy != dpy->cursor_surface)
                    present_buffer(dpy, bs);
                /* The release must reach the queue the buffer owner
                 * dispatches, which is not necessarily the buffer's own
                 * queue. Broadcasting keeps the swapchain image free. */
                queue_event_broadcast(dpy, ss->pending_buffer, 0, NULL, 0);
                ss->pending_buffer = NULL;
            } else {
                struct wl_proxy *xdg = ss->xdg ? ss->xdg : dpy->xdg;
                if (xdg) {
                    struct xdg_state *xs = xdg->state;
                    if (xs && xs->toplevel && !xs->configured)
                        send_xdg_configure(xdg);
                }
            }
            if (ss->frame_callback) {
                union wl_argument args[MAX_ARGS];
                memset(args, 0, sizeof(args));
                args[0].u = (uint32_t)now_ms();
                queue_event_broadcast(dpy, ss->frame_callback, 0, args, 1);
                ss->frame_callback = NULL;
            }
        }
        return;
    }
    if (strcmp(name, "wl_buffer") == 0 && opcode == 0) {
        struct buffer_state *bs = proxy->state;
        if (bs) {
            release_pool_ref(bs->pool);
            free(bs);
            proxy->state = NULL;
        }
        drop_events_for(dpy, proxy);
        return;
    }
    if (strcmp(name, "wl_shm_pool") == 0) {
        if (opcode == 1) { /* destroy */
            struct shm_pool *pool = proxy->state;
            if (pool) {
                pool->dead = 1;
                release_pool_ref(pool);
                proxy->state = NULL;
            }
            drop_events_for(dpy, proxy);
        }
        return;
    }
    if (strcmp(name, "wl_seat") == 0) {
        if (opcode == 0 && created)
            dpy->pointer = created;
        if (opcode == 1 && created) {
            dpy->keyboard = created;
            send_keyboard(created);
        }
        return;
    }
    if (strcmp(name, "wl_pointer") == 0 && opcode == 0) {
        dpy->cursor_surface = args[0].o;
        return;
    }
    if (strcmp(name, "xdg_toplevel") == 0 && opcode == 2) { /* set_title */
        aether_title(dpy, args[0].s);
        return;
    }
    if (strcmp(name, "xdg_wm_base") == 0) {
        if (opcode == 3) /* pong */
            return;
        return;
    }
    if (strcmp(name, "xdg_surface") == 0) {
        struct xdg_state *xs = proxy->state;
        if (!xs)
            return;
        if (opcode == 0) { /* destroy */
            if (xs->surface) {
                struct surface_state *ss = xs->surface->state;
                if (ss && ss->xdg == proxy)
                    ss->xdg = NULL;
            }
            xdg_state_free(proxy);
            drop_events_for(dpy, proxy);
        } else if (opcode == 4) { /* ack_configure */
            /* accepted; nothing to do for the shim */
        }
        return;
    }
    if (strcmp(name, "xdg_toplevel") == 0) {
        if (opcode == 0) {
            xdg_state_free(proxy);
            drop_events_for(dpy, proxy);
        }
        return;
    }
    (void)version;
    (void)new_iface;
    (void)args;
}

/* ------------------------------------------------------------------ */
/* Proxy API                                                           */
/* ------------------------------------------------------------------ */

static void destroy_proxy_now(struct wl_display *dpy, struct wl_proxy *proxy) {
    struct wl_proxy **pp;
    if (!proxy || proxy->is_wrapper || proxy == &dpy->proxy)
        return;
    pthread_mutex_lock(&dpy->lock);
    drop_events_for(dpy, proxy);
    for (pp = &dpy->proxies; *pp; pp = &(*pp)->next) {
        if (*pp == proxy) {
            *pp = proxy->next;
            break;
        }
    }
    pthread_mutex_unlock(&dpy->lock);
    free_proxy_state(proxy);
    free(proxy);
}

static struct wl_proxy *marshal_request(struct wl_proxy *proxy,
                                        uint32_t opcode,
                                        const struct wl_interface *iface,
                                        uint32_t version, uint32_t flags,
                                        union wl_argument *args) {
    struct wl_display *dpy;
    int is_ctor = iface != NULL;
    struct wl_proxy *created = NULL;
    struct wl_event_queue *queue;
    if (!proxy || !proxy->display)
        return NULL;
    dpy = proxy->display;
    if (proxy->is_wrapper)
        queue = proxy->queue;
    else
        queue = proxy->queue;

    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_REQ iface=%s opcode=%u proxy_id=%u proxy=%p version=%u%s%s\n",
                 iface_name(proxy), opcode, proxy->id, (void *)proxy,
                 proxy->version, is_ctor ? " constructor" : "",
                 proxy->is_wrapper ? " wrapper" : "");

    /* Snap off the constructor, then run the fake compositor under the
     * display lock: handle_request mutates per-proxy state and queues the
     * response events, so it must not interleave with the dispatch or read
     * paths, which mutate the same queues under this lock. */
    pthread_mutex_lock(&dpy->lock);
    if (is_ctor) {
        created = proxy_new(dpy, iface, version, queue);
        if (!created) {
            pthread_mutex_unlock(&dpy->lock);
            return NULL;
        }
    }
    handle_request(proxy, opcode, iface, version, args, created);
    pthread_mutex_unlock(&dpy->lock);

    if (is_ctor)
        return created;
    if (flags & 1 /* WL_MARSHAL_FLAG_DESTROY */)
        destroy_proxy_now(dpy, proxy);
    return NULL;
}

EXPORT struct wl_proxy *wl_proxy_marshal_flags(struct wl_proxy *proxy,
                                               uint32_t opcode,
                                               const struct wl_interface *iface,
                                               uint32_t version, uint32_t flags,
                                               ...) {
    union wl_argument args[MAX_ARGS];
    const char *sig = NULL;
    va_list ap;
    int n = 0;
    memset(args, 0, sizeof(args));
    if (!proxy || !proxy->interface || !proxy->interface->methods ||
        opcode >= (uint32_t)proxy->interface->method_count)
        return NULL;
    sig = proxy->interface->methods[opcode].signature;
    va_start(ap, flags);
    for (const char *c = sig; *c && n < MAX_ARGS; c++) {
        if (*c == '?')
            continue;
        switch (*c) {
        case 'i': args[n].i = va_arg(ap, int); break;
        case 'u': args[n].u = va_arg(ap, uint32_t); break;
        case 'f': args[n].f = va_arg(ap, int); break;
        case 's': args[n].s = va_arg(ap, const char *); break;
        case 'o': args[n].o = va_arg(ap, void *); break;
        case 'n': args[n].n = va_arg(ap, uint32_t); break;
        case 'a': args[n].a = va_arg(ap, void *); break;
        case 'h': args[n].h = va_arg(ap, int); break;
        default: break;
        }
        n++;
    }
    va_end(ap);
    return marshal_request(proxy, opcode, iface, version, flags, args);
}

EXPORT struct wl_proxy *wl_proxy_marshal_array_flags(
    struct wl_proxy *proxy, uint32_t opcode, const struct wl_interface *iface,
    uint32_t version, uint32_t flags, union wl_argument *args) {
    return marshal_request(proxy, opcode, iface, version, flags, args);
}

EXPORT struct wl_proxy *wl_proxy_marshal_constructor(
    struct wl_proxy *proxy, uint32_t opcode, const struct wl_interface *iface,
    ...) {
    union wl_argument args[MAX_ARGS];
    const char *sig = NULL;
    va_list ap;
    int n = 0;
    memset(args, 0, sizeof(args));
    if (!proxy || !proxy->interface || !proxy->interface->methods ||
        opcode >= (uint32_t)proxy->interface->method_count)
        return NULL;
    sig = proxy->interface->methods[opcode].signature;
    va_start(ap, iface);
    for (const char *c = sig; *c && n < MAX_ARGS; c++) {
        if (*c == '?')
            continue;
        switch (*c) {
        case 'i': args[n].i = va_arg(ap, int); break;
        case 'u': args[n].u = va_arg(ap, uint32_t); break;
        case 'f': args[n].f = va_arg(ap, int); break;
        case 's': args[n].s = va_arg(ap, const char *); break;
        case 'o': args[n].o = va_arg(ap, void *); break;
        case 'n': args[n].n = va_arg(ap, uint32_t); break;
        case 'a': args[n].a = va_arg(ap, void *); break;
        case 'h': args[n].h = va_arg(ap, int); break;
        default: break;
        }
        n++;
    }
    va_end(ap);
    return marshal_request(proxy, opcode, iface,
                           iface ? (uint32_t)iface->version : 1, 0, args);
}

EXPORT struct wl_proxy *wl_proxy_marshal_constructor_versioned(
    struct wl_proxy *proxy, uint32_t opcode, const struct wl_interface *iface,
    uint32_t version, ...) {
    union wl_argument args[MAX_ARGS];
    const char *sig = NULL;
    va_list ap;
    int n = 0;
    memset(args, 0, sizeof(args));
    if (!proxy || !proxy->interface || !proxy->interface->methods ||
        opcode >= (uint32_t)proxy->interface->method_count)
        return NULL;
    sig = proxy->interface->methods[opcode].signature;
    va_start(ap, version);
    for (const char *c = sig; *c && n < MAX_ARGS; c++) {
        if (*c == '?')
            continue;
        switch (*c) {
        case 'i': args[n].i = va_arg(ap, int); break;
        case 'u': args[n].u = va_arg(ap, uint32_t); break;
        case 'f': args[n].f = va_arg(ap, int); break;
        case 's': args[n].s = va_arg(ap, const char *); break;
        case 'o': args[n].o = va_arg(ap, void *); break;
        case 'n': args[n].n = va_arg(ap, uint32_t); break;
        case 'a': args[n].a = va_arg(ap, void *); break;
        case 'h': args[n].h = va_arg(ap, int); break;
        default: break;
        }
        n++;
    }
    va_end(ap);
    return marshal_request(proxy, opcode, iface, version, 0, args);
}

EXPORT struct wl_proxy *wl_proxy_marshal_array_constructor(
    struct wl_proxy *proxy, uint32_t opcode, union wl_argument *args,
    const struct wl_interface *iface) {
    return marshal_request(proxy, opcode, iface,
                           iface ? (uint32_t)iface->version : 1, 0, args);
}

EXPORT struct wl_proxy *wl_proxy_marshal_array_constructor_versioned(
    struct wl_proxy *proxy, uint32_t opcode, union wl_argument *args,
    const struct wl_interface *iface, uint32_t version) {
    return marshal_request(proxy, opcode, iface, version, 0, args);
}

EXPORT struct wl_proxy *wl_proxy_marshal_array(struct wl_proxy *proxy,
                                               uint32_t opcode,
                                               union wl_argument *args) {
    return marshal_request(proxy, opcode, NULL, 0, 0, args);
}

EXPORT void wl_proxy_marshal(struct wl_proxy *proxy, uint32_t opcode, ...) {
    union wl_argument args[MAX_ARGS];
    const char *sig = NULL;
    va_list ap;
    int n = 0;
    memset(args, 0, sizeof(args));
    if (!proxy || !proxy->interface || !proxy->interface->methods ||
        opcode >= (uint32_t)proxy->interface->method_count)
        return;
    sig = proxy->interface->methods[opcode].signature;
    va_start(ap, opcode);
    for (const char *c = sig; *c && n < MAX_ARGS; c++) {
        if (*c == '?')
            continue;
        switch (*c) {
        case 'i': args[n].i = va_arg(ap, int); break;
        case 'u': args[n].u = va_arg(ap, uint32_t); break;
        case 'f': args[n].f = va_arg(ap, int); break;
        case 's': args[n].s = va_arg(ap, const char *); break;
        case 'o': args[n].o = va_arg(ap, void *); break;
        case 'n': args[n].n = va_arg(ap, uint32_t); break;
        case 'a': args[n].a = va_arg(ap, void *); break;
        case 'h': args[n].h = va_arg(ap, int); break;
        default: break;
        }
        n++;
    }
    va_end(ap);
    marshal_request(proxy, opcode, NULL, 0, 0, args);
}

EXPORT int wl_proxy_add_listener(struct wl_proxy *proxy,
                                 void (**listener)(void), void *data) {
    if (!proxy || !listener)
        return -1;
    proxy->listener = (const void **)listener;
    proxy->listener_data = data;
    return 0;
}

EXPORT const void *wl_proxy_get_listener(struct wl_proxy *proxy) {
    return proxy ? proxy->listener : NULL;
}

EXPORT int wl_proxy_add_dispatcher(struct wl_proxy *proxy,
                                   wl_dispatcher_func_t dispatcher,
                                   const void *implementation, void *data) {
    if (!proxy || !dispatcher)
        return -1;
    proxy->dispatcher = dispatcher;
    proxy->dispatcher_impl = implementation;
    proxy->listener = (const void **)implementation;
    proxy->listener_data = data;
    proxy->user_data = data; /* libwayland: 4th arg is the proxy user data */
    return 0;
}

EXPORT void wl_proxy_set_user_data(struct wl_proxy *proxy, void *data) {
    if (proxy)
        proxy->user_data = data;
}

EXPORT void *wl_proxy_get_user_data(struct wl_proxy *proxy) {
    return proxy ? proxy->user_data : NULL;
}

EXPORT uint32_t wl_proxy_get_id(struct wl_proxy *proxy) {
    return proxy ? proxy->id : 0;
}

EXPORT const char *wl_proxy_get_class(struct wl_proxy *proxy) {
    return iface_name(proxy);
}

EXPORT struct wl_display *wl_proxy_get_display(struct wl_proxy *proxy) {
    return proxy ? proxy->display : NULL;
}

EXPORT void wl_proxy_set_queue(struct wl_proxy *proxy,
                               struct wl_event_queue *queue) {
    struct wl_display *dpy;
    struct wl_event *moved_head = NULL, *moved_tail = NULL;
    struct wl_event_queue *q;
    if (!proxy || !queue || proxy->queue == queue)
        return;
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_SETQUEUE iface=%s id=%u proxy=%p wrapper=%d -> %p\n",
                 iface_name(proxy), proxy->id, (void *)proxy, proxy->is_wrapper,
                 (void *)queue);
    dpy = proxy->display;
    pthread_mutex_lock(&dpy->lock);
    for (q = &dpy->default_queue; q; q = q->next) {
        struct wl_event **pp = &q->head;
        q->tail = NULL;
        while (*pp) {
            struct wl_event *ev = *pp;
            if (ev->target == proxy) {
                *pp = ev->next;
                ev->next = NULL;
                if (moved_tail)
                    moved_tail->next = ev;
                else
                    moved_head = ev;
                moved_tail = ev;
            } else {
                q->tail = ev;
                pp = &ev->next;
            }
        }
    }
    if (moved_head) {
        if (queue->tail)
            queue->tail->next = moved_head;
        else
            queue->head = moved_head;
        queue->tail = moved_tail;
        signal_fd(dpy);
    }
    proxy->queue = queue;
    pthread_mutex_unlock(&dpy->lock);
}

EXPORT struct wl_event_queue *wl_proxy_get_queue(struct wl_proxy *proxy) {
    return proxy ? proxy->queue : NULL;
}

EXPORT uint32_t wl_proxy_get_version(struct wl_proxy *proxy) {
    return proxy ? proxy->version : 0;
}

EXPORT const struct wl_interface *wl_proxy_get_interface(struct wl_proxy *proxy) {
    return proxy ? proxy->interface : NULL;
}

EXPORT void wl_proxy_set_tag(struct wl_proxy *proxy, const void *tag) {
    if (proxy)
        proxy->tag = (void *)tag;
}

EXPORT const void *wl_proxy_get_tag(struct wl_proxy *proxy) {
    return proxy ? proxy->tag : NULL;
}

EXPORT struct wl_proxy *wl_proxy_create(struct wl_proxy *factory,
                                        const struct wl_interface *iface) {
    struct wl_proxy *p;
    if (!factory || !factory->display)
        return NULL;
    pthread_mutex_lock(&factory->display->lock);
    p = proxy_new(factory->display, iface, iface ? (uint32_t)iface->version : 1,
                  factory->queue);
    pthread_mutex_unlock(&factory->display->lock);
    return p;
}

EXPORT struct wl_proxy *wl_proxy_create_wrapper(struct wl_proxy *proxy) {
    struct wl_proxy *w;
    if (!proxy)
        return NULL;
    w = calloc(1, sizeof(*w));
    if (!w)
        return NULL;
    *w = *proxy;
    w->is_wrapper = 1;
    if (getenv("G4_SHIM_TRACE")) {
        Dl_info di;
        const char *mod = "?";
        if (dladdr(__builtin_return_address(0), &di) && di.dli_fname)
            mod = di.dli_fname;
        shim_log("HOST_SHIM_WRAPPER iface=%s id=%u orig=%p wrapper=%p queue=%p from=%s\n",
                 iface_name(proxy), proxy->id, (void *)proxy, (void *)w,
                 (void *)w->queue, mod);
    }
    w->next = NULL;
    w->listener = NULL;
    w->dispatcher = NULL;
    w->user_data = NULL;
    return w;
}

EXPORT void wl_proxy_wrapper_destroy(struct wl_proxy *proxy) {
    if (proxy)
        free(proxy);
}

EXPORT void wl_proxy_destroy(struct wl_proxy *proxy) {
    if (!proxy || proxy->is_wrapper)
        return;
    if (!proxy->display || proxy == &proxy->display->proxy)
        return;
    destroy_proxy_now(proxy->display, proxy);
}

/* ------------------------------------------------------------------ */
/* Display and event queue API                                         */
/* ------------------------------------------------------------------ */

static struct wl_display *display_alloc(int fd) {
    struct wl_display *dpy = calloc(1, sizeof(*dpy));
    if (!dpy)
        return NULL;
    dpy->fd = fd;
    dpy->wake_fd = -1;
    dpy->serial = 1;
    dpy->next_id = 2; /* id 1 is reserved for wl_display, as in libwayland */
    pthread_mutex_init(&dpy->lock, NULL);
    pthread_mutex_init(&dpy->read_lock, NULL);
    dpy->proxy.display = dpy;
    dpy->proxy.interface = &wl_display_interface;
    dpy->proxy.id = 1;
    dpy->proxy.version = 1;
    dpy->proxy.queue = &dpy->default_queue;
    dpy->default_queue.display = dpy;
    dpy->default_queue.name = strdup("default");
    return dpy;
}

EXPORT struct wl_display *wl_display_connect(const char *name) {
    (void)name;
    int fd = open("/dev/aether-display", O_RDWR | O_CLOEXEC);
    int aether = fd >= 0;
    if (fd < 0)
        fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    if (fd < 0)
        return NULL;
    struct wl_display *dpy = display_alloc(fd);
    if (!dpy) {
        close(fd);
        return NULL;
    }
    dpy->aether = aether;
    dpy->width = 1440;
    dpy->height = 900;
    if (aether) {
        dpy->wake_fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
        if (dpy->wake_fd < 0 && getenv("G4_SHIM_TRACE"))
            shim_log("AETHER_BRIDGE_WAKE_FD_FAILED errno=%d\n", errno);
        aether_create(dpy);
        if (pthread_create(&dpy->pump, NULL, aether_pump_thread, dpy) == 0)
            dpy->pump_started = 1;
        else if (getenv("G4_SHIM_TRACE"))
            shim_log("AETHER_BRIDGE_PUMP_FAILED errno=%d\n", errno);
    }
    if (getenv("G4_SHIM_CRASH")) {
        struct sigaction sa;
        memset(&sa, 0, sizeof(sa));
        sa.sa_sigaction = stack_handler;
        sa.sa_flags = SA_SIGINFO;
        sigaction(SIGUSR1, &sa, NULL);
        signal(SIGSEGV, crash_handler);
    }
    if (!aether && getenv("G4_SHIM_WAKE")) {
        pthread_t th;
        if (pthread_create(&th, NULL, wake_thread, dpy) == 0)
            pthread_detach(th);
    }
    shim_log("HOST_SHIM_DISPLAY connected name=%s fd=%d %s\n",
             name ? name : "(null)", fd,
             aether ? "aether-display" : "(in-process, no socket)");
    return dpy;
}

EXPORT struct wl_display *wl_display_connect_to_fd(int fd) {
    (void)fd;
    return wl_display_connect(NULL);
}

EXPORT void wl_display_disconnect(struct wl_display *dpy) {
    struct wl_proxy *p, *pn;
    struct wl_event_queue *q, *qn;
    if (!dpy)
        return;
    dpy->shutdown = 1;
    signal_fd(dpy);
    if (dpy->pump_started) {
        pthread_join(dpy->pump, NULL);
        dpy->pump_started = 0;
    }
    aether_destroy(dpy);
    shim_log("HOST_SHIM_SUMMARY frames=%llu bytes=%llu checksum=0x%016llx "
             "size=%dx%d stride=%d format=%u\n",
             (unsigned long long)g_frames, (unsigned long long)g_bytes,
             (unsigned long long)g_checksum, g_last_w, g_last_h, g_last_stride,
             g_last_format);
    p = dpy->proxies;
    while (p) {
        pn = p->next;
        free(p->state);
        free(p);
        p = pn;
    }
    q = dpy->default_queue.next;
    while (q) {
        qn = q->next;
        struct wl_event *ev = q->head;
        while (ev) {
            struct wl_event *en = ev->next;
            free(ev);
            ev = en;
        }
        free(q->name);
        free(q);
        q = qn;
    }
    if (dpy->wake_fd >= 0)
        close(dpy->wake_fd);
    if (dpy->fd >= 0)
        close(dpy->fd);
    pthread_mutex_destroy(&dpy->read_lock);
    pthread_mutex_destroy(&dpy->lock);
    free(dpy);
}

EXPORT int wl_display_get_fd(struct wl_display *dpy) {
    if (!dpy)
        return -1;
    /* In Aether mode the kernel endpoint is consumed by the pump thread, so
     * poll readiness is reported on the local wake eventfd instead. */
    int fd = dpy->wake_fd >= 0 ? dpy->wake_fd : dpy->fd;
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_GETFD fd=%d\n", fd);
    return fd;
}

EXPORT int wl_display_flush(struct wl_display *dpy) {
    return dpy ? 0 : -1;
}

EXPORT void wl_display_set_max_buffer_size(struct wl_display *dpy, size_t size) {
    (void)dpy;
    (void)size;
}

EXPORT struct wl_event_queue *wl_display_create_queue_with_name(
    struct wl_display *dpy, const char *name);

EXPORT struct wl_event_queue *wl_display_create_queue(struct wl_display *dpy) {
    return wl_display_create_queue_with_name(dpy, NULL);
}

EXPORT struct wl_event_queue *wl_display_create_queue_with_name(
    struct wl_display *dpy, const char *name) {
    struct wl_event_queue *q;
    if (!dpy)
        return NULL;
    q = calloc(1, sizeof(*q));
    if (!q)
        return NULL;
    q->name = name ? strdup(name) : NULL;
    q->display = dpy;
    pthread_mutex_lock(&dpy->lock);
    q->next = dpy->default_queue.next;
    dpy->default_queue.next = q;
    pthread_mutex_unlock(&dpy->lock);
    return q;
}

EXPORT const char *wl_event_queue_get_name(struct wl_event_queue *q) {
    return q ? q->name : NULL;
}

EXPORT void wl_event_queue_destroy(struct wl_event_queue *q) {
    struct wl_event *ev, *en;
    if (!q)
        return;
    if (q->display) {
        struct wl_event_queue **pp;
        pthread_mutex_lock(&q->display->lock);
        for (pp = &q->display->default_queue.next; *pp; pp = &(*pp)->next) {
            if (*pp == q) {
                *pp = q->next;
                break;
            }
        }
        pthread_mutex_unlock(&q->display->lock);
    }
    ev = q->head;
    while (ev) {
        en = ev->next;
        free(ev);
        ev = en;
    }
    free(q->name);
    free(q);
}

static int dispatch_one(struct wl_event_queue *q) {
    struct wl_display *dpy;
    struct wl_event *ev;
    if (!q)
        return -1;
    if (getenv("G4_SHIM_TRACE") && q->head)
        shim_log("HOST_SHIM_DISPATCH iface=%s opcode=%u id=%u\n",
                 iface_name(q->head->target), q->head->opcode,
                 q->head->target->id);
    dpy = q->display;
    if (!dpy)
        return 0;
    pthread_mutex_lock(&dpy->lock);
    ev = queue_pop(q);
    pthread_mutex_unlock(&dpy->lock);
    if (!ev)
        return 0;
    invoke_event(ev->target, ev->opcode, ev->args);
    free(ev);
    return 1;
}

EXPORT int wl_display_dispatch_queue_pending(struct wl_display *dpy,
                                             struct wl_event_queue *q) {
    int dispatched = 0;
    if (!dpy || !q)
        return -1;
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_DQP_ENTER queue=%p pending=%d\n", (void *)q,
                 queue_has_events(q));
    for (;;) {
        int did = 0;
        /* Pending dispatch is queue-scoped: drain this queue and no other.
         * The display's own default queue is drained as well: it carries the
         * events of the objects that the client left on the default queue,
         * such as the buffer release of a swapchain image the Vulkan WSI
         * owns. Leaving it undrained keeps those images busy forever and
         * stalls the next acquire. */
        struct wl_event_queue *queues[2] = { q, &dpy->default_queue };
        for (int i = 0; i < 2; i++) {
            while (queue_has_events(queues[i])) {
                int r = dispatch_one(queues[i]);
                if (r < 0)
                    return -1;
                if (r == 0)
                    break;
                dispatched++;
                did = 1;
            }
        }
        if (!did)
            break;
    }
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_DQP_LEAVE dispatched=%d\n", dispatched);
    return dispatched;
}

EXPORT int wl_display_dispatch_pending(struct wl_display *dpy) {
    if (!dpy)
        return -1;
    return wl_display_dispatch_queue_pending(dpy, &dpy->default_queue);
}

static int wait_readable(struct wl_display *dpy, int timeout_ms) {
    if (dpy->aether && dpy->wake_fd >= 0) {
        struct pollfd pfds[2] = {
            { .fd = dpy->wake_fd, .events = POLLIN },
            { .fd = dpy->fd, .events = POLLIN },
        };
        int r;
        do {
            r = poll(pfds, 2, timeout_ms);
        } while (r < 0 && errno == EINTR);
        if (r <= 0)
            return r;
        if (pfds[1].revents & (POLLIN | POLLERR | POLLHUP)) {
            if (aether_read_events(dpy) < 0)
                return -1;
        }
        /* Return on a local wake even when the pump consumed the kernel
         * record: queued Wayland events still need dispatching. */
        if (pfds[0].revents & (POLLIN | POLLERR | POLLHUP))
            drain_fd(dpy);
        return 1;
    }
    struct pollfd pfd = { .fd = dpy->fd, .events = POLLIN };
    int r;
    do {
        r = poll(&pfd, 1, timeout_ms);
    } while (r < 0 && errno == EINTR);
    if (r <= 0)
        return r;
    if (dpy->aether)
        return aether_read_events(dpy) < 0 ? -1 : r;
    drain_fd(dpy);
    return r;
}

EXPORT int wl_display_dispatch_queue(struct wl_display *dpy,
                                     struct wl_event_queue *q) {
    if (!dpy || !q)
        return -1;
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_WAIT queue=%s pending=%d\n",
                 q->name ? q->name : "?", queue_has_events(q));
    for (;;) {
        if (queue_has_events(q)) {
            int r = dispatch_one(q);
            if (r != 0)
                return 0;
        }
        if (dpy->error)
            return -1;
        if (wait_readable(dpy, -1) < 0)
            return -1;
    }
}

static int timespec_to_ms(const struct timespec *ts) {
    long long ms;
    if (!ts)
        return -1;
    ms = (long long)ts->tv_sec * 1000LL +
         ((long long)ts->tv_nsec + 999999LL) / 1000000LL;
    if (ms < 0)
        ms = 0;
    if (ms > INT_MAX)
        ms = INT_MAX;
    return (int)ms;
}

EXPORT int wl_display_dispatch_queue_timeout(struct wl_display *dpy,
                                             struct wl_event_queue *q,
                                             const struct timespec *timeout) {
    int timeout_ms;
    if (!dpy || !q)
        return -1;
    timeout_ms = timespec_to_ms(timeout);
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_DQT queue=%p pending=%d timeout=%d\n", (void *)q,
                 queue_has_events(q), timeout_ms);
    for (;;) {
        if (queue_has_events(q)) {
            int r = dispatch_one(q);
            if (r != 0)
                return 0;
        }
        /* The display's own default queue carries the events of the objects
         * the client left there, such as the release of a swapchain image
         * that the Vulkan WSI owns. Draining it here keeps those images
         * free; leaving it undrained stalls the next acquire. */
        if (queue_has_events(&dpy->default_queue)) {
            int r = dispatch_one(&dpy->default_queue);
            if (r != 0)
                return 0;
        }
        if (dpy->error)
            return -1;
        int r = wait_readable(dpy, timeout_ms);
        if (r == 0) {
            errno = EAGAIN;
            return -1;
        }
        if (r < 0)
            return -1;
    }
}

EXPORT int wl_display_dispatch(struct wl_display *dpy) {
    if (!dpy)
        return -1;
    return wl_display_dispatch_queue(dpy, &dpy->default_queue);
}

EXPORT int wl_display_dispatch_timeout(struct wl_display *dpy,
                                       const struct timespec *timeout) {
    if (!dpy)
        return -1;
    return wl_display_dispatch_queue_timeout(dpy, &dpy->default_queue,
                                             timeout);
}

EXPORT int wl_display_prepare_read_queue(struct wl_display *dpy,
                                         struct wl_event_queue *q) {
    if (!dpy || !q)
        return -1;
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_PREPARE queue=%p pending=%d\n",
                 (void *)q, queue_has_events(q));
    if (queue_has_events(q) || queue_has_events(&dpy->default_queue)) {
        errno = EAGAIN;
        return -1;
    }
    return 0;
}

EXPORT int wl_display_prepare_read(struct wl_display *dpy) {
    if (!dpy)
        return -1;
    return wl_display_prepare_read_queue(dpy, &dpy->default_queue);
}

EXPORT void wl_display_cancel_read(struct wl_display *dpy) {
    (void)dpy;
}

EXPORT int wl_display_read_events(struct wl_display *dpy) {
    if (!dpy)
        return -1;
    if (getenv("G4_SHIM_TRACE"))
        shim_log("HOST_SHIM_READ\n");
    if (dpy->aether) {
        int result = aether_read_events(dpy);
        drain_fd(dpy);
        return result < 0 ? -1 : 0;
    }
    drain_fd(dpy);
    return 0;
}

EXPORT int wl_display_roundtrip_queue(struct wl_display *dpy,
                                      struct wl_event_queue *q) {
    if (!dpy || !q)
        return -1;
    if (dpy->aether) {
        if (aether_read_events(dpy) < 0)
            return -1;
        drain_fd(dpy);
    }
    /* Requests are answered synchronously, so every response caused by the
     * caller's earlier requests is already queued on the caller's queue.  A
     * roundtrip must not drain other threads' queues: their listeners may
     * require state (including thread-local dispatch state) that only the
     * owning thread has set up. */
    for (;;) {
        int did = 0;
        while (queue_has_events(q)) {
            int r = dispatch_one(q);
            if (r <= 0)
                break;
            did = 1;
        }
        if (!did)
            break;
    }
    return dpy->error ? -1 : 0;
}

EXPORT int wl_display_roundtrip(struct wl_display *dpy) {
    if (!dpy)
        return -1;
    return wl_display_roundtrip_queue(dpy, &dpy->default_queue);
}

EXPORT int wl_display_get_error(struct wl_display *dpy) {
    return dpy ? dpy->error : -1;
}

EXPORT uint32_t wl_display_get_protocol_error(struct wl_display *dpy,
                                              const struct wl_interface **iface,
                                              uint32_t *id) {
    (void)dpy;
    if (iface)
        *iface = NULL;
    if (id)
        *id = 0;
    return 0;
}

EXPORT uint32_t wl_display_get_serial(struct wl_display *dpy) {
    return dpy ? dpy->serial : 0;
}

EXPORT void wl_display_set_user_data(struct wl_display *dpy, void *data) {
    if (dpy)
        dpy->user_data = data;
}

EXPORT void *wl_display_get_user_data(struct wl_display *dpy) {
    return dpy ? dpy->user_data : NULL;
}

EXPORT void wl_log_set_handler_client(wl_log_func_t handler) {
    g_log_handler = handler;
}

/* ------------------------------------------------------------------ */
/* List and array helpers                                              */
/* ------------------------------------------------------------------ */

EXPORT void wl_list_init(struct wl_list *list) {
    if (list) {
        list->prev = list;
        list->next = list;
    }
}

EXPORT void wl_list_insert(struct wl_list *list, struct wl_list *elm) {
    if (!list || !elm)
        return;
    elm->prev = list->prev;
    elm->next = list;
    list->prev->next = elm;
    list->prev = elm;
}

EXPORT void wl_list_remove(struct wl_list *elm) {
    if (!elm || !elm->prev || !elm->next)
        return;
    elm->prev->next = elm->next;
    elm->next->prev = elm->prev;
    elm->prev = elm->next = NULL;
}

EXPORT void wl_list_insert_list(struct wl_list *list, struct wl_list *other) {
    if (!list || !other || other->next == other)
        return;
    other->next->prev = list->prev;
    list->prev->next = other->next;
    other->prev->next = list;
    list->prev = other->prev;
    other->prev = other->next = other;
}

EXPORT int wl_list_length(const struct wl_list *list) {
    int n = 0;
    if (!list)
        return 0;
    for (const struct wl_list *p = list->next; p && p != list; p = p->next)
        n++;
    return n;
}

EXPORT int wl_list_empty(const struct wl_list *list) {
    return !list || list->next == list;
}

EXPORT void wl_array_init(struct wl_array *array) {
    if (array)
        memset(array, 0, sizeof(*array));
}

EXPORT void wl_array_release(struct wl_array *array) {
    if (!array)
        return;
    free(array->data);
    array->data = NULL;
    array->size = 0;
    array->alloc = 0;
}

EXPORT void *wl_array_add(struct wl_array *array, size_t size) {
    size_t alloc;
    void *data;
    if (!array)
        return NULL;
    if (array->size + size > array->alloc) {
        alloc = array->alloc ? array->alloc : 16;
        while (alloc < array->size + size)
            alloc *= 2;
        data = realloc(array->data, alloc);
        if (!data)
            return NULL;
        array->data = data;
        array->alloc = alloc;
    }
    data = (char *)array->data + array->size;
    array->size += size;
    return data;
}

EXPORT void wl_array_copy(struct wl_array *array, struct wl_array *source) {
    if (!array || !source)
        return;
    wl_array_release(array);
    if (source->size && wl_array_add(array, source->size))
        memcpy(array->data, source->data, source->size);
}
