// SPDX-License-Identifier: Apache-2.0
//! Host tests for package framing, manifests, payloads, and verification.

extern crate std;

use std::format;
use std::string::String;
use std::vec;
use std::vec::Vec;

use crate::manifest::{Manifest, ManifestFields, Permissions, Version, is_safe_relative_path};
use crate::payload::encode_payload;
use crate::signed::{TrustStore, TrustedPublisher, decode_frame, encode_frame};
use crate::text::{FieldError, Fields};
use crate::{MAX_MANIFEST_BYTES, PACKAGE_FORMAT, PACKAGE_MAGIC, PackageError, verify_package};
use aether_crypto::{ed25519, sha256, to_hex};

const SEED: [u8; 32] = [7u8; 32];
const OTHER_SEED: [u8; 32] = [9u8; 32];

fn hex(data: &[u8]) -> String {
    let mut out = vec![0u8; data.len() * 2];
    let length = to_hex(data, &mut out).unwrap();
    String::from_utf8(out[..length].to_vec()).unwrap()
}

fn publishers<'a>(seed: &[u8; 32], name: &'a str) -> (TrustedPublisher<'a>, [u8; 32]) {
    let key = ed25519::public_key_from_seed(seed);
    (TrustedPublisher { name, key }, key)
}

fn manifest_text(key: &[u8; 32], digest: &[u8; 32]) -> Vec<u8> {
    let text = format!(
        "format=1\n\
         id=org.aether.demo\n\
         version=0.2.0\n\
         arch=x86_64\n\
         runtime=native\n\
         entry=demo.bin\n\
         publisher=Aether Test Publisher\n\
         publisher-key={}\n\
         channel=developer\n\
         permissions=file.read,file.write\n\
         limits.memory_kib=256\n\
         limits.open_files=4\n\
         limits.tasks=1\n\
         payload.sha256={}\n",
        hex(key),
        hex(digest)
    );
    text.into_bytes()
}

fn build(seed: &[u8; 32], files: &[(&str, &[u8])], mutate: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let mut payload = vec![0u8; 4096];
    let payload_len = encode_payload(files, &mut payload).expect("payload");
    payload.truncate(payload_len);
    build_with_payload(seed, payload, mutate)
}

fn build_with_payload(seed: &[u8; 32], payload: Vec<u8>, mutate: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let key = ed25519::public_key_from_seed(seed);
    let digest = sha256(&payload);
    let mut body = manifest_text(&key, &digest);
    mutate(&mut body);
    let signature = ed25519::sign(seed, &body);
    let mut package = vec![0u8; 8192];
    let length = encode_frame(
        &PACKAGE_MAGIC,
        PACKAGE_FORMAT,
        &body,
        &payload,
        &signature,
        &mut package,
    )
    .expect("frame");
    package.truncate(length);
    package
}

fn normal(seed: &[u8; 32]) -> Vec<u8> {
    build(
        seed,
        &[("demo.bin", b"native"), ("data.txt", b"hello")],
        |_| {},
    )
}

#[test]
fn valid_package_verifies_and_exposes_files() {
    let (publisher, _key) = publishers(&SEED, "Aether Test Publisher");
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let package = normal(&SEED);
    let verified = verify_package(&package, &store).expect("verify");
    assert_eq!(verified.manifest.id, "org.aether.demo");
    assert_eq!(verified.manifest.version, Version::new(0, 2, 0));
    assert_eq!(
        verified.manifest.permissions.bits(),
        Permissions::FILE_READ | Permissions::FILE_WRITE
    );
    assert_eq!(verified.payload.len(), 2);
    assert_eq!(verified.payload.get("demo.bin").unwrap().data, b"native");
}

#[test]
fn manifest_round_trips_through_canonical_rendering() {
    let key = ed25519::public_key_from_seed(&SEED);
    let digest = sha256(b"payload");
    let text = manifest_text(&key, &digest);
    let fields = ManifestFields::parse(&text).expect("fields");
    let manifest = fields.build(digest).expect("manifest");
    let mut rendered = [0u8; MAX_MANIFEST_BYTES];
    let length = manifest.render(&mut rendered).expect("render");
    let reparsed = Manifest::parse(&rendered[..length]).expect("reparse");
    assert_eq!(reparsed, manifest);
    assert_eq!(&rendered[..length], text.as_slice());
}

