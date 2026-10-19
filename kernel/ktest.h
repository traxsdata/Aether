/*
 * ktest.h - the kernel's built-in unit tests, run by the terminal's `test`
 * command (SYS_SELFTEST). Each test is a function that makes checks with
 * KT_CHECK; the first failing check in a test is what gets reported.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

struct ktest_result {
    uint32_t passed, failed;
};

/* Runs every suite, or only `suite` if it's non-empty, writing one line per
 * test into out (always NUL-terminated). Returns 0, or -E_NOENT if no suite
 * has that name. */
int ktest_run(const char *suite, char *out, size_t len, struct ktest_result *res);

void ktest_check(int ok, const char *expr, int line);
#define KT_CHECK(e) ktest_check(!!(e), #e, __LINE__)
