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
const INPUT_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "input",
    capabilities: &["input.keyboard"],
};
const DISPLAY_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "display",
    capabilities: &["framebuffer", "surface.compose"],
};
const DESKTOP_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "desktop",
    capabilities: &[
        "surface.compose",
        "input.read",
        "portal.request",
        "policy.observe",
    ],
};
const PORTAL_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "portal",
    capabilities: &["file.read", "grant.issue", "grant.revoke"],
};
const SECURITY_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "security",
    capabilities: &["policy.observe", "grant.observe"],
};
const LINUX_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "linux",
    capabilities: &["file.read", "process.spawn", "policy.observe"],
};
const PROBE_PROFILE: CapabilityProfile = CapabilityProfile {
    name: "probe",
    capabilities: &[],
};

static NO_DEPS: [ServiceId; 0] = [];
static FSD_DEPS: [ServiceId; 1] = [0];
static APP_DEPS: [ServiceId; 1] = [1];
static CONSOLE_DEPS: [ServiceId; 1] = [1];
static DISPLAY_DEPS: [ServiceId; 1] = [4];
static DESKTOP_DEPS: [ServiceId; 3] = [1, 2, 5];
static PORTAL_DEPS: [ServiceId; 2] = [1, 6];
static SECURITY_DEPS: [ServiceId; 2] = [1, 6];
static LINUXD_DEPS: [ServiceId; 2] = [1, 2];

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

    let input = supervisor.register(ServiceSpec {
        name: "inputd",
        profile: INPUT_PROFILE,
        dependencies: &NO_DEPS,
        restart_limit: 1,
    })?;
    let display = supervisor.register(ServiceSpec {
        name: "displayd",
        profile: DISPLAY_PROFILE,
        dependencies: &DISPLAY_DEPS,
        restart_limit: 1,
    })?;
    let desktop = supervisor.register(ServiceSpec {
        name: "desktopd",
        profile: DESKTOP_PROFILE,
        dependencies: &DESKTOP_DEPS,
        restart_limit: 1,
    })?;
    let portal = supervisor.register(ServiceSpec {
        name: "portald",
        profile: PORTAL_PROFILE,
        dependencies: &PORTAL_DEPS,
        restart_limit: 1,
    })?;
    let security = supervisor.register(ServiceSpec {
        name: "securityd",
        profile: SECURITY_PROFILE,
        dependencies: &SECURITY_DEPS,
        restart_limit: 1,
    })?;
    let linuxd = supervisor.register(ServiceSpec {
        name: "linuxd",
        profile: LINUX_PROFILE,
        dependencies: &LINUXD_DEPS,
        restart_limit: 1,
    })?;

    supervisor.start(storage)?;
    supervisor.start(filesystem)?;
    supervisor.start(app_manager)?;
    supervisor.start(console)?;
    supervisor.start(input)?;
    supervisor.start(display)?;
    supervisor.start(desktop)?;
    supervisor.start(portal)?;
    supervisor.start(security)?;
    supervisor.start(linuxd)?;

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

/// Returns the number of running services.
#[must_use]
pub fn running_count() -> usize {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    unsafe { (*SUPERVISOR.0.get()).count(ServiceState::Running) }
}

/// Returns the number of quarantined services.
#[must_use]
pub fn quarantined_count() -> usize {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    unsafe { (*SUPERVISOR.0.get()).count(ServiceState::Quarantined) }
}