#[test]
fn tampered_payload_is_rejected() {
    let (publisher, _key) = publishers(&SEED, "Aether Test Publisher");
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let mut package = normal(&SEED);
    let last = package.len() - 65;
    package[last] ^= 1;
    assert_eq!(
        verify_package(&package, &store).unwrap_err(),
        PackageError::DigestMismatch
    );
}

#[test]
fn tampered_manifest_is_rejected() {
    let (publisher, _key) = publishers(&SEED, "Aether Test Publisher");
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let mut package = normal(&SEED);
    // Change the signed body without re-signing.
    let position = package
        .windows(5)
        .position(|window| window == b"0.2.0")
        .unwrap();
    package[position + 2] = b'9';
    assert_eq!(
        verify_package(&package, &store).unwrap_err(),
        PackageError::BadSignature
    );
}

#[test]
fn unknown_publisher_is_rejected_before_the_signature_is_checked() {
    let (publisher, _key) = publishers(&SEED, "Aether Test Publisher");
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let package = normal(&OTHER_SEED);
    assert_eq!(
        verify_package(&package, &store).unwrap_err(),
        PackageError::UnknownPublisher
    );
}

#[test]
fn publisher_name_must_match_the_trusted_key() {
    // The key is trusted, but under a different display name than the package
    // claims. A key must not be able to speak for another publisher name.
    let (publisher, _key) = publishers(&SEED, "Somebody Else");
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let package = normal(&SEED);
    assert_eq!(
        verify_package(&package, &store).unwrap_err(),
        PackageError::PublisherMismatch
    );
}

#[test]
fn entry_must_exist_in_the_payload() {
    let (publisher, _key) = publishers(&SEED, "Aether Test Publisher");
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let package = build(&SEED, &[("other.bin", b"native")], |_| {});
    assert_eq!(
        verify_package(&package, &store).unwrap_err(),
        PackageError::EntryMissing
    );
}

fn raw_payload(name: &[u8], data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u16.to_le_bytes());
    payload.extend_from_slice(&(name.len() as u16).to_le_bytes());
    payload.extend_from_slice(name);
    payload.extend_from_slice(&(data.len() as u32).to_le_bytes());
    payload.extend_from_slice(data);
    payload
}

#[test]
fn unsafe_payload_names_are_rejected() {
    for name in ["..", ".", "a/b", "UPPER", "a name", ""] {
        let package = build_with_payload(&SEED, raw_payload(name.as_bytes(), b"x"), |_| {});
        let (publisher, _key) = publishers(&SEED, "Aether Test Publisher");
        let store = TrustStore::new(std::slice::from_ref(&publisher));
        assert_eq!(
            verify_package(&package, &store).unwrap_err(),
            PackageError::Payload(crate::payload::PayloadError::InvalidName),
            "name {name:?}"
        );
    }
}

#[test]
fn duplicate_payload_names_cannot_be_encoded() {
    let mut out = [0u8; 1024];
    assert_eq!(
        encode_payload(&[("same", b"a"), ("same", b"b")], &mut out),
        None
    );
}

#[test]
fn manifest_rejects_duplicate_and_unknown_fields() {
    let key = ed25519::public_key_from_seed(&SEED);
    let digest = sha256(b"payload");
    let mut text = manifest_text(&key, &digest);
    text.extend_from_slice(b"format=1\n");
    assert!(matches!(
        ManifestFields::parse(&text),
        Err(crate::manifest::ManifestError::Field(
            FieldError::DuplicateKey
        ))
    ));

    let mut text = manifest_text(&key, &digest);
    text.extend_from_slice(b"extra.field=1\n");
    assert!(matches!(
        ManifestFields::parse(&text),
        Err(crate::manifest::ManifestError::Field(
            FieldError::UnknownKey
        ))
    ));
}

#[test]
fn permissions_are_an_allowlist() {
    for denied in [
        "device.block",
        "network",
        "process.spawn",
        "grant.issue",
        "file.system",
    ] {
        let package = build(&SEED, &[("demo.bin", b"native")], |body| {
            let text = std::str::from_utf8(body).unwrap();
            let replaced = text.replace("file.read,file.write", denied);
            body.clear();
            body.extend_from_slice(replaced.as_bytes());
        });
        let (publisher, _key) = publishers(&SEED, "Aether Test Publisher");
        let store = TrustStore::new(std::slice::from_ref(&publisher));
        assert_eq!(
            verify_package(&package, &store).unwrap_err(),
            PackageError::Manifest(crate::manifest::ManifestError::UnknownPermission),
            "permission {denied}"
        );
    }
}

