// SPDX-License-Identifier: Apache-2.0
//! Signed update verification and rollback protection self-test.
//!
//! The boot verifies four embedded update manifests against the image bytes
//! they describe:
//!
//! - `current`: a newer version with a newer counter, accepted.
//! - `previous`: a valid older manifest whose counter is not newer, rejected.
//! - `downgrade`: a valid older version with a newer counter, rejected.
//! - `forged`: signed by a key that is not in the trust store, rejected.
//!
//! Installed state is read from `/system/install.txt`. The accepted manifest is
//! recorded in `/system/updates/selected`; activation and reboot are release
//! workflow steps, not something this prototype claims to implement.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::fs;
use crate::serial_line;
use aether_crypto::from_hex;
use aether_packaging::manifest::{Channel, Version};
use aether_packaging::signed::{TrustStore, TrustedPublisher};
use aether_packaging::text::Fields;
use aether_update::{
    InstalledState, PolicyError, PolicyOptions, UpdateError, check_policy, verify_update,
};
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU and
// the status is written once during boot.
unsafe impl<T> Sync for Global<T> {}

const PUBLISHER: &str = "Aether Test Publisher";
const INSTALL_STATE: &str = "/system/install.txt";
const SELECTED: &str = "/system/updates/selected";
const MAX_STATE: usize = 128;

static TRUST: Global<[TrustedPublisher<'static>; 1]> =
    Global(UnsafeCell::new([TrustedPublisher {
        name: PUBLISHER,
        key: [0; 32],
    }]));
static STATUS: Global<Option<Status>> = Global(UnsafeCell::new(None));

static CURRENT_MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/updates/manifests/current.manifest"
));
static CURRENT_IMAGE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/updates/images/image-0.2.0.bin"
));
static PREVIOUS_MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/updates/manifests/previous.manifest"
));
static PREVIOUS_IMAGE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/updates/images/image-0.1.0.bin"
));
static DOWNGRADE_MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/updates/manifests/downgrade.manifest"
));
static DOWNGRADE_IMAGE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/updates/images/image-0.0.9.bin"
));
static FORGED_MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/updates/manifests/forged.manifest"
));
static FORGED_IMAGE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/updates/images/image-0.3.0.bin"
));
static TRUSTED_KEY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/keys/test.pub"
));

/// Update status shown by the shell and security view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// Installed version.
    pub installed: Version,
    /// Installed rollback counter.
    pub installed_counter: u64,
    /// Version of the accepted update.
    pub available: Version,
    /// Counter of the accepted update.
    pub available_counter: u64,
    /// Channel of the accepted update.
    pub channel: Channel,
}

