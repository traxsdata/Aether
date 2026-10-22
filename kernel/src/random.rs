// SPDX-License-Identifier: Apache-2.0
//! Kernel random-number generator for `getrandom` and `/dev/urandom`.
//!
//! A SHA-256 hash DRBG seeded from the CPU's hardware entropy instructions
//! (`RDSEED`, else `RDRAND`) and the time-stamp counter. Every request
//! advances the key, so an output never reveals an earlier one, and the key is
//! reseeded from hardware entropy at a fixed request interval. Without either
//! instruction the generator falls back to TSC jitter and reports that it is
//! not a hardware-seeded source.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_crypto::Sha256;
use core::arch::asm;
use core::arch::x86_64::__cpuid;
use core::cell::UnsafeCell;

/// Requests served before the key is mixed with fresh hardware entropy.
const RESEED_INTERVAL: u64 = 1024;
/// Hardware draws attempted per 64-bit word before the word is skipped.
const HARDWARE_RETRIES: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Rdseed,
    Rdrand,
    Tsc,
}

struct State {
    key: [u8; 32],
    counter: u64,
    requests: u64,
    seeded: bool,
    source: Source,
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: Access is serialized by [`InterruptGuard`] on the single CPU.
unsafe impl<T> Sync for Global<T> {}

static STATE: Global<State> = Global(UnsafeCell::new(State {
    key: [0; 32],
    counter: 0,
    requests: 0,
    seeded: false,
    source: Source::Tsc,
}));

fn detect_source() -> Source {
    // CPUID.(EAX=7,ECX=0):EBX[18] = RDSEED; CPUID.1:ECX[30] = RDRAND.
    let max_leaf = __cpuid(0).eax;
    if max_leaf >= 7 && __cpuid(7).ebx & (1 << 18) != 0 {
        return Source::Rdseed;
    }
    if __cpuid(1).ecx & (1 << 30) != 0 {
        return Source::Rdrand;
    }
    Source::Tsc
}

fn rdtsc() -> u64 {
    crate::arch::x86_64::timer::tsc()
}

/// Draws one 64-bit hardware value, or `None` when the instruction keeps
/// reporting that no entropy is ready.
fn hardware_word(source: Source) -> Option<u64> {
    for _ in 0..HARDWARE_RETRIES {
        let value: u64;
        let ok: u8;
        // SAFETY: The instruction was reported by CPUID; it writes only the
        // output register and the carry flag.
        unsafe {
            match source {
                Source::Rdseed => asm!(
                    "rdseed {value}",
                    "setc {ok}",
                    value = out(reg) value,
                    ok = out(reg_byte) ok,
                    options(nomem, nostack)
                ),
                Source::Rdrand => asm!(
                    "rdrand {value}",
                    "setc {ok}",
                    value = out(reg) value,
                    ok = out(reg_byte) ok,
                    options(nomem, nostack)
                ),
                Source::Tsc => return None,
            }
        }
        if ok != 0 {
            return Some(value);
        }
    }
    None
}

fn reseed(state: &mut State) {
    if !state.seeded {
        state.source = detect_source();
    }
    let mut hasher = Sha256::new();
    hasher.update(b"aether-random-seed");
    hasher.update(&state.key);
    for _ in 0..8 {
        if let Some(word) = hardware_word(state.source) {
            hasher.update(&word.to_le_bytes());
        }
        hasher.update(&rdtsc().to_le_bytes());
    }
    state.key = hasher.finish();
    state.requests = 0;
    if !state.seeded {
        state.seeded = true;
        match state.source {
            Source::Rdseed => serial_line!("random: seeded source=rdseed"),
            Source::Rdrand => serial_line!("random: seeded source=rdrand"),
            Source::Tsc => serial_line!("AETHER_RANDOM_WARNING source=tsc hardware=absent"),
        }
    }
}

/// Fills `out` with generator output.
pub fn fill(out: &mut [u8]) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and this is the only accessor.
    let state = unsafe { &mut *STATE.0.get() };
    if !state.seeded || state.requests >= RESEED_INTERVAL {
        reseed(state);
    }
    state.requests += 1;
    for chunk in out.chunks_mut(32) {
        let mut hasher = Sha256::new();
        hasher.update(b"aether-random-out");
        hasher.update(&state.key);
        hasher.update(&state.counter.to_le_bytes());
        state.counter = state.counter.wrapping_add(1);
        let block = hasher.finish();
        chunk.copy_from_slice(&block[..chunk.len()]);
    }
    // Advance the key so a later compromise of the state cannot recompute
    // this request's output.
    let mut hasher = Sha256::new();
    hasher.update(b"aether-random-rekey");
    hasher.update(&state.key);
    hasher.update(&state.counter.to_le_bytes());
    state.key = hasher.finish();
}
