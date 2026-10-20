// SPDX-License-Identifier: Apache-2.0
//! Host tests for update verification and activation policy.

extern crate std;

use std::format;
use std::string::String;
use std::vec;
use std::vec::Vec;

use crate::{
    InstalledState, PolicyError, PolicyOptions, UPDATE_FORMAT, UPDATE_MAGIC, UpdateError,
    UpdateManifest, check_policy, verify_update,
};
use aether_crypto::{ed25519, sha256, to_hex};
use aether_packaging::manifest::{Channel, Version};
use aether_packaging::signed::{TrustStore, TrustedPublisher, decode_frame, encode_frame};

const SEED: [u8; 32] = [3u8; 32];
const OTHER_SEED: [u8; 32] = [4u8; 32];
const PUBLISHER: &str = "Aether Test Publisher";

fn hex(data: &[u8]) -> String {
    let mut out = vec![0u8; data.len() * 2];
    let length = to_hex(data, &mut out).unwrap();
    String::from_utf8(out[..length].to_vec()).unwrap()
}

fn publisher(seed: &[u8; 32]) -> TrustedPublisher<'_> {
    TrustedPublisher {
        name: PUBLISHER,
        key: ed25519::public_key_from_seed(seed),
    }
}

fn manifest_text(
    seed: &[u8; 32],
    image: &[u8],
    version: &str,
    counter: u64,
    channel: &str,
) -> Vec<u8> {
    let key = ed25519::public_key_from_seed(seed);
    format!(
        "format=1\n\
         image=aether-{version}.iso\n\
         version={version}\n\
         counter={counter}\n\
         channel={channel}\n\
         publisher={PUBLISHER}\n\
         publisher-key={}\n\
         image.sha256={}\n",
        hex(&key),
        hex(&sha256(image))
    )
    .into_bytes()
}

fn sign(seed: &[u8; 32], body: Vec<u8>) -> Vec<u8> {
    let signature = ed25519::sign(seed, &body);
    let mut out = vec![0u8; 4096];
    let length = encode_frame(
        &UPDATE_MAGIC,
        UPDATE_FORMAT,
        &body,
        &[],
        &signature,
        &mut out,
    )
    .expect("frame");
    out.truncate(length);
    out
}

fn update(version: &str, counter: u64, channel: &str) -> (Vec<u8>, Vec<u8>) {
    let image = format!("aether-{version}.iso").into_bytes();
    let body = manifest_text(&SEED, &image, version, counter, channel);
    (sign(&SEED, body), image)
}

fn installed(version: Version, counter: u64) -> InstalledState {
    InstalledState {
        version,
        counter,
        channel: Channel::Developer,
    }
}

#[test]
fn valid_update_verifies() {
    let (manifest, image) = update("0.2.0", 2, "developer");
    let trusted = publisher(&SEED);
    let store = TrustStore::new(std::slice::from_ref(&trusted));
    let verified = verify_update(&manifest, &image, &store).expect("verify");
    assert_eq!(verified.version, Version::new(0, 2, 0));
    assert_eq!(verified.counter, 2);
    assert_eq!(verified.image, "aether-0.2.0.iso");
    assert_eq!(verified.publisher, PUBLISHER);
}

#[test]
fn image_names_may_exceed_the_payload_component_limit() {
    let (manifest, image) = update("0.10.0", 10, "developer");
    let trusted = publisher(&SEED);
    let store = TrustStore::new(std::slice::from_ref(&trusted));
    let verified = verify_update(&manifest, &image, &store).expect("verify");
    assert_eq!(verified.image, "aether-0.10.0.iso");
}

#[test]
fn manifest_round_trips_through_canonical_rendering() {
    let (manifest, _image) = update("0.2.0", 7, "beta");
    let frame = decode_frame(&manifest, &UPDATE_MAGIC).expect("frame");
    let parsed = UpdateManifest::parse(frame.body).expect("parse");
    let mut rendered = [0u8; crate::MAX_UPDATE_MANIFEST_BYTES];
    let length = parsed.render(&mut rendered).expect("render");
    assert_eq!(&rendered[..length], frame.body);
}

#[test]
fn update_manifest_must_not_carry_a_payload() {
    let image = b"aether-0.2.0.iso".to_vec();
    let body = manifest_text(&SEED, &image, "0.2.0", 2, "developer");
    let signature = ed25519::sign(&SEED, &body);
    let mut record = vec![0u8; 4096];
    let length = encode_frame(
        &UPDATE_MAGIC,
        UPDATE_FORMAT,
        &body,
        b"unsigned bytes",
        &signature,
        &mut record,
    )
    .expect("frame");
    record.truncate(length);
    let trusted = publisher(&SEED);
    let store = TrustStore::new(std::slice::from_ref(&trusted));
    assert_eq!(
        verify_update(&record, &image, &store).unwrap_err(),
        UpdateError::UnexpectedPayload
    );
}

#[test]
fn tampered_manifest_and_image_are_rejected() {
    let (mut manifest, image) = update("0.2.0", 2, "developer");
    let trusted = publisher(&SEED);
    let store = TrustStore::new(std::slice::from_ref(&trusted));
    let position = manifest
        .windows(8)
        .position(|window| window == b"counter=")
        .unwrap();
    manifest[position + 8] = b'3';
    assert_eq!(
        verify_update(&manifest, &image, &store).unwrap_err(),
        UpdateError::BadSignature
    );

    let (manifest, mut image) = update("0.2.0", 2, "developer");
    image.push(0xff);
    assert_eq!(
        verify_update(&manifest, &image, &store).unwrap_err(),
        UpdateError::DigestMismatch
    );
}