/// Verifies the embedded update set and applies activation policy.
pub fn init() -> Result<(), &'static str> {
    let _guard = InterruptGuard::enter();

    // SAFETY: Interrupts are masked and this runs once during boot.
    let trust = unsafe { &mut *TRUST.0.get() };
    if from_hex(TRUSTED_KEY.trim().as_bytes(), &mut trust[0].key) != Some(32) {
        serial_line!("updates: embedded publisher key is invalid");
        return Err("embedded update publisher key is not 32-byte hex");
    }
    let store = TrustStore::new(trust);

    let current = verify_update(CURRENT_MANIFEST, CURRENT_IMAGE, &store).map_err(|error| {
        serial_line!("updates: current manifest rejected: {error:?}");
        "current update manifest rejected"
    })?;
    let previous = verify_update(PREVIOUS_MANIFEST, PREVIOUS_IMAGE, &store).map_err(|error| {
        serial_line!("updates: previous manifest rejected: {error:?}");
        "previous update manifest rejected"
    })?;
    let downgrade =
        verify_update(DOWNGRADE_MANIFEST, DOWNGRADE_IMAGE, &store).map_err(|error| {
            serial_line!("updates: downgrade manifest rejected: {error:?}");
            "downgrade update manifest rejected"
        })?;
    match verify_update(FORGED_MANIFEST, FORGED_IMAGE, &store) {
        Err(UpdateError::UnknownPublisher) => {}
        other => {
            serial_line!("updates: forged manifest outcome: {other:?}");
            return Err("update signed by an untrusted key was not rejected");
        }
    }

    let installed = read_installed()?;
    if check_policy(&current, installed, PolicyOptions::default()) != Ok(()) {
        serial_line!(
            "updates: current update failed policy against {}.{}.{}/{}",
            installed.version.major,
            installed.version.minor,
            installed.version.patch,
            installed.counter
        );
        return Err("current update was not accepted by policy");
    }
    if check_policy(&previous, installed, PolicyOptions::default()) != Err(PolicyError::Rollback) {
        return Err("replayed update manifest was not rejected as a rollback");
    }
    if check_policy(&downgrade, installed, PolicyOptions::default()) != Err(PolicyError::Downgrade)
    {
        return Err("older update version was not rejected as a downgrade");
    }

    fs::write_path(
        SELECTED,
        b"image-0.2.0.bin version=0.2.0 counter=2 channel=developer\n",
    )
    .map_err(|_| "cannot record the selected update")?;

    // SAFETY: Interrupts are masked and this runs once during boot.
    unsafe {
        *STATUS.0.get() = Some(Status {
            installed: installed.version,
            installed_counter: installed.counter,
            available: current.version,
            available_counter: current.counter,
            channel: current.channel,
        });
    }

    serial_line!(
        "AETHER_UPDATE_OK accepted=1 rejected=3 rollback_protected=true counter={} selected={}.{}.{}",
        current.counter,
        current.version.major,
        current.version.minor,
        current.version.patch
    );
    Ok(())
}

fn read_installed() -> Result<InstalledState, &'static str> {
    let mut buffer = [0u8; MAX_STATE];
    let length =
        fs::read_path(INSTALL_STATE, &mut buffer).map_err(|_| "cannot read installed state")?;
    let fields = Fields::parse(&buffer[..length], MAX_STATE).map_err(|error| {
        serial_line!("updates: installed state is malformed: {error:?}");
        "installed state is malformed"
    })?;
    let version = Version::parse(
        fields
            .require("version")
            .map_err(|_| "missing installed version")?,
    )
    .map_err(|_| "installed version is invalid")?;
    let counter: u64 = fields
        .require("counter")
        .map_err(|_| "missing installed counter")?
        .parse()
        .map_err(|_| "installed counter is invalid")?;
    let channel = Channel::parse(fields.require("channel").map_err(|_| "missing channel")?)
        .map_err(|_| "installed channel is invalid")?;
    Ok(InstalledState {
        version,
        counter,
        channel,
    })
}

/// Returns the update status.
#[must_use]
pub fn status() -> Option<Status> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and the status was written during boot.
    unsafe { *STATUS.0.get() }
}

/// Prints the update status for the shell.
pub fn print_status() {
    match status() {
        Some(status) => {
            serial_line!(
                "updates: installed={}.{}.{} counter={} available={}.{}.{} counter={} channel={}",
                status.installed.major,
                status.installed.minor,
                status.installed.patch,
                status.installed_counter,
                status.available.major,
                status.available.minor,
                status.available.patch,
                status.available_counter,
                status.channel.name()
            );
            serial_line!("updates: accepted=1 rejected=3 rollback_protected=true");
            let mut buffer = [0u8; 96];
            match fs::read_path(SELECTED, &mut buffer) {
                Ok(length) => {
                    serial_line!(
                        "updates: selected={}",
                        core::str::from_utf8(&buffer[..length])
                            .unwrap_or("?")
                            .trim_end()
                    );
                }
                Err(error) => serial_line!("updates: selected marker error {error:?}"),
            }
        }
        None => serial_line!("updates: no verified update manifest"),
    }
}
