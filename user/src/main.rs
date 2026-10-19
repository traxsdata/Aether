// SPDX-License-Identifier: Apache-2.0
//! Aether native user-space init.
//!
//! This program runs at CPL3 in its own address space. It is intentionally
//! ordinary application code: it reaches the kernel only through the SDK's
//! system call wrappers and has no access to kernel memory. The boot self-test
//! uses it to demonstrate that the ring-3 boundary, syscall validation, and
//! private application storage work together.

#![no_std]
#![no_main]

use aether_sdk as sdk;

const PRIVATE_NOTE: &str = "/private/notes.txt";
const OTHER_APP_SECRET: &str = "../other/private/secret.txt";
const ESCAPE_ATTEMPT: &str = "../../system/policy.txt";

fn puts(text: &str) {
    let _ = sdk::print_str(text);
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
                Ok(count) if count == 18 => puts("init: private write ok\n"),
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

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    puts("AETHER_APP_ENTRY cpl=3\n");
    puts("init: pid=");
    puts_u64(sdk::pid());
    puts("\n");

    write_note();
    print_file(PRIVATE_NOTE);
    report_open("init: own-private", PRIVATE_NOTE, sdk::READ);
    report_open("init: sibling-private", OTHER_APP_SECRET, sdk::READ);
    report_open("init: root-escape", ESCAPE_ATTEMPT, sdk::READ);

    // A well-formed syscall with an out-of-window pointer must be rejected by
    // the kernel before it is dereferenced.
    let bad = unsafe { sdk::syscall4(2, 1, 0x0000_0000_0000_1000, 8, 0) };
    puts("init: kernel-pointer-syscall=");
    puts_i64(bad);
    puts("\n");

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
