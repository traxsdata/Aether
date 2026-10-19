// SPDX-License-Identifier: Apache-2.0
//! Host tests for service supervision and application lifecycle rules.

use super::*;

const FILESYSTEM: CapabilityProfile = CapabilityProfile {
    name: "filesystem",
    capabilities: &["file.read", "file.write", "device.block"],
};
const DESKTOP: CapabilityProfile = CapabilityProfile {
    name: "desktop",
    capabilities: &["surface", "input", "portal.request"],
};
static FSD_DEPS: [ServiceId; 1] = [0];

#[test]
fn services_start_only_when_dependencies_are_running() {
    let mut supervisor = Supervisor::new();
    let storage = supervisor
        .register(ServiceSpec {
            name: "storaged",
            profile: FILESYSTEM,
            dependencies: &[],
            restart_limit: 1,
        })
        .expect("storage");
    let fsd = supervisor
        .register(ServiceSpec {
            name: "fsd",
            profile: FILESYSTEM,
            dependencies: &FSD_DEPS,
            restart_limit: 1,
        })
        .expect("fsd");
    assert_eq!(
        supervisor.start(fsd),
        Err(ServiceError::DependencyNotRunning(storage))
    );
    supervisor.start(storage).expect("storage start");
    supervisor.start(fsd).expect("fsd start");
    assert_eq!(supervisor.state(fsd), ServiceState::Running);
    assert_eq!(supervisor.starts(), 2);
}

#[test]
fn restart_budget_quarantines_a_failing_service() {
    let mut supervisor = Supervisor::new();
    let probe = supervisor
        .register(ServiceSpec {
            name: "probe",
            profile: DESKTOP,
            dependencies: &[],
            restart_limit: 1,
        })
        .expect("probe");
    supervisor.start(probe).expect("start");
    supervisor.fail(probe).expect("fail");
    assert_eq!(supervisor.state(probe), ServiceState::Failed);
    supervisor.restart(probe).expect("first restart");
    assert_eq!(supervisor.state(probe), ServiceState::Running);
    supervisor.fail(probe).expect("second fail");
    assert_eq!(supervisor.restart(probe), Err(ServiceError::RestartLimit));
    assert_eq!(supervisor.state(probe), ServiceState::Quarantined);
    assert_eq!(supervisor.count(ServiceState::Quarantined), 1);
}

#[test]
fn profiles_are_narrow_and_explicit() {
    assert!(FILESYSTEM.allows("file.read"));
    assert!(!FILESYSTEM.allows("surface"));
    assert!(!DESKTOP.allows("file.write"));
}

#[test]
fn application_lifecycle_records_exit_and_fault() {
    let mut apps = AppTable::<4>::new();
    let demo = apps
        .register(AppSpec {
            name: "demo",
            root: "/apps/demo",
            pid: 4,
        })
        .expect("demo");
    let other = apps
        .register(AppSpec {
            name: "other",
            root: "/apps/other",
            pid: 5,
        })
        .expect("other");
    assert_eq!(apps.len(), 2);
    assert_eq!(apps.state(demo), AppState::Stopped);
    apps.started(4).expect("started");
    assert_eq!(apps.state(demo), AppState::Running);
    apps.faulted(5, 14).expect("faulted");
    assert_eq!(apps.state(other), AppState::Faulted(14));
    apps.exited(4, 0).expect("exited");
    assert_eq!(apps.state(demo), AppState::Exited(0));
    assert_eq!(apps.exits(), 1);
    assert_eq!(apps.faults(), 1);
}
