// SPDX-License-Identifier: Apache-2.0
//! Ring-3 `linuxd` launch manager: kernel side of the policy channel.
//!
//! The signed AppImage is imported and verified by the kernel, but the
//! application task is not created at boot. The kernel queues a bounded launch
//! request on a capability-scoped IPC endpoint owned by the ring-3 `linuxd`
//! service, and creates the application task only after `linuxd` sends an
//! approval message on the return endpoint. A deliberately malformed request
//! is exercised first, so the denial path is genuine boot evidence and not a
//! claim in a document.
//!
//! The kernel still owns the Linux syscall trap and dispatcher, ELF mapping,
//! and runtime library loading. This module does not remote syscalls; moving
//! the ABI into `linuxd` is Gate 3 work.
//!
//! The service never trusts an application-provided string. Identity, version,
//! and the launch path come from the signed import record and packaged runtime
//! profile. `linuxd` applies the identity/version/path policy in ring 3; the
//! Ed25519 signature and artifact digest are verified here because the
//! signature trust store is kernel boot state, not a user-space file.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_ipc::{CapHandle, CapTable, IpcError, ObjectId, ObjectType, ReceivedMessage, Rights};
use core::cell::UnsafeCell;

/// Overlay store slot owned by the imported AppImage domain.
pub const APP_OVERLAY_ID: u8 = 1;

/// First capability granted to the service task.
///
/// The task starts with an empty table, so the request-receive capability is
/// slot 0 at generation 1: `(1 << 8) | 0`. The user program uses the same
/// documented constant and the kernel checks the grant order at boot.
pub const SERVICE_REQUEST_HANDLE: u64 = 0x100;
/// Second capability granted to the service task: approval send.
pub const SERVICE_APPROVAL_HANDLE: u64 = 0x101;

const TAG_STATUS: u8 = 0;
const TAG_IDENTITY: u8 = 1;
const TAG_VERSION: u8 = 2;
const TAG_PATH: u8 = 3;
const STATUS_REQUIRED: u8 = 0b0111;
const APPROVE: u8 = 1;
const DENY: u8 = 0;
const DENY_PATH: u8 = 1;
/// Largest field one bounded request message carries.
const MAX_FIELD: usize = 63;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Waiting for `linuxd` to reject the deliberately invalid request.
    WaitingDenial,
    /// Waiting for `linuxd` to approve the signed launch request.
    WaitingApproval,
    /// The application task has been created and made runnable.
    Launched,
    /// A protocol or policy failure. The boot fails closed.
    Failed,
}

impl Phase {
    fn raw(self) -> u8 {
        match self {
            Self::WaitingDenial => 0,
            Self::WaitingApproval => 1,
            Self::Launched => 2,
            Self::Failed => 3,
        }
    }

    fn from_raw(value: u8) -> Self {
        match value {
            1 => Self::WaitingApproval,
            2 => Self::Launched,
            3 => Self::Failed,
            _ => Self::WaitingDenial,
        }
    }
}

struct State {
    phase: Phase,
    /// Endpoint on which the kernel sends the launch request.
    request: Option<ObjectId>,
    /// Endpoint on which `linuxd` sends denial/approval.
    approval: Option<ObjectId>,
    request_send: Option<CapHandle>,
    approval_recv: Option<CapHandle>,
}

impl State {
    const EMPTY: Self = Self {
        phase: Phase::WaitingDenial,
        request: None,
        approval: None,
        request_send: None,
        approval_recv: None,
    };
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU, or
// occurs during single-threaded boot before interrupts are enabled.
unsafe impl<T> Sync for Global<T> {}

static STATE: Global<State> = Global(UnsafeCell::new(State::EMPTY));
static KERNEL_CAPS: Global<CapTable> = Global(UnsafeCell::new(CapTable::new()));

/// Runtime manager failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxdError {
    /// The packaged runtime profile is missing or unverified.
    NoRuntimeProfile,
    /// The signed AppImage record or image failed verification.
    ImportFailed,
    /// The signed identity or version is not acceptable.
    Policy,
    /// The selected launch path is not an absolute normalized path.
    LaunchPath,
    /// The policy endpoints or capability grants could not be created.
    Endpoint,
    /// The kernel could not enqueue the bounded launch request.
    Request,
}

