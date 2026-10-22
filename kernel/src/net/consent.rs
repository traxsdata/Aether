// SPDX-License-Identifier: Apache-2.0
//! First-use network consent.
//!
//! An application whose signed record declares `network=client` still
//! needs the user's answer before its first connection. The first attempt
//! records a pending request, which the desktop shows; the answer is kept
//! per application in `/system/network/consent` and can be changed later.
//! Until the user answers, connections wait (non-blocking ones report
//! `EINPROGRESS`), and nothing leaves the machine.
//!
//! The record store lives on the system RAM disk like the portal grants, so
//! decisions last until the machine powers off.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_net::consent::{Decision, Decisions, MAX_ID};
use core::cell::UnsafeCell;

const STORE_PATH: &str = "/system/network/consent";
const STORE_BYTES: usize = 4096;

/// Where an application stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Allowed,
    Denied,
    /// The user has not answered; a request is pending.
    Pending,
}

struct State {
    decisions: Decisions,
    pending: Option<([u8; MAX_ID], usize)>,
    /// Tick the pending request was raised.
    requested: u64,
    /// Bumped whenever a request or an answer changes, so the desktop knows
    /// when to repaint.
    generation: u64,
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: Every access below holds an `InterruptGuard` on the single CPU.
unsafe impl<T> Sync for Global<T> {}

static STATE: Global<State> = Global(UnsafeCell::new(State {
    decisions: Decisions::new(),
    pending: None,
    requested: 0,
    generation: 0,
}));

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the borrow; `f` does not re-enter.
    f(unsafe { &mut *STATE.0.get() })
}

/// Loads the decision store. A corrupt store loads nothing, so every
/// application is asked again rather than allowed.
pub fn init() {
    let mut stored = [0u8; STORE_BYTES];
    let Ok(length) = crate::fs::read_path(STORE_PATH, &mut stored) else {
        return;
    };
    match Decisions::parse(&stored[..length]) {
        Ok(decisions) => with(|state| state.decisions = decisions),
        Err(_) => serial_line!("net: consent store corrupt; asking again"),
    }
}

/// Returns the application's access, recording a request the first time.
pub fn check(app: &str) -> Access {
    with(|state| match state.decisions.get(app) {
        Some(Decision::Allow) => Access::Allowed,
        Some(Decision::Deny) => Access::Denied,
        None => {
            if state.pending.is_none() && app.len() <= MAX_ID {
                let mut id = [0u8; MAX_ID];
                id[..app.len()].copy_from_slice(app.as_bytes());
                state.pending = Some((id, app.len()));
                state.requested = crate::scheduler::ticks();
                state.generation += 1;
                serial_line!("net: consent requested app={app}");
            }
            Access::Pending
        }
    })
}

/// Whether the user has blocked `app`. Unlike [`check`], this never raises
/// a request.
#[must_use]
pub fn blocked(app: &str) -> bool {
    with(|state| state.decisions.get(app) == Some(Decision::Deny))
}

/// The application waiting for an answer, if any.
pub fn pending(out: &mut [u8; MAX_ID]) -> Option<usize> {
    with(|state| {
        state.pending.map(|(id, len)| {
            out[..len].copy_from_slice(&id[..len]);
            len
        })
    })
}

/// Records the user's answer for `app` and wakes waiting sockets.
pub fn decide(app: &str, allow: bool) {
    let decision = if allow {
        Decision::Allow
    } else {
        Decision::Deny
    };
    let stored = with(|state| {
        if state
            .pending
            .is_some_and(|(id, len)| &id[..len] == app.as_bytes())
        {
            state.pending = None;
        }
        state.generation += 1;
        state.decisions.set(app, decision).is_ok()
    });
    if !stored {
        serial_line!("net: consent store full; app={app} not recorded");
    }
    serial_line!("net: consent app={app} allow={allow}");
    persist();
    crate::scheduler::linux_wake_pollers();
}

/// Forgets the answer for `app`, so it is asked again on its next attempt.
pub fn forget(app: &str) {
    with(|state| {
        state.decisions.forget(app);
        state.generation += 1;
    });
    persist();
}

/// Changes whenever a request is raised or an answer changes.
#[must_use]
pub fn generation() -> u64 {
    with(|state| state.generation)
}

/// Calls `visit` with each recorded `(app, allowed)` pair.
pub fn for_each(mut visit: impl FnMut(&str, bool)) {
    let decisions = with(|state| state.decisions);
    for (app, decision) in decisions.iter() {
        visit(app, decision == Decision::Allow);
    }
}

fn persist() {
    let mut encoded = [0u8; STORE_BYTES];
    let Some(length) = with(|state| state.decisions.write(&mut encoded)) else {
        return;
    };
    if crate::fs::write_path(STORE_PATH, &encoded[..length]).is_err() {
        serial_line!("net: failed to write consent store");
    }
}

/// Test builds answer "allow" half a second after a request, so the boot
/// test exercises a connect that really waits for the user.
#[cfg(feature = "net-consent-test")]
pub fn test_step() {
    let mut id = [0u8; MAX_ID];
    let due = with(|state| {
        state.pending.and_then(|(pending, len)| {
            (crate::scheduler::ticks() >= state.requested + 50).then(|| {
                id[..len].copy_from_slice(&pending[..len]);
                len
            })
        })
    });
    if let Some(len) = due
        && let Ok(app) = core::str::from_utf8(&id[..len])
    {
        serial_line!("AETHER_NET_CONSENT_TEST allow app={app}");
        decide(app, true);
    }
}