#[test]
fn unknown_publisher_and_name_mismatch_are_rejected() {
    let (manifest, image) = update("0.2.0", 2, "developer");
    let other = publisher(&OTHER_SEED);
    let store = TrustStore::new(std::slice::from_ref(&other));
    assert_eq!(
        verify_update(&manifest, &image, &store).unwrap_err(),
        UpdateError::UnknownPublisher
    );

    let wrong_name = TrustedPublisher {
        name: "Someone Else",
        key: ed25519::public_key_from_seed(&SEED),
    };
    let store = TrustStore::new(std::slice::from_ref(&wrong_name));
    assert_eq!(
        verify_update(&manifest, &image, &store).unwrap_err(),
        UpdateError::PublisherMismatch
    );
}

#[test]
fn policy_rejects_replays_and_downgrades() {
    let (manifest, image) = update("0.2.0", 2, "developer");
    let trusted = publisher(&SEED);
    let store = TrustStore::new(std::slice::from_ref(&trusted));
    let verified = verify_update(&manifest, &image, &store).expect("verify");

    assert_eq!(
        check_policy(
            &verified,
            installed(Version::new(0, 1, 0), 2),
            PolicyOptions::default()
        ),
        Err(PolicyError::Rollback)
    );
    assert_eq!(
        check_policy(
            &verified,
            installed(Version::new(0, 1, 0), 3),
            PolicyOptions::default()
        ),
        Err(PolicyError::Rollback)
    );
    assert_eq!(
        check_policy(
            &verified,
            installed(Version::new(0, 3, 0), 1),
            PolicyOptions::default()
        ),
        Err(PolicyError::Downgrade)
    );
    assert_eq!(
        check_policy(
            &verified,
            installed(Version::new(0, 2, 0), 1),
            PolicyOptions::default()
        ),
        Err(PolicyError::Downgrade)
    );
    assert_eq!(
        check_policy(
            &verified,
            installed(Version::new(0, 1, 0), 1),
            PolicyOptions::default()
        ),
        Ok(())
    );
}

#[test]
fn channel_mismatch_is_rejected_and_recovery_can_roll_back_with_a_newer_counter() {
    let (manifest, image) = update("0.2.0", 2, "beta");
    let trusted = publisher(&SEED);
    let store = TrustStore::new(std::slice::from_ref(&trusted));
    let verified = verify_update(&manifest, &image, &store).expect("verify");
    let mut state = installed(Version::new(0, 1, 0), 1);
    assert_eq!(
        check_policy(&verified, state, PolicyOptions::default()),
        Err(PolicyError::ChannelMismatch)
    );

    state.channel = Channel::Beta;
    assert_eq!(
        check_policy(&verified, state, PolicyOptions::default()),
        Ok(())
    );

    // A recovery image with an older version but a newer counter is only
    // accepted when the operator explicitly asks for it.
    let (manifest, image) = update("0.1.0", 3, "beta");
    let verified = verify_update(&manifest, &image, &store).expect("verify");
    let mut recovery = installed(Version::new(0, 2, 0), 2);
    recovery.channel = Channel::Beta;
    assert_eq!(
        check_policy(&verified, recovery, PolicyOptions::default()),
        Err(PolicyError::Downgrade)
    );
    assert_eq!(
        check_policy(
            &verified,
            recovery,
            PolicyOptions {
                allow_older_version: true
            }
        ),
        Ok(())
    );
}

#[test]
fn malformed_manifests_are_rejected() {
    let image = b"image";
    let mut body = manifest_text(&SEED, image, "0.2.0", 2, "developer");
    body.extend_from_slice(b"counter=3\n");
    let manifest = sign(&SEED, body);
    let trusted = publisher(&SEED);
    let store = TrustStore::new(std::slice::from_ref(&trusted));
    assert_eq!(
        verify_update(&manifest, image, &store).unwrap_err(),
        UpdateError::Field(aether_packaging::text::FieldError::DuplicateKey)
    );

    let mut body = manifest_text(&SEED, image, "0.2.0", 2, "developer");
    body.extend_from_slice(b"unknown=1\n");
    let manifest = sign(&SEED, body);
    assert_eq!(
        verify_update(&manifest, image, &store).unwrap_err(),
        UpdateError::Field(aether_packaging::text::FieldError::UnknownKey)
    );

    let mut body = manifest_text(&SEED, image, "0.2.0", 2, "developer");
    let position = body
        .windows(9)
        .position(|window| window == b"counter=2")
        .unwrap();
    body[position + 8] = b'x';
    let manifest = sign(&SEED, body);
    assert_eq!(
        verify_update(&manifest, image, &store).unwrap_err(),
        UpdateError::InvalidCounter
    );
}

#[test]
fn frame_magic_and_version_are_enforced() {
    let (manifest, image) = update("0.2.0", 2, "developer");
    let trusted = publisher(&SEED);
    let store = TrustStore::new(std::slice::from_ref(&trusted));
    let mut wrong_magic = manifest.clone();
    wrong_magic[0] = b'X';
    assert!(matches!(
        verify_update(&wrong_magic, &image, &store),
        Err(UpdateError::Frame(
            aether_packaging::signed::FrameError::BadMagic
        ))
    ));

    let mut wrong_version = manifest;
    wrong_version[8] = 2;
    assert_eq!(
        verify_update(&wrong_version, &image, &store).unwrap_err(),
        UpdateError::UnsupportedVersion
    );
}