impl From<IpcError> for LinuxdError {
    fn from(_: IpcError) -> Self {
        Self::Request
    }
}

fn state() -> &'static mut State {
    // SAFETY: Callers hold the interrupt guard (or run during single-threaded
    // boot), so there is exactly one mutable reference.
    unsafe { &mut *STATE.0.get() }
}

fn kernel_caps() -> &'static CapTable {
    // SAFETY: The kernel capability table is written only during boot and is
    // read with interrupts masked afterwards.
    unsafe { &*KERNEL_CAPS.0.get() }
}

fn kernel_caps_mut() -> &'static mut CapTable {
    // SAFETY: Callers hold the interrupt guard, so no other task can observe
    // or mutate the table concurrently.
    unsafe { &mut *KERNEL_CAPS.0.get() }
}

fn tag_payload(tag: u8, value: &[u8]) -> Option<[u8; aether_ipc::MAX_MESSAGE_BYTES]> {
    if value.len() > MAX_FIELD {
        return None;
    }
    let mut payload = [0u8; aether_ipc::MAX_MESSAGE_BYTES];
    payload[0] = tag;
    payload[1..=value.len()].copy_from_slice(value);
    Some(payload)
}

fn send_tag(tag: u8, value: &[u8]) -> Result<(), LinuxdError> {
    let _guard = InterruptGuard::enter();
    let state = state();
    let Some(handle) = state.request_send else {
        return Err(LinuxdError::Endpoint);
    };
    let payload = tag_payload(tag, value).ok_or(LinuxdError::Policy)?;
    crate::ipc::send(kernel_caps(), handle, 0, &payload[..value.len() + 1], None)?;
    Ok(())
}

/// Queues a complete launch request: status, identity, version, and path.
fn send_request(status: u8, identity: &str, version: &str, path: &str) -> Result<(), LinuxdError> {
    send_tag(TAG_STATUS, &[status])?;
    send_tag(TAG_IDENTITY, identity.as_bytes())?;
    send_tag(TAG_VERSION, version.as_bytes())?;
    send_tag(TAG_PATH, path.as_bytes())?;
    Ok(())
}

fn receive_approval() -> Result<ReceivedMessage, IpcError> {
    let _guard = InterruptGuard::enter();
    let state = state();
    let Some(handle) = state.approval_recv else {
        return Err(IpcError::EndpointGone);
    };
    crate::ipc::receive(kernel_caps_mut(), handle)
}

