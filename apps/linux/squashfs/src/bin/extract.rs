// SPDX-License-Identifier: Apache-2.0
//! Host extractor for a SquashFS or AppImage filesystem.
//!
//! This is import tooling, not the in-OS mount path. It validates the tree and
//! writes only regular files beneath the destination directory.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use aether_squashfs::{Image, Limits};

fn main() -> ExitCode {
    let Some(input) = env::args().nth(1) else {
        eprintln!("usage: squashfs-extract <file.AppImage|file.squashfs> <output-dir>");
        return ExitCode::from(2);
    };
    let Some(output) = env::args().nth(2) else {
        eprintln!("usage: squashfs-extract <file.AppImage|file.squashfs> <output-dir>");
        return ExitCode::from(2);
    };
    let bytes = match fs::read(&input) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("squashfs-extract: {input}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let image = if bytes.starts_with(b"hsqs") {
        Image::from_squashfs(&bytes)
    } else {
        Image::from_appimage(&bytes)
    };
    let image = match image {
        Ok(image) => image,
        Err(error) => {
            eprintln!("squashfs-extract: {input}: {error:?}");
            return ExitCode::FAILURE;
        }
    };
    let files = match image.extract(Limits::default()) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("squashfs-extract: extract: {error:?}");
            return ExitCode::FAILURE;
        }
    };
    let root = PathBuf::from(output);
    let mut written = 0usize;
    for file in &files {
        let relative = Path::new(&file.path);
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            eprintln!("squashfs-extract: unsafe path {}", file.path);
            return ExitCode::FAILURE;
        }
        let destination = root.join(relative);
        if let Some(parent) = destination.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            eprintln!("squashfs-extract: {}: {error}", parent.display());
            return ExitCode::FAILURE;
        }
        if let Err(error) = fs::write(&destination, &file.data) {
            eprintln!("squashfs-extract: {}: {error}", destination.display());
            return ExitCode::FAILURE;
        }
        written += 1;
    }
    println!(
        "extracted files={} bytes={} root={}",
        written,
        files.iter().map(|file| file.data.len()).sum::<usize>(),
        root.display()
    );
    ExitCode::SUCCESS
}
