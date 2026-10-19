// SPDX-License-Identifier: Apache-2.0
//! Service startup and failure policy for the Phase 3 system.
//!
//! Services are started in dependency order with narrow capability profiles.
//! A service that fails may restart within its declared budget; after that it
//! is quarantined and cannot silently acquire new authority. The boot self-test
//! exercises both the restart and quarantine paths.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_services::{
    CapabilityProfile, ServiceError, ServiceId, ServiceSpec, ServiceState, Supervisor,
};
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU.
unsafe impl<T> Sync for Global<T> {}

static SUPERVISOR: Global<Supervisor> = Global(UnsafeCell::new(Supervisor::new()));

const STORAGE_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "storage",
    capabilities: &["device.block"],
};
const FILESYSTEM_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "filesystem",
    capabilities: &["device.block", "file.read", "file.write", "grant.issue"],
};
const APP_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "app-manager",
    capabilities: &["file.read", "process.spawn", "process.observe"],
};
const SHELL_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "console",
    capabilities: &["file.read", "file.write", "process.observe"],
};
const PROBE_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "probe",
    capabilities: &[],
};

static NO_DEPS: [ServiceId; 0] = [];
static FSD_DEPS: [ServiceId; 1] = [0];
static APP_DEPS: [ServiceId; 1] = [1];
static CONSOLE_DEPS: [ServiceId; 1] = [1];

static mut STORAGE: ServiceId = 0;
static mut FILESYSTEM: ServiceId = 0;
static mut APP_MANAGER: ServiceId = 0;
static mut CONSOLE: ServiceId = 0;

/// Starts the Phase 3 services and runs the failure-policy self-test.
pub fn init() -> Result<(), ServiceError> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and this runs once during boot.
    let supervisor = unsafe { &mut *SUPERVISOR.0.get() };

    let storage = supervisor.register(ServiceSpec {
        name: "storaged",
        profile: STORAGE_PROFILE,
        dependencies: &NO_DEPS,
        restart_limit: 1,
    })?;
    let filesystem = supervisor.register(ServiceSpec {
        name: "fsd",
        profile: FILESYSTEM_PROFILE,
        dependencies: &FSD_DEPS,
        restart_limit: 1,
    })?;
    let app_manager = supervisor.register(ServiceSpec {
        name: "appd",
        profile: APP_PROFILE,
        dependencies: &APP_DEPS,
        restart_limit: 1,
    })?;
    let console = supervisor.register(ServiceSpec {
        name: "consoled",
        profile: SHELL_PROFILE,
        dependencies: &CONSOLE_DEPS,
        restart_limit: 0,
    })?;

    supervisor.start(storage)?;
    supervisor.start(filesystem)?;
    supervisor.start(app_manager)?;
    supervisor.start(console)?;

    // Failure-policy probe: one restart is allowed, then quarantine. This is a
    // deliberate boot-time test of the supervisor, not a production service.
    let probe = supervisor.register(ServiceSpec {
        name: "fail-probe",
        profile: PROBE_PROFILE,
        dependencies: &[],
        restart_limit: 1,
    })?;
    supervisor.start(probe)?;
    supervisor.fail(probe)?;
    supervisor.restart(probe)?;
    supervisor.fail(probe)?;
    let quarantined = supervisor.restart(probe) == Err(ServiceError::RestartLimit);

    // SAFETY: Interrupts are masked and these ids are written once.
    unsafe {
        STORAGE = storage;
        FILESYSTEM = filesystem;
        APP_MANAGER = app_manager;
        CONSOLE = console;
    }

    if !quarantined || supervisor.state(probe) != ServiceState::Quarantined {
        serial_line!("AETHER_BOOT_FAILED reason=supervisor quarantine check");
        return Err(ServiceError::RestartLimit);
    }
    serial_line!(
        "AETHER_SUPERVISOR_OK starts={} running={} quarantined={}",
        supervisor.starts(),
        supervisor.count(ServiceState::Running),
        supervisor.count(ServiceState::Quarantined)
    );
    Ok(())
}

/// Prints the registered service profiles and states for the security view.
pub fn print_security() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the duration of the read.
    let supervisor = unsafe { &*SUPERVISOR.0.get() };
    for index in 0..aether_services::MAX_SERVICES {
        let state = supervisor.state(index);
        if state == ServiceState::Empty {
            continue;
        }
        if let Ok(spec) = supervisor.spec(index) {
            serial_line!(
                "service {} profile={} state={:?} caps={}",
                spec.name,
                spec.profile.name,
                state,
                spec.profile.capabilities.len()
            );
        }
    }
}

/// Returns a service state by name.
#[must_use]
pub fn state(name: &str) -> ServiceState {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the duration of the read.
    let supervisor = unsafe { &*SUPERVISOR.0.get() };
    for index in 0..aether_services::MAX_SERVICES {
        if let Ok(spec) = supervisor.spec(index)
            && spec.name == name
        {
            return supervisor.state(index);
        }
    }
    ServiceState::Empty
}

/// Returns the ids used by the boot system (diagnostics only).
#[must_use]
pub fn ids() -> (ServiceId, ServiceId, ServiceId, ServiceId) {
    // SAFETY: These values are written once during boot and then immutable.
    unsafe { (STORAGE, FILESYSTEM, APP_MANAGER, CONSOLE) }
}