/// Imports the AppImage, creates the policy endpoints, and queues the first
/// (deliberately invalid) request.
///
/// `service_caps` is the ring-3 `linuxd` task's capability table. The kernel
/// keeps separate send/receive capabilities, so neither side can impersonate
/// the other. The task receives no file, device, portal, or create-endpoint
/// capability.
pub fn init(service_caps: &mut CapTable) -> Result<(), LinuxdError> {
    let Some(profile) = crate::linux::runtime_profile() else {
        serial_line!("linuxd: no verified runtime profile");
        return Err(LinuxdError::NoRuntimeProfile);
    };
    crate::linux::import_appimage().map_err(|_| LinuxdError::ImportFailed)?;
    let Some((id, version, path)) = crate::linux::app_identity() else {
        return Err(LinuxdError::ImportFailed);
    };
    if !aether_packaging::manifest::is_valid_identity(id)
        || id == "org.aether.linux-runtime"
        || version.is_empty()
        || version.len() > 32
    {
        serial_line!("linuxd: AppImage identity rejected");
        return Err(LinuxdError::Policy);
    }
    if !path.starts_with('/')
        || path.len() > 128
        || path.split('/').skip(1).any(|part| part.is_empty())
    {
        serial_line!("linuxd: AppImage launch path rejected");
        return Err(LinuxdError::LaunchPath);
    }
    // The ring-3 request protocol carries one bounded field per message. A
    // path that cannot fit is denied here rather than truncated.
    if id.len() > MAX_FIELD || version.len() > MAX_FIELD || path.len() > MAX_FIELD {
        serial_line!("linuxd: signed launch facts exceed the bounded request");
        return Err(LinuxdError::Policy);
    }

    let request = crate::ipc::create_endpoint(crate::scheduler::LINUXD_TASK as u16)
        .ok_or(LinuxdError::Endpoint)?;
    let approval = crate::ipc::create_endpoint(crate::scheduler::LINUXD_TASK as u16)
        .ok_or(LinuxdError::Endpoint)?;
    let request_recv = service_caps
        .grant(request, ObjectType::Endpoint, Rights::RECEIVE)
        .map_err(|_| LinuxdError::Endpoint)?;
    let approval_send = service_caps
        .grant(approval, ObjectType::Endpoint, Rights::SEND)
        .map_err(|_| LinuxdError::Endpoint)?;
    if u64::from(request_recv.index) | (u64::from(request_recv.generation) << 8)
        != SERVICE_REQUEST_HANDLE
        || u64::from(approval_send.index) | (u64::from(approval_send.generation) << 8)
            != SERVICE_APPROVAL_HANDLE
    {
        serial_line!("linuxd: service capability layout mismatch");
        return Err(LinuxdError::Endpoint);
    }

    let _guard = InterruptGuard::enter();
    let caps = kernel_caps_mut();
    let request_send = caps
        .grant(request, ObjectType::Endpoint, Rights::SEND)
        .map_err(|_| LinuxdError::Endpoint)?;
    let approval_recv = caps
        .grant(approval, ObjectType::Endpoint, Rights::RECEIVE)
        .map_err(|_| LinuxdError::Endpoint)?;
    *state() = State {
        phase: Phase::WaitingDenial,
        request: Some(request),
        approval: Some(approval),
        request_send: Some(request_send),
        approval_recv: Some(approval_recv),
    };
    drop(_guard);

    // Deliberately invalid: the path escapes its component under the same
    // signed identity and version. `linuxd` must deny this request, and the
    // kernel must still be waiting for approval afterwards.
    send_request(STATUS_REQUIRED, id, version, "/usr/bin/../app")
        .map_err(|_| LinuxdError::Request)?;

    serial_line!(
        "AETHER_LINUXD_OK profile={} glibc={} apps=1 id={} version={} path={} service=ring3",
        profile.profile,
        profile.glibc,
        id,
        version,
        path
    );
    Ok(())
}

/// Advances the launch handshake. Called from the timer path.
///
/// The first message must be a path denial for the invalid request. Only after
/// that is observed does the kernel queue the genuine signed request. The
/// application task is created only after an approval message arrives.
pub fn service_tick() {
    let phase = {
        let _guard = InterruptGuard::enter();
        state().phase
    };
    // A missing or crashed approval path must fail the boot rather than leave
    // the application permanently pending. Ten seconds is well beyond the
    // first scheduler tick that lets the service run on the test platform.
    if phase != Phase::Launched && crate::scheduler::ticks() > 1000 {
        failed("linuxd approval timeout");
    }
    match phase {
        Phase::WaitingDenial => match receive_approval() {
            Ok(message) => {
                let payload = message.payload();
                if message.sender as usize != crate::scheduler::LINUXD_TASK
                    || payload != [DENY, DENY_PATH]
                {
                    failed("linuxd denial protocol");
                }
                serial_line!("AETHER_LINUXD_DENY_OK reason=launch-path");
                let Some((id, version, path)) = crate::linux::app_identity() else {
                    failed("linuxd missing imported identity");
                };
                if send_request(STATUS_REQUIRED, id, version, path).is_err() {
                    failed("linuxd valid request enqueue");
                }
                let _guard = InterruptGuard::enter();
                state().phase = Phase::WaitingApproval;
            }
            Err(IpcError::Empty) => {}
            Err(_) => failed("linuxd denial receive"),
        },
        Phase::WaitingApproval => match receive_approval() {
            Ok(message) => {
                let payload = message.payload();
                if message.sender as usize != crate::scheduler::LINUXD_TASK
                    || payload != [APPROVE, STATUS_REQUIRED]
                {
                    failed("linuxd approval protocol");
                }
                if crate::scheduler::launch_appimage().is_err() {
                    failed("linuxd approved launch could not start");
                }
                let _guard = InterruptGuard::enter();
                state().phase = Phase::Launched;
                serial_line!("AETHER_LINUXD_USER_OK ring3=ok ipc=ok launch=approved");
            }
            Err(IpcError::Empty) => {}
            Err(_) => failed("linuxd approval receive"),
        },
        Phase::Launched => {}
        Phase::Failed => failed("linuxd launch state"),
    }
}

