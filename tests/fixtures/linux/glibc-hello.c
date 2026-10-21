/* SPDX-License-Identifier: Apache-2.0 */
/* Host-glibc smoke test for the Linux dynamic loader.
 *
 * The binary is built against the host glibc at test time and packaged with
 * the host interpreter and libc. This proves the ABI path, not a pinned
 * production runtime; the pinned runtime rootfs is a separate deliverable.
 */
#include <unistd.h>

int main(void) {
    static const char message[] = "AETHER_GLIBC_OK\n";
    return write(1, message, sizeof(message) - 1) == (ssize_t)(sizeof(message) - 1)
               ? 0
               : 1;
}
