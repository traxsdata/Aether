// SPDX-License-Identifier: Apache-2.0

use std::env;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    println!("cargo:rustc-link-arg=-T{manifest}/kernel.ld");
    println!("cargo:rustc-link-arg=--build-id=none");
    println!("cargo:rerun-if-changed=kernel.ld");
    println!("cargo:rerun-if-env-changed=AETHER_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=AETHER_NET_TEST_PORT");

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

    // One trust root for package, update, and Linux runtime verification.
    // `AETHER_TRUST_PUBLIC_KEY_FILE` names a hex Ed25519 public key; without it
    // the kernel trusts the checked-in test key, whose private seed is public.
    // The kernel reports which one it embedded so a release cannot ship the
    // test key silently.
    let root = std::path::Path::new(&manifest)
        .parent()
        .expect("kernel manifest parent")
        .to_path_buf();
    let test_key_path = root.join("tests/fixtures/keys/test.pub");
    let key_path = env::var("AETHER_TRUST_PUBLIC_KEY_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| test_key_path.clone());
    let key = std::fs::read_to_string(&key_path)
        .unwrap_or_else(|error| panic!("read trust key {}: {error}", key_path.display()));
    let key = key.trim().to_ascii_lowercase();
    assert!(
        key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "trust key {} is not 32-byte hex",
        key_path.display()
    );
    let test_key = std::fs::read_to_string(&test_key_path)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    println!("cargo:rustc-env=AETHER_TRUST_PUBLIC_KEY={key}");
    println!(
        "cargo:rustc-env=AETHER_TRUST_IS_TEST_KEY={}",
        u8::from(key == test_key)
    );
    println!("cargo:rerun-if-env-changed=AETHER_TRUST_PUBLIC_KEY_FILE");
    println!("cargo:rerun-if-changed={}", key_path.display());
    println!("cargo:rerun-if-changed={}", test_key_path.display());
}
