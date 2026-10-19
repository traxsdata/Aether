// SPDX-License-Identifier: Apache-2.0

use std::env;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    println!("cargo:rustc-link-arg=-T{manifest}/kernel.ld");
    println!("cargo:rustc-link-arg=--build-id=none");
    println!("cargo:rerun-if-changed=kernel.ld");
    println!("cargo:rerun-if-env-changed=AETHER_GIT_COMMIT");

    // The native user program is embedded in the kernel so the boot image and
    // the user ABI cannot drift apart. The Makefile builds it first and passes
    // an absolute path; direct cargo builds fall back to the conventional
    // build directory.
    let user_elf = env::var("AETHER_USER_ELF").unwrap_or_else(|_| {
        let root = std::path::Path::new(&manifest)
            .parent()
            .expect("kernel manifest parent")
            .to_path_buf();
        root.join("build/aether-init.elf")
            .to_string_lossy()
            .into_owned()
    });
    println!("cargo:rustc-env=AETHER_USER_ELF={user_elf}");
    println!("cargo:rerun-if-env-changed=AETHER_USER_ELF");
    if std::path::Path::new(&user_elf).exists() {
        println!("cargo:rerun-if-changed={user_elf}");
    }
}
