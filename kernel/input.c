/*
 * input.c - one event queue for every keyboard and pointing device.
 *
 * PS/2 and USB HID both feed this queue. The window manager drains it from
 * the kernel loop; the remaining fields are the ABI's raw input shape.
 */
#include "input.h"

#define QUEUE_LEN 256
static struct input_event queue[QUEUE_LEN];
static volatile unsigned q_head, q_tail;
volatile int input_pending;

void input_push(struct input_event e) {
    if (q_head - q_tail >= QUEUE_LEN) return; /* full: drop */
    queue[q_head % QUEUE_LEN] = e;
    q_head++;
    input_pending = 1;
}

int input_pop(struct input_event *out) {
    if (q_tail == q_head) {
        input_pending = 0;
        return 0;
    }
    *out = queue[q_tail % QUEUE_LEN];
    q_tail++;
    return 1;
}
