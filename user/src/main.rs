// SPDX-License-Identifier: Apache-2.0
//! Aether native user-space init.
//!
//! This program runs at CPL3 in its own address space. It is intentionally
//! ordinary application code: it reaches the kernel only through the SDK's
//! system call wrappers and has no access to kernel memory. The boot
//! self-test uses it to demonstrate, from inside the sandbox, that
//!
//! * ring-3 system calls are gated and validated (`yield`, `sleep`, and a
//!   well-formed call with a kernel pointer),
//! * IPC endpoints work from user mode and every denial reason in the
//!   system ABI specification is actually produced by the kernel instead of
//!   silently succeeding,
//! * the filesystem sandbox denies escape attempts, including listing and
//!   statting outside the task root,
//! * the verified package's open-file limit is enforced,
//! * and portal grants authorize exactly the seeded file and no more.
//!
//! Every printed denial is kernel-enforced evidence. The boot gate in the
//! scheduler counts these attempts, and a task that claims success without
//! producing the evidence fails the boot. The kernel owns the
//! `AETHER_IPC_USER_OK` and `AETHER_SANDBOX_OK` markers; this program never
//! prints them.

#![no_std]
#![no_main]

use aether_sdk as sdk;

/// System call number for `EndpointSend`, from
/// `docs/specifications/system-abi.md`.
///
/// The SDK wrapper rejects the too-long payload in user mode before the
/// kernel sees it. To prove that the kernel itself re-checks the length
/// across the boundary, this one case uses the raw entry.
const SYSCALL_ENDPOINT_SEND: u64 = 16;

const PRIVATE_NOTE: &str = "/private/notes.txt";
const OTHER_APP_SECRET: &str = "../other/private/secret.txt";
const ESCAPE_ATTEMPT: &str = "../../system/policy.txt";

/// Held files for the open-file quota check; the fifth open must fail. The
/// demo manifest allows four concurrent open files.
const HOLD_PATHS: [&str; 4] = [
    "/private/hold-0.txt",
    "/private/hold-1.txt",
    "/private/hold-2.txt",
    "/private/hold-3.txt",
];

fn puts(text: &str) {
    let _ = sdk::print_str(text);
}

/// Reports one failing guard and exits nonzero. The scheduler retires the
/// task, the user counters then fall short of the boot gate, and the boot
/// reports `AETHER_BOOT_FAILED` instead of printing a success marker.
fn fail(detail: &str) -> ! {
    puts("init: UNEXPECTED ");
    puts(detail);
    puts("\n");
    sdk::exit(1)
}

/// Requires a kernel denial and names it on the console.
fn require_denial(label: &str, error: i64, expected: &str) {
    let name = sdk::error_name(error);
    if name == expected {
        puts(label);
        puts(" ");
        puts(expected);
        puts("\n");
    } else {
        puts(label);
        puts(": expected ");
        puts(expected);
        puts(" got ");
        puts(name);
        fail("denial did not match the expected error");
    }
}

fn report_open(label: &str, path: &str, rights: u64) {
    match sdk::open(path, rights) {
        Ok(handle) => {
            puts(label);
            puts(": opened\n");
            let _ = sdk::close(handle);
        }
        Err(error) => {
            puts(label);
            puts(": denied ");
            puts(sdk::error_name(error));
            puts("\n");
        }
    }
}

fn report_portal(label: &str, path: &str, rights: u64) {
    match sdk::portal_open(path, rights) {
        Ok(handle) => {
            puts(label);
            puts(": unexpectedly opened\n");
            let _ = sdk::close(handle);
        }
        Err(error) => {
            puts(label);
            puts(": denied ");
            puts(sdk::error_name(error));
            puts("\n");
        }
    }
}

fn print_file(path: &str) {
    let mut buffer = [0u8; 128];
    match sdk::open(path, sdk::READ) {
        Ok(handle) => {
            if let Ok(count) = sdk::read(handle, &mut buffer) {
                puts("init: note=");
                let text = core::str::from_utf8(&buffer[..count]).unwrap_or("<binary>");
                puts(text.trim_end());
                puts("\n");
            }
            let _ = sdk::close(handle);
        }
        Err(error) => {
            puts("init: read failed ");
            puts(sdk::error_name(error));
            puts("\n");
        }
    }
}

fn write_note() {
    match sdk::open(PRIVATE_NOTE, sdk::WRITE | sdk::CREATE) {
        Ok(handle) => {
            match sdk::write_file(handle, b"hello from ring 3\n") {
                Ok(18) => puts("init: private write ok\n"),
                Ok(_) => puts("init: private write truncated\n"),
                Err(error) => {
                    puts("init: private write failed ");
                    puts(sdk::error_name(error));
                    puts("\n");
                }
            }
            let _ = sdk::close(handle);
        }
        Err(error) => {
            puts("init: private open failed ");
            puts(sdk::error_name(error));
            puts("\n");
        }
    }
}

