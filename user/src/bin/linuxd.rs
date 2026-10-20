// SPDX-License-Identifier: Apache-2.0
//! Ring-3 `linuxd` launch manager.
//!
//! The kernel imports and verifies the signed AppImage, then asks this
//! process, over a pair of capability-scoped IPC endpoints, whether the
//! imported identity, version, and launch path may start. This program owns
//! the launch policy: the kernel must not create or schedule the Linux
//! application task until it receives an approval message from here.
//!
//! The service holds exactly two capabilities, installed by the kernel before
//! its first instruction:
//!
//! * `REQUEST_HANDLE` — receive-only on the launch-request endpoint;
//! * `APPROVAL_HANDLE` — send-only on the approval endpoint.
//!
//! It has no file, device, portal, or endpoint-creation capability. The
//! kernel's native-syscall allowlist for service tasks also rejects filesystem
//! and endpoint-creation calls, so the narrow capability set is enforced by
//! both the table contents and the syscall gate.
//!
//! This is not syscall remoting. The Linux syscall trap, ELF mapping, and
//! library loading remain in the kernel for this Gate 2 slice.

#![no_std]
#![no_main]

use aether_sdk as sdk;

/// First capability granted to this task: receive on the request endpoint.
///
/// Handles encode `(generation << 8) | slot`; the task starts with an empty
/// capability table, so the two installed capabilities are slots 0 and 1 at
/// generation 1. The constants are checked at boot by the kernel's grant
/// order.
const REQUEST_HANDLE: u64 = 0x100;
/// Second capability granted to this task: send on the approval endpoint.
const APPROVAL_HANDLE: u64 = 0x101;

const TAG_STATUS: u8 = 0;
const TAG_IDENTITY: u8 = 1;
const TAG_VERSION: u8 = 2;
const TAG_PATH: u8 = 3;

const STATUS_SIGNED: u8 = 1 << 0;
const STATUS_IMPORTED: u8 = 1 << 1;
const STATUS_RUNTIME_VERIFIED: u8 = 1 << 2;
const STATUS_REQUIRED: u8 = STATUS_SIGNED | STATUS_IMPORTED | STATUS_RUNTIME_VERIFIED;

const APPROVE: u8 = 1;
const DENY: u8 = 0;
const DENY_PATH: u8 = 1;
const DENY_IDENTITY: u8 = 2;
const DENY_PROTOCOL: u8 = 3;

const MAX_FIELD: usize = 63;
const MAX_ROUNDS: usize = 4;
const IDLE_LIMIT: usize = 200_000;

fn puts(text: &str) {
    let _ = sdk::print_str(text);
}

fn puts_field(label: &str, bytes: &[u8]) {
    puts(label);
    if let Ok(text) = core::str::from_utf8(bytes) {
        puts(text);
    } else {
        puts("<binary>");
    }
    puts("\n");
}

fn fail(label: &str) -> ! {
    puts("linuxd: ");
    puts(label);
    puts("\n");
    sdk::exit(1)
}

fn send_denial(reason: u8) {
    let message = [DENY, reason];
    if sdk::endpoint_send(APPROVAL_HANDLE, &message).is_err() {
        fail("approval endpoint send failed");
    }
}

fn send_approval() {
    let message = [APPROVE, STATUS_REQUIRED];
    if sdk::endpoint_send(APPROVAL_HANDLE, &message).is_err() {
        fail("approval endpoint send failed");
    }
}

/// Accepts only a non-reserved application identity.
///
/// The kernel sourced it from the signed record, but this service still
/// validates the shape because an identity is policy input, not an opaque
/// label. A leading dot, empty component, or reserved runtime name is denied.
fn valid_identity(value: &[u8]) -> bool {
    if value.is_empty()
        || value.len() > MAX_FIELD
        || value == b"org.aether.linux-runtime"
        || value.starts_with(b".")
        || value.ends_with(b".")
    {
        return false;
    }
    let mut dots = 0usize;
    for byte in value {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => {}
            b'.' => dots += 1,
            _ => return false,
        }
    }
    dots >= 1
}

/// Accepts a bounded numeric dotted version such as `1.0.0`.
fn valid_version(value: &[u8]) -> bool {
    if value.is_empty()
        || value.len() > MAX_FIELD
        || !value[0].is_ascii_digit()
        || !value[value.len() - 1].is_ascii_digit()
    {
        return false;
    }
    let mut dots = 0usize;
    for byte in value {
        match byte {
            b'0'..=b'9' => {}
            b'.' => dots += 1,
            _ => return false,
        }
    }
    (1..=3).contains(&dots)
}

