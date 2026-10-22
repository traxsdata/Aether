// SPDX-License-Identifier: Apache-2.0
//! Host tests for the AppImage container inspector.

use crate::manifest::{self, ManifestError};
use crate::{Compression, Digest, Error, RuntimeElf, parse, parse_verified};
use aether_packaging::signed::{TrustStore, TrustedPublisher};

const TEST_PUBLISHER: &str = "Aether Test Publisher";

fn test_seed() -> [u8; 32] {
    let text = include_str!("../../../../tests/fixtures/keys/test.seed");
    let mut seed = [0u8; 32];
    assert_eq!(
        aether_crypto::from_hex(text.trim().as_bytes(), &mut seed),
        Some(32)
    );
    seed
}

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
fn digest_pins_one_exact_artifact() {
    let bytes = image();
    let digest = Digest::of(&bytes);
    assert!(digest.matches(&bytes));
    assert_eq!(
        parse_verified(&bytes, digest).unwrap().squashfs_offset,
        1024
    );

    let mut other = bytes.clone();
    other.push(0);
    assert_eq!(
        Digest::from_hex(b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        Some(Digest::of(b"abc"))
    );
    assert_eq!(parse_verified(&other, digest), Err(Error::DigestMismatch));
}

fn signed_record(image: &[u8], runtime: &str, mutate: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    signed_record_with(image, runtime, "", mutate)
}

fn signed_record_with(
    image: &[u8],
    runtime: &str,
    extra: &str,
    mutate: impl FnOnce(&mut Vec<u8>),
) -> Vec<u8> {
    let digest = Digest::of(image);
    let mut hex = [0u8; 64];
    let length = aether_crypto::to_hex(digest.as_bytes(), &mut hex).unwrap();
    let digest_text = core::str::from_utf8(&hex[..length]).unwrap();
    let mut key_hex = [0u8; 64];
    let key = aether_crypto::ed25519::public_key_from_seed(&test_seed());
    let length = aether_crypto::to_hex(&key, &mut key_hex).unwrap();
    let key_text = core::str::from_utf8(&key_hex[..length]).unwrap();
    let body = format!(
        "id=org.artcraft.gridcraft\nversion=0.8.0\nsha256={digest_text}\nruntime={runtime}\npublisher={TEST_PUBLISHER}\npublisher-key={key_text}\n{extra}"
    );
    let signature = aether_crypto::ed25519::sign(&test_seed(), body.as_bytes());
    let mut record = Vec::new();
    record.extend_from_slice(&manifest::MANIFEST_MAGIC);
    record.extend_from_slice(&manifest::MANIFEST_VERSION.to_le_bytes());
    record.extend_from_slice(&(body.len() as u32).to_le_bytes());
    record.extend_from_slice(body.as_bytes());
    record.extend_from_slice(&signature);
    mutate(&mut record);
    record
}

#[test]
fn signed_import_record_verifies_the_image() {
    let image = image();
    let key = aether_crypto::ed25519::public_key_from_seed(&test_seed());
    let publishers = [TrustedPublisher {
        name: TEST_PUBLISHER,
        key,
    }];
    let trust = TrustStore::new(&publishers);
    let record = signed_record(&image, "linux-appimage", |_| {});
    let parsed = manifest::verify_image(&record, &trust, &image).expect("verified");
    assert_eq!(parsed.id, "org.artcraft.gridcraft");
    assert_eq!(parsed.version, "0.8.0");
    assert_eq!(parsed.runtime, "linux-appimage");
    assert_eq!(parsed.publisher, TEST_PUBLISHER);

    let mut other_image = image.clone();
    other_image.push(0);
    assert_eq!(
        manifest::verify_image(&record, &trust, &other_image).unwrap_err(),
        ManifestError::DigestMismatch
    );
}

#[test]
fn memory_jit_is_a_signed_opt_in() {
    let image = image();
    let key = aether_crypto::ed25519::public_key_from_seed(&test_seed());
    let publishers = [TrustedPublisher {
        name: TEST_PUBLISHER,
        key,
    }];
    let trust = TrustStore::new(&publishers);
    let plain = signed_record(&image, "linux-appimage", |_| {});
    assert!(
        !manifest::verify_image(&plain, &trust, &image)
            .expect("verified")
            .jit
    );

    let jit = signed_record_with(&image, "linux-appimage", "memory=jit\n", |_| {});
    assert!(
        manifest::verify_image(&jit, &trust, &image)
            .expect("verified")
            .jit
    );

    let unknown = signed_record_with(&image, "linux-appimage", "memory=wx\n", |_| {});
    assert_eq!(
        manifest::verify(&unknown, &trust).unwrap_err(),
        ManifestError::Field
    );
    let duplicate =
        signed_record_with(&image, "linux-appimage", "memory=jit\nmemory=jit\n", |_| {});
    assert_eq!(
        manifest::verify(&duplicate, &trust).unwrap_err(),
        ManifestError::Field
    );
}

#[test]
fn signed_runtime_rootfs_record_verifies() {
    let image = image();
    let key = aether_crypto::ed25519::public_key_from_seed(&test_seed());
    let publishers = [TrustedPublisher {
        name: TEST_PUBLISHER,
        key,
    }];
    let trust = TrustStore::new(&publishers);
    let record = signed_record(&image, "linux-rootfs", |_| {});
    let parsed = manifest::verify_image(&record, &trust, &image).expect("verified");
    assert_eq!(parsed.runtime, "linux-rootfs");
}

#[test]
fn signed_import_record_rejects_bad_signature_and_untrusted_key() {
    let image = image();
    let key = aether_crypto::ed25519::public_key_from_seed(&test_seed());
    let publishers = [TrustedPublisher {
        name: TEST_PUBLISHER,
        key,
    }];
    let trust = TrustStore::new(&publishers);
    let record = signed_record(&image, "linux-appimage", |record| {
        let length = record.len();
        record[length - 1] ^= 0xff;
    });
    assert_eq!(
        manifest::verify(&record, &trust).unwrap_err(),
        ManifestError::Signature
    );

    let record = signed_record(&image, "linux-appimage", |_| {});
    let empty_entries: [TrustedPublisher; 0] = [];
    let empty = TrustStore::new(&empty_entries);
    assert_eq!(
        manifest::verify(&record, &empty).unwrap_err(),
        ManifestError::UntrustedPublisher
    );
    let wrong_entries = [TrustedPublisher {
        name: "Someone Else",
        key,
    }];
    let wrong_name = TrustStore::new(&wrong_entries);
    assert_eq!(
        manifest::verify(&record, &wrong_name).unwrap_err(),
        ManifestError::PublisherName
    );
}

#[test]
fn rejects_images_without_a_superblock() {
    let mut bytes = vec![0u8; 4096];
    elf_header(&mut bytes, 3);
    assert_eq!(parse(&bytes), Err(Error::NoSquashfs));
}