/// Exercises the ring-3 IPC contract end to end.
///
/// The endpoint pool holds eight objects: the kernel's workers own two and
/// the ring-3 `linuxd` policy channel owns two. This task can create the
/// remaining four; the next create is refused. Positive and negative paths
/// alternate so the kernel's denial counters are driven from user mode,
/// matching the boot gate.
fn ipc_section() {
    let Ok(first) = sdk::endpoint_create() else {
        fail("first endpoint create was refused");
    };
    let Ok(second) = sdk::endpoint_create() else {
        fail("second endpoint create was refused");
    };
    for _ in 0..2 {
        if sdk::endpoint_create().is_err() {
            fail("reserve endpoint create was refused");
        }
    }

    // A message round trip.
    if sdk::endpoint_send(first, b"hello").is_err() {
        fail("hello send failed");
    }
    let mut message = [0u8; sdk::MAX_MESSAGE_BYTES];
    match sdk::endpoint_receive(first, &mut message) {
        Ok((length, transferred))
            if length == b"hello".len() && &message[..length] == b"hello" && transferred == 0 =>
        {
            puts("init: ipc round trip ok\n");
        }
        Ok((_, _)) => fail("hello round trip payload mismatch"),
        Err(error) => {
            puts("init: hello receive failed ");
            puts(sdk::error_name(error));
            fail("hello round trip");
        }
    }

    // The kernel must re-check the payload length across the boundary even
    // though the SDK rejects it earlier; the raw entry proves it.
    let oversized = [0u8; sdk::MAX_MESSAGE_BYTES + 1];
    // SAFETY: `oversized` is a readable buffer in this task's address space
    // and the kernel validates the range before it acts on the length.
    let raw = unsafe {
        sdk::syscall4(
            SYSCALL_ENDPOINT_SEND,
            first,
            oversized.as_ptr() as u64,
            oversized.len() as u64,
            0,
        )
    };
    require_denial("init: ipc-too-long", raw, "EINVAL");

    // A well-formed handle that this task does not own must be rejected as
    // an invalid handle, never treated as a fresh grant.
    let forged = (0x5au64 << 8) | (first & 0xff);
    match sdk::endpoint_send(forged, b"pray") {
        Err(error) => require_denial("init: ipc-forged", error, "EBADF"),
        Ok(()) => fail("forged handle was accepted"),
    }

    // Queue capacity: four queued messages, the fifth must fail.
    let fills: [&[u8]; 4] = [b"fill-one", b"fill-two", b"fill-three", b"fill-four"];
    for fill in fills {
        if sdk::endpoint_send(first, fill).is_err() {
            fail("queue fill send failed");
        }
    }
    match sdk::endpoint_send(first, b"fifth") {
        Err(error) => require_denial("init: ipc-queue-full", error, "EAGAIN"),
        Ok(()) => fail("queue accepted a fifth message"),
    }

    // Drain exactly what was queued.
    for fill in fills {
        match sdk::endpoint_receive(first, &mut message) {
            Ok((length, _)) if &message[..length] == fill => {}
            Ok((_, _)) => fail("drained payload mismatch"),
            Err(error) => {
                puts("init: drain receive failed ");
                puts(sdk::error_name(error));
                fail("queue drain");
            }
        }
    }

    // An empty queue must report "try again", not pretend to succeed.
    match sdk::endpoint_receive(first, &mut message) {
        Err(error) => require_denial("init: ipc-drain-empty", error, "EAGAIN"),
        Ok((_, _)) => fail("empty queue delivered a message"),
    }

    // Capability transfer: `second`'s capability is carried through
    // `first`. Only the kernel mints capabilities, so the received copy
    // must have its `TRANSFER` right stripped while keeping the rights it
    // legitimately carries.
    if sdk::endpoint_send_with_transfer(first, b"cap", second).is_err() {
        fail("capability carry send failed");
    }
    let carried = match sdk::endpoint_receive(first, &mut message) {
        Ok((length, transferred)) if length == b"cap".len() && &message[..length] == b"cap" => {
            transferred
        }
        Ok((_, _)) => fail("carry round trip payload mismatch"),
        Err(error) => {
            puts("init: carry receive failed ");
            puts(sdk::error_name(error));
            fail("capability carry");
        }
    };
    if carried == 0 {
        fail("carry round trip did not transfer a capability");
    }
    match sdk::endpoint_send_with_transfer(first, b"x", carried) {
        Err(error) => require_denial("init: ipc-retransfer", error, "EACCES"),
        Ok(()) => fail("transferred capability kept its TRANSFER right"),
    }

    // The received copy still carries SEND and RECEIVE for its own object.
    if sdk::endpoint_send(carried, b"via-ep2").is_err() {
        fail("send via transferred capability failed");
    }
    match sdk::endpoint_receive(carried, &mut message) {
        Ok((length, 0)) if &message[..length] == b"via-ep2" => {
            puts("init: ipc transfer chain ok\n");
        }
        Ok((_, _)) => fail("via-ep2 round trip payload mismatch"),
        Err(error) => {
            puts("init: via-ep2 receive failed ");
            puts(sdk::error_name(error));
            fail("capability transfer chain");
        }
    }

    // Pool exhaustion: two objects belong to the workers, two to `linuxd`,
    // and four were created above, so the next create is refused instead of
    // growing kernel memory.
    match sdk::endpoint_create() {
        Err(error) => require_denial("init: ipc-create-limit", error, "ENOSPC"),
        Ok(_) => fail("endpoint pool grew past its bound"),
    }

    // Blocking receive with a deadline: an empty queue must yield the CPU and
    // return ETIMEDOUT only after the requested ticks have elapsed. This is
    // the blocking path, not a user-space spin loop.
    let before = sdk::uptime();
    match sdk::endpoint_receive_timeout(second, &mut message, 5) {
        Err(error) => require_denial("init: ipc-timeout", error, "ETIMEDOUT"),
        Ok((_, _)) => fail("blocking receive returned a message from an empty queue"),
    }
    if sdk::uptime().saturating_sub(before) < 5 {
        fail("blocking receive did not wait for the timeout");
    }
    puts("init: ipc blocking timeout ok\n");

    // Closing the owner's endpoint destroys the object and revokes every alias
    // in this table. The old handle must fail as stale, and the pool slot must
    // become available again.
    if sdk::endpoint_close(first).is_err() {
        fail("endpoint close failed");
    }
    match sdk::endpoint_send(first, b"stale") {
        Err(error) => require_denial("init: ipc-closed", error, "EBADF"),
        Ok(()) => fail("a closed endpoint handle was accepted"),
    }
    match sdk::endpoint_create() {
        Ok(replacement) => {
            if sdk::endpoint_send(replacement, b"reused").is_err() {
                fail("send on recreated endpoint failed");
            }
            match sdk::endpoint_receive_timeout(replacement, &mut message, 10) {
                Ok((length, 0)) if &message[..length] == b"reused" => {
                    puts("init: ipc endpoint lifetime ok\n")
                }
                Ok((_, _)) => fail("recreated endpoint payload mismatch"),
                Err(error) => {
                    puts("init: recreated receive failed ");
                    puts(sdk::error_name(error));
                    fail("recreated endpoint receive");
                }
            }
        }
        Err(error) => {
            puts("init: endpoint slot was not reclaimed ");
            puts(sdk::error_name(error));
            fail("endpoint pool lifetime");
        }
    }
}

