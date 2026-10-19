// SPDX-License-Identifier: Apache-2.0

use std::env;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    println!("cargo:rustc-link-arg=-T{manifest}/kernel.ld");
    println!("cargo:rustc-link-arg=--build-id=none");
    println!("cargo:rerun-if-changed=kernel.ld");
    println!("cargo:rerun-if-env-changed=AETHER_GIT_COMMIT");
}
