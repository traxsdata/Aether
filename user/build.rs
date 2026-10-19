// SPDX-License-Identifier: Apache-2.0

use std::env;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    println!("cargo:rustc-link-arg=-T{manifest}/user.ld");
    println!("cargo:rustc-link-arg=--build-id=none");
    println!("cargo:rerun-if-changed=user.ld");
}
