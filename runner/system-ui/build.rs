//! Build the kernel's own software drawing code (common/gfx.c) into a static
//! library for Rust system apps. This is the same code the native C apps use,
//! including Facet, the project's own font. No third-party code is involved.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap_or_else(|e| panic!("can't run {:?}: {e}", cmd));
    if !status.success() {
        panic!("{:?} failed with {status}", cmd);
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../..").canonicalize().expect("repo root");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let shim = manifest.join("csrc/gfx_shim.c");
    let gfx = root.join("common/gfx.c");
    let include = root.join("include");
    let common = root.join("common");

    for f in [&shim, &gfx] {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    println!("cargo:rerun-if-changed={}", include.join("font_data.h").display());

    let mut objects = Vec::new();
    for src in [&shim, &gfx] {
        let obj = out.join(format!(
            "{}.o",
            Path::new(src).file_stem().unwrap().to_string_lossy()
        ));
        run(Command::new("clang")
            .args(["-c", "-O2", "-ffreestanding", "-fno-builtin", "-fno-stack-protector", "-fPIC"])
            .arg("-I")
            .arg(&include)
            .arg("-I")
            .arg(&common)
            .arg("-o")
            .arg(&obj)
            .arg(src));
        objects.push(obj);
    }

    let lib = out.join("libaether_system_ui.a");
    let _ = std::fs::remove_file(&lib);
    let mut ar = Command::new("ar");
    ar.arg("crs").arg(&lib);
    for o in &objects {
        ar.arg(o);
    }
    run(&mut ar);

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=aether_system_ui");
}