/// Accepts an absolute, normalized path under a policy-selected directory.
///
/// This is a launch policy decision, not just syntax: the payload is already
/// inside the verified AppImage root, so the selected executable must be
/// absolute and under `/usr/bin/` or `/opt/`. `..`, `.`, empty components, and
/// non-UTF-8 bytes are denied.
fn valid_launch_path(value: &[u8]) -> bool {
    if value.is_empty()
        || value.len() > MAX_FIELD
        || value[0] != b'/'
        || !(value.starts_with(b"/usr/bin/") || value.starts_with(b"/opt/"))
        || core::str::from_utf8(value).is_err()
    {
        return false;
    }
    value
        .split(|byte| *byte == b'/')
        .skip(1)
        .all(|part| !part.is_empty() && part != b"." && part != b"..")
}

struct Request {
    status: Option<u8>,
    identity_len: usize,
    identity: [u8; MAX_FIELD],
    version_len: usize,
    version: [u8; MAX_FIELD],
    path_len: usize,
    path: [u8; MAX_FIELD],
    duplicate: bool,
}

impl Request {
    const fn new() -> Self {
        Self {
            status: None,
            identity_len: 0,
            identity: [0; MAX_FIELD],
            version_len: 0,
            version: [0; MAX_FIELD],
            path_len: 0,
            path: [0; MAX_FIELD],
            duplicate: false,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn record(&mut self, tag: u8, value: &[u8]) {
        if value.len() > MAX_FIELD || (tag != TAG_STATUS && value.is_empty()) {
            self.duplicate = true;
            return;
        }
        match tag {
            TAG_STATUS => {
                if self.status.is_some() || value.len() != 1 {
                    self.duplicate = true;
                } else {
                    self.status = Some(value[0]);
                }
            }
            TAG_IDENTITY => {
                if self.identity_len != 0 {
                    self.duplicate = true;
                } else {
                    self.identity[..value.len()].copy_from_slice(value);
                    self.identity_len = value.len();
                }
            }
            TAG_VERSION => {
                if self.version_len != 0 {
                    self.duplicate = true;
                } else {
                    self.version[..value.len()].copy_from_slice(value);
                    self.version_len = value.len();
                }
            }
            TAG_PATH => {
                if self.path_len != 0 {
                    self.duplicate = true;
                } else {
                    self.path[..value.len()].copy_from_slice(value);
                    self.path_len = value.len();
                }
            }
            _ => self.duplicate = true,
        }
    }

    fn complete(&self) -> bool {
        self.status.is_some()
            && self.identity_len != 0
            && self.version_len != 0
            && self.path_len != 0
    }

    fn identity(&self) -> &[u8] {
        &self.identity[..self.identity_len]
    }

    fn version(&self) -> &[u8] {
        &self.version[..self.version_len]
    }

    fn path(&self) -> &[u8] {
        &self.path[..self.path_len]
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    puts("AETHER_LINUXD_ENTRY service=linuxd\n");

    let mut buffer = [0u8; sdk::MAX_MESSAGE_BYTES];
    let mut request = Request::new();
    let mut rounds = 0usize;
    let mut idle = 0usize;

    loop {
        if rounds >= MAX_ROUNDS {
            fail("launch request round limit reached");
        }
        match sdk::endpoint_receive(REQUEST_HANDLE, &mut buffer) {
            Ok((length, transferred)) => {
                if transferred != 0 || length == 0 {
                    fail("malformed launch request envelope");
                }
                let tag = buffer[0];
                request.record(tag, &buffer[1..length]);
                if !request.complete() {
                    if idle > IDLE_LIMIT {
                        fail("launch request incomplete");
                    }
                    idle += 1;
                    let _ = sdk::sleep();
                    continue;
                }

                idle = 0;
                rounds += 1;
                let status = request.status.unwrap_or(0);
                if request.duplicate || status & STATUS_REQUIRED != STATUS_REQUIRED {
                    send_denial(DENY_PROTOCOL);
                    puts("linuxd: launch request denied reason=status\n");
                    request.reset();
                    continue;
                }
                if !valid_identity(request.identity()) {
                    puts_field("linuxd: rejected identity ", request.identity());
                    send_denial(DENY_IDENTITY);
                    request.reset();
                    continue;
                }
                if !valid_version(request.version()) {
                    puts_field("linuxd: rejected version ", request.version());
                    send_denial(DENY_PROTOCOL);
                    request.reset();
                    continue;
                }
                if !valid_launch_path(request.path()) {
                    puts_field("linuxd: rejected launch path ", request.path());
                    send_denial(DENY_PATH);
                    request.reset();
                    continue;
                }

                puts_field("linuxd: approved identity ", request.identity());
                puts_field("linuxd: approved version ", request.version());
                puts_field("linuxd: approved path ", request.path());
                send_approval();
                puts("linuxd: approval sent; exiting\n");
                sdk::exit(0)
            }
            Err(_) => {
                idle += 1;
                if idle > IDLE_LIMIT {
                    fail("no launch request arrived");
                }
                let _ = sdk::sleep();
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    puts("AETHER_LINUXD_PANIC\n");
    sdk::exit(1)
}