/// Exercises the filesystem sandbox and the verified package's limits.
fn fs_section() {
    write_note();
    print_file(PRIVATE_NOTE);
    report_open("init: own-private", PRIVATE_NOTE, sdk::READ);
    report_open("init: sibling-private", OTHER_APP_SECRET, sdk::READ);
    report_open("init: root-escape", ESCAPE_ATTEMPT, sdk::READ);

    // A file capability must never authorize endpoint operations, no
    // matter what numeric value its handle has.
    let Ok(file_handle) = sdk::open(PRIVATE_NOTE, sdk::READ) else {
        fail("private note open failed");
    };
    match sdk::endpoint_send(file_handle, b"x") {
        Err(error) => require_denial("init: ipc-wrong-type", error, "EBADF"),
        Ok(()) => fail("a file capability was accepted as an endpoint"),
    }
    if sdk::close(file_handle).is_err() {
        fail("private note close failed");
    }

    // Listing and statting outside the task root must be denied for the
    // same reason `open` denies them; the rule is the root scope, not the
    // existence of the target.
    let mut listing = [0u8; 256];
    match sdk::list("../other", &mut listing) {
        Err(error) => require_denial("init: sibling-list", error, "EPERM"),
        Ok(_) => fail("a sibling directory was listed"),
    }
    match sdk::file_size("../other") {
        Err(error) => require_denial("init: sibling-stat", error, "EPERM"),
        Ok(_) => fail("a sibling path was statted"),
    }

    // Verified manifest limit: four concurrently open files are allowed, so
    // a fifth open must fail with ENOSPC. Holding four files first makes the
    // limit observable; the denied fifth path is an existing private file so
    // the failure comes from the limit, not from a lookup failure. The
    // kernel records it in its quota counter too.
    let mut handles = [0u64; HOLD_PATHS.len()];
    for index in 0..handles.len() {
        match sdk::open(HOLD_PATHS[index], sdk::WRITE | sdk::CREATE) {
            Ok(handle) => handles[index] = handle,
            Err(error) => {
                puts("init: hold open failed ");
                puts(sdk::error_name(error));
                fail("concurrent open setup");
            }
        }
    }
    match sdk::open(PRIVATE_NOTE, sdk::READ) {
        Err(error) => require_denial("init: open-file-limit", error, "ENOSPC"),
        Ok(handle) => {
            let _ = sdk::close(handle);
            fail("the fifth open succeeded")
        }
    }
    for handle in handles {
        if sdk::close(handle).is_err() {
            fail("hold close failed");
        }
    }
    puts("init: open-files ok\n");
}

