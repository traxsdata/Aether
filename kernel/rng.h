/* rng.h - the kernel's cryptographic random number generator. */
#pragma once
#include <stddef.h>
#include <stdint.h>

void rng_init(void);                        /* gathers boot entropy; needs the timer running */
void rng_mix(const void *data, size_t len); /* stir in more (input timings and so on) */
void random_bytes(void *out, size_t len);
