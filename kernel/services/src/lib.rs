// SPDX-License-Identifier: Apache-2.0
//! Service startup, failure, and application lifecycle policy.
//!
//! This crate is deliberately independent of the kernel so the rules that
//! decide whether a service may start, restart, or remain quarantined can be
//! tested on the host. The kernel uses the same state machine at boot; it does
//! not duplicate the policy.

#![no_std]

pub mod portal_store;
pub mod runtime;

/// Maximum registered services.
pub const MAX_SERVICES: usize = 12;
/// Maximum registered applications.
pub const MAX_APPS: usize = 4;

/// Service index.
pub type ServiceId = usize;

/// A service's declared capability profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityProfile {
    /// Human-readable profile name.
    pub name: &'static str,
    /// Capabilities the service is allowed to hold.
    pub capabilities: &'static [&'static str],
}

impl CapabilityProfile {
    /// Returns `true` if the profile declares `capability`.
    #[must_use]
    pub fn allows(&self, capability: &str) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// Static service description.
#[derive(Clone, Copy, Debug)]
pub struct ServiceSpec {
    /// Service name.
    pub name: &'static str,
    /// Assigned capability profile.
    pub profile: CapabilityProfile,
    /// Services that must be running first.
    pub dependencies: &'static [ServiceId],
    /// Maximum automatic restarts before quarantine.
    pub restart_limit: u8,
}

/// Service state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    /// No service registered in this slot.
    Empty,
    /// Registered but not started.
    Stopped,
    /// Started and running.
    Running,
    /// Failed but may be restarted within its limit.
    Failed,
    /// Failed too often and quarantined.
    Quarantined,
}

/// Startup and failure errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceError {
    /// No service exists at that index.
    UnknownService,
    /// The registry is full.
    RegistryFull,
    /// A dependency is not running.
    DependencyNotRunning(ServiceId),
    /// A dependency cycle would result (rejected during registration).
    ///
    /// Registration requires every dependency to exist already, so a cycle
    /// cannot be expressed through this API. Kept for a future dynamic
    /// dependency graph.
    DependencyCycle,
    /// The service is already running.
    AlreadyRunning,
    /// The restart budget is exhausted.
    RestartLimit,
    /// The service is quarantined and cannot be started or restarted.
    Quarantined,
    /// A failed service must go through the bounded restart path.
    RestartRequired,
}

/// Service supervisor.
pub struct Supervisor {
    specs: [Option<ServiceSpec>; MAX_SERVICES],
    states: [ServiceState; MAX_SERVICES],
    restarts: [u8; MAX_SERVICES],
    starts: u32,
}

impl Supervisor {
    /// Creates an empty supervisor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            specs: [None; MAX_SERVICES],
            states: [ServiceState::Empty; MAX_SERVICES],
            restarts: [0; MAX_SERVICES],
            starts: 0,
        }
    }

    /// Registers a service and returns its index.
    pub fn register(&mut self, spec: ServiceSpec) -> Result<ServiceId, ServiceError> {
        for dependency in spec.dependencies {
            if *dependency >= MAX_SERVICES || self.specs[*dependency].is_none() {
                return Err(ServiceError::UnknownService);
            }
        }
        let slot = self
            .specs
            .iter()
            .position(Option::is_none)
            .ok_or(ServiceError::RegistryFull)?;
        self.specs[slot] = Some(spec);
        self.states[slot] = ServiceState::Stopped;
        Ok(slot)
    }

    /// Starts a service after checking every dependency.
    pub fn start(&mut self, id: ServiceId) -> Result<(), ServiceError> {
        let spec = self.spec(id)?;
        if self.states[id] == ServiceState::Running {
            return Err(ServiceError::AlreadyRunning);
        }
        if self.states[id] == ServiceState::Quarantined {
            return Err(ServiceError::Quarantined);
        }
        if self.states[id] == ServiceState::Failed {
            return Err(ServiceError::RestartRequired);
        }
        for dependency in spec.dependencies {
            if self.states[*dependency] != ServiceState::Running {
                return Err(ServiceError::DependencyNotRunning(*dependency));
            }
        }
        self.states[id] = ServiceState::Running;
        self.starts = self.starts.saturating_add(1);
        Ok(())
    }

    /// Marks a running service as failed.
    pub fn fail(&mut self, id: ServiceId) -> Result<(), ServiceError> {
        self.spec(id)?;
        if self.states[id] == ServiceState::Running {
            self.states[id] = ServiceState::Failed;
        }
        Ok(())
    }

    /// Restarts a failed service within its restart budget.
    ///
    /// A restart must obey the same dependency rule as a first start. A
    /// service is not allowed to come back while something it depends on is
    /// still failed or quarantined.
    pub fn restart(&mut self, id: ServiceId) -> Result<(), ServiceError> {
        let spec = self.spec(id)?;
        if self.states[id] != ServiceState::Failed {
            return Err(ServiceError::UnknownService);
        }
        for dependency in spec.dependencies {
            if self.states[*dependency] != ServiceState::Running {
                return Err(ServiceError::DependencyNotRunning(*dependency));
            }
        }
        if self.restarts[id] >= spec.restart_limit {
            self.states[id] = ServiceState::Quarantined;
            return Err(ServiceError::RestartLimit);
        }
        self.restarts[id] += 1;
        self.states[id] = ServiceState::Running;
        self.starts = self.starts.saturating_add(1);
        Ok(())
    }

    /// Returns a service state.
    #[must_use]
    pub fn state(&self, id: ServiceId) -> ServiceState {
        self.states.get(id).copied().unwrap_or(ServiceState::Empty)
    }

    /// Returns a service specification.
    pub fn spec(&self, id: ServiceId) -> Result<ServiceSpec, ServiceError> {
        self.specs
            .get(id)
            .and_then(|spec| *spec)
            .ok_or(ServiceError::UnknownService)
    }

    /// Counts services in `state`.
    #[must_use]
    pub fn count(&self, state: ServiceState) -> usize {
        self.states.iter().filter(|value| **value == state).count()
    }

    /// Total successful starts, including restarts.
    #[must_use]
    pub const fn starts(&self) -> u32 {
        self.starts
    }
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Application lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppState {
    /// No application registered.
    Empty,
    /// Registered but not running.
    Stopped,
    /// Running as a task.
    Running,
    /// Exited with a status.
    Exited(i64),
    /// Terminated by a fault.
    Faulted(u64),
}

