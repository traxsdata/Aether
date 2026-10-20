// SPDX-License-Identifier: Apache-2.0
//! Host tests for the SquashFS reader.

use crate::{Error, Image, Kind, Limits};

const IMAGE: &[u8] = include_bytes!("../tests/fixtures/root.sqfs");

fn entry_names(image: &Image, path: &str) -> Vec<String> {
    let inode = find(image, path).expect("inode");
    image
        .list(&inode)
        .expect("list")
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

fn find(image: &Image, path: &str) -> Option<crate::Inode> {
    image.lookup(path).ok()
}

#[test]
fn reads_the_root_directory() {
    let image = Image::from_squashfs(IMAGE).expect("parse");
    let mut names = entry_names(&image, "");
    names.sort();
    assert_eq!(names, ["etc", "hello.txt", "lib", "link"]);
}

#[test]
fn reads_regular_files_and_fragments() {
    let image = Image::from_squashfs(IMAGE).expect("parse");
    let hello = find(&image, "hello.txt").expect("hello");
    assert_eq!(image.read_file(&hello).unwrap(), b"hello from squashfs\n");

    let motd = find(&image, "etc/motd").expect("motd");
    assert_eq!(
        image.read_file(&motd).unwrap(),
        b"AETHER_SQUASHFS_OK source=/etc/motd\n"
    );
}

#[test]
fn reads_a_symlink_inode() {
    let image = Image::from_squashfs(IMAGE).expect("parse");
    let link = find(&image, "link").expect("link");
    match link.kind {
        Kind::Symlink(target) => assert_eq!(target, b"etc/motd"),
        other => panic!("expected symlink, got {other:?}"),
    }
}

#[test]
fn extracts_the_tree() {
    let image = Image::from_squashfs(IMAGE).expect("parse");
    let files = image.extract(Limits::default()).expect("extract");
    let mut names: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
    names.sort();
    assert_eq!(names, ["etc/motd", "hello.txt"]);
    assert!(
        files
            .iter()
            .any(|file| file.path == "hello.txt" && file.data == b"hello from squashfs\n")
    );
}

#[test]
fn enforces_extraction_limits() {
    let image = Image::from_squashfs(IMAGE).expect("parse");
    let limits = Limits {
        max_bytes: 1,
        ..Limits::default()
    };
    assert_eq!(image.extract(limits).unwrap_err(), Error::Limit);

    let limits = Limits {
        max_entries: 1,
        ..Limits::default()
    };
    assert_eq!(image.extract(limits).unwrap_err(), Error::Limit);
}

#[test]
fn reads_an_appimage_with_a_container_offset() {
    let mut container = vec![0u8; 1024];
    container[..4].copy_from_slice(b"\x7fELF");
    container[4] = 2;
    container[5] = 1;
    container[6] = 1;
    container[16..18].copy_from_slice(&3u16.to_le_bytes());
    container[18..20].copy_from_slice(&62u16.to_le_bytes());
    container[8..11].copy_from_slice(b"AI\x02");
    container.extend_from_slice(IMAGE);
    let image = Image::from_appimage(&container).expect("appimage");
    assert_eq!(image.offset(), 1024);
    assert_eq!(
        image
            .read_file(&find(&image, "hello.txt").unwrap())
            .unwrap(),
        b"hello from squashfs\n"
    );
}

#[test]
fn rejects_non_squashfs_input() {
    assert!(Image::from_squashfs(b"not a filesystem").is_err());
    assert!(Image::from_squashfs(&[0u8; 96]).is_err());
}