/// Returns `true` once the approved application task has been created.
#[must_use]
pub fn launch_approved() -> bool {
    let _guard = InterruptGuard::enter();
    let state = state();
    state.phase == Phase::Launched && state.request.is_some() && state.approval.is_some()
}

fn failed(reason: &str) -> ! {
    serial_line!("AETHER_BOOT_FAILED reason={reason}");
    crate::arch::x86_64::halt()
}

/// Checks overlay persistence after the imported application task has exited.
///
/// The check runs only after the task is `Done`, so it can observe what the
/// application wrote. It then exercises the same erasure entry point the
/// `linux-erase` shell command uses. Nothing here survives a power cycle: the
/// filesystem service is a RAM disk and there is no persistent block device.
pub fn post_app_exit() {
    // The static fixture writes three files to prove overlay persistence. A
    // real dynamically linked application is not required to write anything,
    // so its successful exit is checked by the scheduler result instead.
    if crate::linux::app_is_dynamic() {
        serial_line!("AETHER_LINUX_OVERLAY_SKIP_OK reason=dynamic-app");
        return;
    }
    let (entries, bytes) = crate::scheduler::linux_overlay_stats(APP_OVERLAY_ID);
    if entries == 0 {
        serial_line!("AETHER_BOOT_FAILED reason=linuxd overlay did not persist");
        return;
    }
    serial_line!(
        "AETHER_LINUX_OVERLAY_PERSIST_OK app={} entries={} bytes={}",
        crate::linux::app_identity().map_or("unknown", |(id, _, _)| id),
        entries,
        bytes
    );
    let erased = crate::scheduler::linux_overlay_erase(APP_OVERLAY_ID);
    let (remaining, _) = crate::scheduler::linux_overlay_stats(APP_OVERLAY_ID);
    if erased != entries || remaining != 0 {
        serial_line!("AETHER_BOOT_FAILED reason=linuxd overlay erase");
        return;
    }
    serial_line!("AETHER_LINUX_OVERLAY_ERASE_OK app={APP_OVERLAY_ID} erased={erased}");
}

/// Prints the runtime manager status for the shell and security view.
pub fn print_status() {
    let profile = crate::linux::runtime_profile();
    let app = crate::linux::app_identity();
    let (entries, bytes) = crate::scheduler::linux_overlay_stats(APP_OVERLAY_ID);
    let _guard = InterruptGuard::enter();
    let phase = state().phase;
    drop(_guard);
    serial_line!(
        "linuxd: profile={} glibc={} app={} version={} path={} phase={:?} overlay_entries={entries} overlay_bytes={bytes}",
        profile.map_or("none", |profile| profile.profile.as_str()),
        profile.map_or("none", |profile| profile.glibc.as_str()),
        app.map_or("none", |(id, _, _)| id),
        app.map_or("none", |(_, version, _)| version),
        app.map_or("none", |(_, _, path)| path),
        Phase::from_raw(phase.raw())
    );
}
