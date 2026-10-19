// SPDX-License-Identifier: Apache-2.0

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let kernel = env::var_os("AETHER_KERNEL_ELF")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../build/aether-kernel.elf"));
    let kernel = kernel.canonicalize().unwrap_or(kernel);
    println!("cargo:rustc-env=AETHER_KERNEL_ELF={}", kernel.display());
    println!("cargo:rerun-if-changed={}", kernel.display());
    println!("cargo:rerun-if-env-changed=AETHER_KERNEL_ELF");

    // UEFI applications are PE32+ images. rust-lld sets most of this up for the
    // x86_64-unknown-uefi target; the subsystem and entry point are explicit so
    // firmware can load the image without a boot manager.
    println!("cargo:rustc-link-arg=/subsystem:efi_application");
    println!("cargo:rustc-link-arg=/entry:efi_main");
    println!("cargo:rustc-link-arg=/fixed:no");
    println!("cargo:rustc-link-arg=/dynamicbase");
}