#[test]
fn unsupported_runtime_and_limits_are_rejected() {
    let package = build(&SEED, &[("demo.bin", b"native")], |body| {
        let text = std::str::from_utf8(body).unwrap();
        let replaced = text.replace("runtime=native", "runtime=windows");
        body.clear();
        body.extend_from_slice(replaced.as_bytes());
    });
    let (publisher, _key) = publishers(&SEED, "Aether Test Publisher");
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    assert_eq!(
        verify_package(&package, &store).unwrap_err(),
        PackageError::Manifest(crate::manifest::ManifestError::UnsupportedRuntime)
    );

    let package = build(&SEED, &[("demo.bin", b"native")], |body| {
        let text = std::str::from_utf8(body).unwrap();
        let replaced = text.replace("limits.memory_kib=256", "limits.memory_kib=65535");
        body.clear();
        body.extend_from_slice(replaced.as_bytes());
    });
    assert_eq!(
        verify_package(&package, &store).unwrap_err(),
        PackageError::Manifest(crate::manifest::ManifestError::InvalidLimit)
    );
}

#[test]
fn framing_rejects_truncation_and_trailing_bytes() {
    let package = normal(&SEED);
    let mut truncated = package.clone();
    truncated.pop();
    assert_eq!(
        decode_frame(&truncated, &PACKAGE_MAGIC).unwrap_err(),
        crate::signed::FrameError::Truncated
    );

    let mut extended = package.clone();
    extended.push(0);
    assert_eq!(
        decode_frame(&extended, &PACKAGE_MAGIC).unwrap_err(),
        crate::signed::FrameError::TrailingBytes
    );

    assert_eq!(
        decode_frame(&package, b"AETHERXX").unwrap_err(),
        crate::signed::FrameError::BadMagic
    );
}

#[test]
fn identity_and_path_rules_are_strict() {
    for valid in ["org.aether.demo", "io.example.app2"] {
        assert!(crate::manifest::is_valid_identity(valid), "{valid}");
    }
    for invalid in [
        "",
        "aether",
        "org..demo",
        "Org.Aether",
        "-org.demo",
        "org.demo-",
        "org.aether.toolonglabeltoolonglabel",
    ] {
        assert!(!crate::manifest::is_valid_identity(invalid), "{invalid}");
    }

    assert!(is_safe_relative_path("demo.bin"));
    assert!(is_safe_relative_path("bin/demo.bin"));
    for invalid in [
        "",
        "/demo.bin",
        "../demo.bin",
        "bin/../x",
        "./x",
        "bin//x",
        "Demo.bin",
    ] {
        assert!(!is_safe_relative_path(invalid), "{invalid}");
    }
}

#[test]
fn fields_reject_control_characters() {
    let fields = Fields::parse(b"id=ok\n", 32).unwrap();
    assert_eq!(fields.require("id"), Ok("ok"));
    assert_eq!(
        Fields::parse(b"id=ok\x1b[2J\n", 32).unwrap_err(),
        FieldError::InvalidCharacter
    );
    assert_eq!(
        Fields::parse(b"=value\n", 32).unwrap_err(),
        FieldError::InvalidCharacter
    );
}

#[test]
fn empty_permissions_use_explicit_none() {
    let permissions = Permissions::parse_list("none").unwrap();
    assert_eq!(permissions.bits(), 0);
    let mut out = [0u8; 16];
    let length = permissions.write_list(&mut out).unwrap();
    assert_eq!(&out[..length], b"none");
    assert_eq!(
        Permissions::parse_list(""),
        Err(crate::manifest::ManifestError::InvalidPermissions)
    );
}

#[test]
fn zeroed_signatures_are_rejected() {
    let (publisher, _key) = publishers(&SEED, "Aether Test Publisher");
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let mut package = normal(&SEED);
    let length = package.len();
    package[length - 64..].fill(0);
    assert_eq!(
        verify_package(&package, &store).unwrap_err(),
        PackageError::BadSignature
    );
}
