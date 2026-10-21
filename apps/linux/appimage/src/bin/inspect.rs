// SPDX-License-Identifier: Apache-2.0
//! Host diagnostic for an AppImage container.
//!
//! This tool never executes the AppImage. It validates the container with the
//! same parser the runtime will use and prints the metadata needed by the
//! import stage.

use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: appimage-inspect <file.AppImage> [sha256-hex]");
        return ExitCode::from(2);
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("appimage-inspect: {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let expected = env::args().nth(2);
    let checked = expected
        .as_ref()
        .and_then(|hex| aether_appimage::Digest::from_hex(hex.as_bytes()));
    if expected.is_some() && checked.is_none() {
        eprintln!("appimage-inspect: expected digest must be 64 hexadecimal characters");
        return ExitCode::from(2);
    }
    let result = match checked {
        Some(digest) => aether_appimage::parse_verified(&bytes, digest),
        None => aether_appimage::parse(&bytes),
    };
    match result {
        Ok(info) => {
            println!("file={path}");
            println!("bytes={}", bytes.len());
            println!("appimage=type-2");
            if expected.is_some() {
                println!("digest=verified");
            }
            println!("runtime={:?}", info.runtime);
            println!("squashfs_offset={}", info.squashfs_offset);
            println!("squashfs_bytes={}", info.squashfs_bytes);
            println!("squashfs_block_size={}", info.block_size);
            println!("squashfs_compression={}", info.compression.name());
            println!("squashfs_inodes={}", info.inodes);
            println!("squashfs_fragments={}", info.fragments);
            println!("squashfs_flags={:#06x}", info.flags);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("appimage-inspect: {path}: {error:?}");
            ExitCode::FAILURE
        }
    }
}
