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
        eprintln!("usage: appimage-inspect <file.AppImage>");
        return ExitCode::from(2);
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("appimage-inspect: {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    match aether_appimage::parse(&bytes) {
        Ok(info) => {
            println!("file={path}");
            println!("bytes={}", bytes.len());
            println!("appimage=type-2");
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
