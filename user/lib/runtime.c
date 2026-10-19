/* runtime.c - stack-smashing protection for apps. */
#include "sys.h"

uint64_t __stack_chk_guard; /* seeded from the TSC by crt0 before main runs */

__attribute__((noreturn)) void __stack_chk_fail(void) {
    sys_log("stack buffer overflow detected; terminating");
    sys_exit(134);
}
