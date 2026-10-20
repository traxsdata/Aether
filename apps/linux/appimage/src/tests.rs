// SPDX-License-Identifier: Apache-2.0
//! Host tests for the AppImage container inspector.

use crate::{Compression, Error, RuntimeElf, parse};

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn elf_header(bytes: &mut [u8], kind: u16) {
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2; // ELFCLASS64
    bytes[5] = 1; // little endian
    bytes[6] = 1; // EV_CURRENT
    put_u16(bytes, 16, kind);
    put_u16(bytes, 18, 62); // EM_X86_64
    bytes[8..11].copy_from_slice(b"AI\x02");
}

fn superblock(bytes: &mut [u8], offset: usize, compression: u16) {
    let block = &mut bytes[offset..offset + 96];
    block[..4].copy_from_slice(b"hsqs");
    put_u32(block, 4, 46); // inodes
    put_u32(block, 12, 131_072);
    put_u32(block, 16, 1);
    put_u16(block, 20, compression);
    put_u16(block, 22, 17);
    put_u16(block, 24, 0);
    put_u16(block, 28, 4);
    put_u16(block, 30, 0);
    put_u64(block, 40, 4096);
    put_u64(block, 48, 2048); // id table
    put_u64(block, 56, u64::MAX); // no xattr table
    put_u64(block, 64, 512); // inode table
    put_u64(block, 72, 1536); // directory table
    put_u64(block, 80, 2560); // fragment table
    put_u64(block, 88, u64::MAX); // no export table
}

fn image() -> Vec<u8> {
    let mut bytes = vec![0u8; 8192];
    elf_header(&mut bytes, 3);
    superblock(&mut bytes, 1024, 6);
    bytes
}

#[test]
fn parses_a_type_two_image() {
    let info = parse(&image()).expect("valid image");
    assert_eq!(info.squashfs_offset, 1024);
    assert_eq!(info.squashfs_bytes, 4096);
    assert_eq!(info.block_size, 131_072);
    assert_eq!(info.compression, Compression::Zstd);
    assert_eq!(info.compression.name(), "zstd");
    assert_eq!(info.inodes, 46);
    assert_eq!(info.fragments, 1);
    assert_eq!(info.runtime, RuntimeElf::PositionIndependent);
}

#[test]
fn skips_a_false_magic_sequence() {
    let mut bytes = image();
    // A SquashFS-looking word in runtime code with an impossible superblock.
    bytes[256..260].copy_from_slice(b"hsqs");
    put_u32(&mut bytes, 260, 0);
    let info = parse(&bytes).expect("valid image after false positive");
    assert_eq!(info.squashfs_offset, 1024);
}

#[test]
fn skips_a_false_candidate_with_unknown_compression() {
    let mut bytes = image();
    bytes.copy_within(1024..1024 + 96, 256);
    put_u16(&mut bytes, 256 + 20, 99);
    let info = parse(&bytes).expect("valid image after false candidate");
    assert_eq!(info.squashfs_offset, 1024);
}

#[test]
fn supports_executable_runtimes() {
    let mut bytes = image();
    put_u16(&mut bytes, 16, 2);
    assert_eq!(parse(&bytes).unwrap().runtime, RuntimeElf::Executable);
}

#[test]
fn rejects_non_elf_and_non_appimage_files() {
    assert_eq!(parse(&[0u8; 8]), Err(Error::Truncated));
    let mut bytes = vec![0u8; 128];
    assert_eq!(parse(&bytes), Err(Error::NotElf));
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 3);
    put_u16(&mut bytes, 18, 62);
    assert_eq!(parse(&bytes), Err(Error::NotAppImage));
}

#[test]
fn rejects_wrong_architecture_and_class() {
    let mut bytes = image();
    put_u16(&mut bytes, 18, 40);
    assert_eq!(parse(&bytes), Err(Error::UnsupportedElf));

    let mut bytes = image();
    bytes[4] = 1;
    assert_eq!(parse(&bytes), Err(Error::UnsupportedElf));
}

#[test]
fn rejects_malformed_superblocks() {
    let mut bytes = image();
    put_u32(&mut bytes, 1024 + 12, 1000); // not a power of two
    assert_eq!(parse(&bytes), Err(Error::NoSquashfs));

    let mut bytes = image();
    put_u64(&mut bytes, 1024 + 40, 9000); // runs past the file
    assert_eq!(parse(&bytes), Err(Error::NoSquashfs));

    let mut bytes = image();
    put_u16(&mut bytes, 1024 + 28, 3); // wrong major
    assert_eq!(parse(&bytes), Err(Error::NoSquashfs));

    let mut bytes = image();
    put_u64(&mut bytes, 1024 + 64, 8192); // inode table outside filesystem
    assert_eq!(parse(&bytes), Err(Error::NoSquashfs));
}

#[test]
fn rejects_unknown_compression() {
    let mut bytes = image();
    put_u16(&mut bytes, 1024 + 20, 99);
    assert_eq!(parse(&bytes), Err(Error::UnsupportedCompression));
}

#[test]
fn rejects_images_without_a_superblock() {
    let mut bytes = vec![0u8; 4096];
    elf_header(&mut bytes, 3);
    assert_eq!(parse(&bytes), Err(Error::NoSquashfs));
}
