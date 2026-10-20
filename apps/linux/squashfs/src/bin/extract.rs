// SPDX-License-Identifier: Apache-2.0
//! Host extractor for a SquashFS or AppImage filesystem.
//!
//! This is import tooling, not the in-OS mount path. It validates the tree and
//! writes only regular files beneath the destination directory.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use aether_squashfs::{Image, Limits, TreeKind};

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
    let tree = match image.import(Limits::default()) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("squashfs-extract: import: {error:?}");
            return ExitCode::FAILURE;
        }
    };
    let root = PathBuf::from(output);
    for entry in &tree.entries {
        let relative = Path::new(&entry.path);
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            eprintln!("squashfs-extract: unsafe path {}", entry.path);
            return ExitCode::FAILURE;
        }
        if let TreeKind::Directory = entry.kind {
            let destination = root.join(relative);
            if let Err(error) = fs::create_dir_all(&destination) {
                eprintln!("squashfs-extract: {}: {error}", destination.display());
                return ExitCode::FAILURE;
            }
            let _ = fs::set_permissions(&destination, fs::Permissions::from_mode(0o755));
        }
    }
    for entry in &tree.entries {
        if let TreeKind::File(data) = &entry.kind {
            let destination = root.join(Path::new(&entry.path));
            if let Some(parent) = destination.parent()
                && let Err(error) = fs::create_dir_all(parent)
            {
                eprintln!("squashfs-extract: {}: {error}", parent.display());
                return ExitCode::FAILURE;
            }
            if let Err(error) = fs::write(&destination, data) {
                eprintln!("squashfs-extract: {}: {error}", destination.display());
                return ExitCode::FAILURE;
            }
            let mode = if entry.mode & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            let _ = fs::set_permissions(&destination, fs::Permissions::from_mode(mode));
        }
    }
    for entry in &tree.entries {
        if let TreeKind::Symlink(target) = &entry.kind {
            let Some(target) = core::str::from_utf8(target).ok() else {
                eprintln!("squashfs-extract: non-UTF-8 symlink {}", entry.path);
                return ExitCode::FAILURE;
            };
            let destination = root.join(Path::new(&entry.path));
            if let Some(parent) = destination.parent()
                && let Err(error) = fs::create_dir_all(parent)
            {
                eprintln!("squashfs-extract: {}: {error}", parent.display());
                return ExitCode::FAILURE;
            }
            if let Err(error) = std::os::unix::fs::symlink(target, &destination) {
                eprintln!("squashfs-extract: {}: {error}", destination.display());
                return ExitCode::FAILURE;
            }
        }
    }
    println!(
        "extracted files={} directories={} symlinks={} hardlinks={} xattrs={} bytes={} root={}",
        tree.files,
        tree.directories,
        tree.symlinks,
        tree.hardlinks,
        tree.xattrs,
        tree.entries
            .iter()
            .map(|entry| match &entry.kind {
                TreeKind::File(data) => data.len(),
                _ => 0,
            })
            .sum::<usize>(),
        root.display()
    );
    ExitCode::SUCCESS
}