/// Static application description.
#[derive(Clone, Copy, Debug)]
pub struct AppSpec {
    /// Application name.
    pub name: &'static str,
    /// Private storage root.
    pub root: &'static str,
    /// Task index, assigned when the application starts.
    pub pid: usize,
}

/// Bounded application registry.
pub struct AppTable<const N: usize> {
    specs: [Option<AppSpec>; N],
    states: [AppState; N],
    exits: u32,
    faults: u32,
}

impl<const N: usize> AppTable<N> {
    /// Creates an empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            specs: [None; N],
            states: [AppState::Empty; N],
            exits: 0,
            faults: 0,
        }
    }

    /// Registers an application.
    pub fn register(&mut self, spec: AppSpec) -> Option<usize> {
        let slot = self.specs.iter().position(Option::is_none)?;
        self.specs[slot] = Some(spec);
        self.states[slot] = AppState::Stopped;
        Some(slot)
    }

    /// Marks an application running.
    pub fn started(&mut self, pid: usize) -> Option<usize> {
        let slot = self
            .specs
            .iter()
            .position(|spec| spec.is_some_and(|s| s.pid == pid))?;
        self.states[slot] = AppState::Running;
        Some(slot)
    }

    /// Records a normal exit.
    pub fn exited(&mut self, pid: usize, status: i64) -> Option<usize> {
        let slot = self
            .specs
            .iter()
            .position(|spec| spec.is_some_and(|s| s.pid == pid))?;
        self.states[slot] = AppState::Exited(status);
        self.exits = self.exits.saturating_add(1);
        Some(slot)
    }

    /// Records a fault.
    pub fn faulted(&mut self, pid: usize, vector: u64) -> Option<usize> {
        let slot = self
            .specs
            .iter()
            .position(|spec| spec.is_some_and(|s| s.pid == pid))?;
        self.states[slot] = AppState::Faulted(vector);
        self.faults = self.faults.saturating_add(1);
        Some(slot)
    }

    /// Looks up a spec.
    #[must_use]
    pub fn spec(&self, slot: usize) -> Option<AppSpec> {
        self.specs.get(slot).and_then(|spec| *spec)
    }

    /// Looks up a state.
    #[must_use]
    pub fn state(&self, slot: usize) -> AppState {
        self.states.get(slot).copied().unwrap_or(AppState::Empty)
    }

    /// Number of registered applications.
    #[must_use]
    pub fn len(&self) -> usize {
        self.specs.iter().filter(|spec| spec.is_some()).count()
    }

    /// Returns `true` if no application is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of normal exits.
    #[must_use]
    pub const fn exits(&self) -> u32 {
        self.exits
    }

    /// Number of fault terminations.
    #[must_use]
    pub const fn faults(&self) -> u32 {
        self.faults
    }
}

impl<const N: usize> Default for AppTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