/// Exercises the portal contract from inside the sandbox.
fn portal_section() {
    match sdk::portal_open("/home/welcome.txt", sdk::READ) {
        Ok(handle) => {
            let mut buffer = [0u8; 64];
            if let Ok(count) = sdk::read(handle, &mut buffer) {
                puts("init: portal=");
                puts(
                    core::str::from_utf8(&buffer[..count])
                        .unwrap_or("?")
                        .trim_end(),
                );
                puts("\n");
                puts("AETHER_PORTAL_OK\n");
            }
            let _ = sdk::close(handle);
        }
        Err(error) => {
            puts("init: portal denied ");
            puts(sdk::error_name(error));
            puts("\n");
        }
    }

    // A grant for one file must not imply access to its parent directory
    // or a sibling: the grant is the whole authority. Both must fail with
    // ENOGRANT.
    report_portal("init: portal-parent", "/home", sdk::READ);
    report_portal("init: portal-sibling", "/home/other.txt", sdk::READ);
    // The seeded grant is READ; a write attempt through the same grant must
    // also fail with ENOGRANT because the grant is rights-aware too.
    report_portal("init: portal-write", "/home/welcome.txt", sdk::WRITE);

    // Asking for a new file creates a pending consent record, not authority.
    // The desktop must approve it before the path becomes openable; this
    // request intentionally stays pending for an interactive review.
    match sdk::portal_request("/home/import.txt", sdk::READ) {
        Ok(index) => {
            puts("init: portal request pending=");
            puts_u64(index as u64);
            puts("\n");
        }
        Err(error) => {
            puts("init: portal request failed ");
            puts(sdk::error_name(error));
            puts("\n");
        }
    }
    report_portal("init: portal-unapproved", "/home/import.txt", sdk::READ);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    puts("AETHER_APP_ENTRY cpl=3\n");
    puts("init: pid=");
    puts_u64(sdk::pid());
    puts("\n");

    // Yield and sleep must enter the scheduler and return only after another
    // task has had a chance to run (at minimum, after the next timer tick).
    let before_yield = sdk::uptime();
    let _ = sdk::yield_now();
    if sdk::uptime() <= before_yield {
        fail("yield did not schedule");
    }
    let before_sleep = sdk::uptime();
    let _ = sdk::sleep(1);
    if sdk::uptime() <= before_sleep {
        fail("sleep did not schedule");
    }
    puts("init: yield/sleep ok\n");

    ipc_section();
    fs_section();

    // A well-formed syscall with an out-of-window pointer must be rejected
    // by the kernel before it is dereferenced.
    let bad = unsafe { sdk::syscall4(2, 1, 0x0000_0000_0000_1000, 8, 0) };
    puts("init: kernel-pointer-syscall=");
    puts_i64(bad);
    puts("\n");

    portal_section();

    puts("AETHER_APP_OK\n");
    sdk::exit(0)
}

fn puts_u64(mut value: u64) {
    let mut buffer = [0u8; 20];
    let mut index = buffer.len();
    if value == 0 {
        puts("0");
        return;
    }
    while value > 0 {
        index -= 1;
        buffer[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    if let Ok(text) = core::str::from_utf8(&buffer[index..]) {
        puts(text);
    }
}

fn puts_i64(value: i64) {
    if value < 0 {
        puts("-");
        puts_u64(value.unsigned_abs());
    } else {
        puts_u64(value as u64);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    puts("AETHER_APP_PANIC\n");
    sdk::exit(1)
}
