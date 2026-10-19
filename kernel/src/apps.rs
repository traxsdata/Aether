// SPDX-License-Identifier: Apache-2.0
//! Application lifecycle manager.
//!
//! The kernel records application state transitions so the supervisor, shell,
//! and (later) desktop can observe them without trusting the application. A
//! process cannot declare its own identity or exit status to the manager.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_services::{AppSpec, AppState, AppTable};
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU.
unsafe impl<T> Sync for Global<T> {}

static APPS: Global<AppTable<4>> = Global(UnsafeCell::new(AppTable::new()));

/// Registers the applications known at boot.
pub fn init() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and this runs once during boot.
    let apps = unsafe { &mut *APPS.0.get() };
    let _ = apps.register(AppSpec {
        name: "demo",
        root: crate::fs::DEMO_ROOT,
        pid: crate::scheduler::USER_TASK,
    });
    let _ = apps.register(AppSpec {
        name: "other",
        root: crate::fs::OTHER_ROOT,
        pid: crate::scheduler::USER_TASK + 1,
    });
}

/// Marks the registered application for `pid` running.
pub fn started(pid: usize) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the update.
    let apps = unsafe { &mut *APPS.0.get() };
    let _ = apps.started(pid);
}

/// Records a normal exit.
pub fn exited(pid: usize, status: i64) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the update.
    let apps = unsafe { &mut *APPS.0.get() };
    let _ = apps.exited(pid, status);
}

/// Records a fault termination.
pub fn faulted(pid: usize, vector: u64) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the update.
    let apps = unsafe { &mut *APPS.0.get() };
    let _ = apps.faulted(pid, vector);
}

/// Prints the application table for the shell.
pub fn print_table() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the duration of the read.
    let apps = unsafe { &*APPS.0.get() };
    serial_line!(
        "apps: registered={} exits={} faults={}",
        apps.len(),
        apps.exits(),
        apps.faults()
    );
    for slot in 0..aether_services::MAX_APPS {
        if let Some(spec) = apps.spec(slot) {
            let state = apps.state(slot);
            match state {
                AppState::Running => serial_line!(
                    "app {} pid={} root={} state=running",
                    spec.name,
                    spec.pid,
                    spec.root
                ),
                AppState::Exited(status) => serial_line!(
                    "app {} pid={} root={} state=exited status={status}",
                    spec.name,
                    spec.pid,
                    spec.root
                ),
                AppState::Faulted(vector) => serial_line!(
                    "app {} pid={} root={} state=faulted vector={vector}",
                    spec.name,
                    spec.pid,
                    spec.root
                ),
                _ => serial_line!(
                    "app {} pid={} root={} state=stopped",
                    spec.name,
                    spec.pid,
                    spec.root
                ),
            }
        }
    }
}
