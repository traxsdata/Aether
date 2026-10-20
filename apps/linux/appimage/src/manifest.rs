// SPDX-License-Identifier: Apache-2.0
//! Signed import records for Linux AppImages.
//!
//! An upstream AppImage is not necessarily signed. A runtime package can ship
//! a small Ed25519-signed record that pins the application identity, version,
//! runtime, exact SHA-256 digest, and publisher. The record is framed as:
//!
//! ```text
//! magic       8 bytes   "AETHERAI"
//! version     u16 LE    currently 1
//! body length u32 LE
//! body        canonical key=value lines
//! signature   64 bytes  Ed25519 over the body
//! ```
//!
//! Verification order is strict: framing, field policy, trusted publisher key,
//! publisher-name match, signature, then the image digest. A valid signature
//! says who vouched for an artifact; it does not make the application safe.

use crate::Digest;
use aether_packaging::TrustStore;

/// Import-record magic.
pub const MANIFEST_MAGIC: [u8; 8] = *b"AETHERAI";
/// Current import-record version.
pub const MANIFEST_VERSION: u16 = 1;
/// Maximum canonical body size.
pub const MAX_BODY: usize = 1024;

/// Import-record verification failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// The record is shorter than its header or body.
    Truncated,
    /// The magic does not match an import record.
    BadMagic,
    /// The version is not supported.
    UnsupportedVersion,
    /// The body exceeds the fixed limit.
    BodyTooLarge,
    /// A required field is missing, duplicated, empty, or malformed.
    Field,
    /// The application identity is not a reverse-domain name.
    Identity,
    /// The digest field is not 64 hexadecimal characters.
    Digest,
    /// The publisher key is not 32 bytes of hexadecimal.
    PublisherKey,
    /// The manifest runtime is not `linux-appimage` or `linux-rootfs`.
    Runtime,
    /// The publisher is not in the trust store.
    UntrustedPublisher,
    /// The trusted publisher's name does not match the manifest.
    PublisherName,
    /// The Ed25519 signature is invalid.
    Signature,
    /// The image digest does not match the signed record.
    DigestMismatch,
}

/// Parsed, unverified import record.
#[derive(Clone, Copy, Debug)]
pub struct ImportManifest<'a> {
    /// Application identity.
    pub id: &'a str,
    /// Application version.
    pub version: &'a str,
    /// Pinned image digest.
    pub digest: Digest,
    /// Runtime name.
    pub runtime: &'a str,
    /// Publisher display name.
    pub publisher: &'a str,
    /// Publisher Ed25519 public key.
    pub publisher_key: [u8; 32],
    /// Signed canonical body.
    pub body: &'a [u8],
    /// Ed25519 signature over `body`.
    pub signature: [u8; 64],
}

/// Parses an import record without verifying its signature.
pub fn parse(bytes: &[u8]) -> Result<ImportManifest<'_>, ManifestError> {
    const HEADER: usize = 8 + 2 + 4;
    if bytes.len() < HEADER + 64 {
        return Err(ManifestError::Truncated);
    }
    if bytes[..8] != MANIFEST_MAGIC {
        return Err(ManifestError::BadMagic);
    }
    let version = u16::from_le_bytes([bytes[8], bytes[9]]);
    if version != MANIFEST_VERSION {
        return Err(ManifestError::UnsupportedVersion);
    }
    let body_length = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
    if body_length > MAX_BODY {
        return Err(ManifestError::BodyTooLarge);
    }
    let body_end = HEADER
        .checked_add(body_length)
        .ok_or(ManifestError::Truncated)?;
    let body = bytes
        .get(HEADER..body_end)
        .ok_or(ManifestError::Truncated)?;
    let signature_bytes = bytes
        .get(body_end..body_end + 64)
        .ok_or(ManifestError::Truncated)?;
    if bytes.len() != body_end + 64 {
        return Err(ManifestError::Truncated);
    }
    let body_text = core::str::from_utf8(body).map_err(|_| ManifestError::Field)?;
    let mut id = None;
    let mut app_version = None;
    let mut digest = None;
    let mut runtime = None;
    let mut publisher = None;
    let mut publisher_key = None;
    for line in body_text.split('\n') {
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or(ManifestError::Field)?;
        let slot = match key {
            "id" => &mut id,
            "version" => &mut app_version,
            "sha256" => &mut digest,
            "runtime" => &mut runtime,
            "publisher" => &mut publisher,
            "publisher-key" => &mut publisher_key,
            _ => return Err(ManifestError::Field),
        };
        if slot.replace(value).is_some() {
            return Err(ManifestError::Field);
        }
    }
    let id = id.ok_or(ManifestError::Field)?;
    let version = app_version.ok_or(ManifestError::Field)?;
    let runtime = runtime.ok_or(ManifestError::Field)?;
    let publisher = publisher.ok_or(ManifestError::Field)?;
    if !aether_packaging::manifest::is_valid_identity(id) {
        return Err(ManifestError::Identity);
    }
    if runtime != "linux-appimage" && runtime != "linux-rootfs" {
        return Err(ManifestError::Runtime);
    }
    if version.is_empty() || version.len() > 32 || publisher.is_empty() || publisher.len() > 64 {
        return Err(ManifestError::Field);
    }
    let digest = Digest::from_hex(digest.ok_or(ManifestError::Field)?.as_bytes())
        .ok_or(ManifestError::Digest)?;
    let mut key = [0u8; 32];
    let key_text = publisher_key.ok_or(ManifestError::Field)?.as_bytes();
    if aether_crypto::from_hex(key_text, &mut key) != Some(32) {
        return Err(ManifestError::PublisherKey);
    }
    let mut signature = [0u8; 64];
    signature.copy_from_slice(signature_bytes);
    Ok(ImportManifest {
        id,
        version,
        digest,
        runtime,
        publisher,
        publisher_key: key,
        body,
        signature,
    })
}

/// Verifies the trusted publisher and signature of an import record.
pub fn verify<'a>(
    bytes: &'a [u8],
    trust: &TrustStore<'_>,
) -> Result<ImportManifest<'a>, ManifestError> {
    let manifest = parse(bytes)?;
    let publisher = trust
        .find(&manifest.publisher_key)
        .ok_or(ManifestError::UntrustedPublisher)?;
    if publisher.name != manifest.publisher {
        return Err(ManifestError::PublisherName);
    }
    if !aether_crypto::verify(&manifest.publisher_key, manifest.body, &manifest.signature) {
        return Err(ManifestError::Signature);
    }
    Ok(manifest)
}

/// Verifies the record and that its signed digest matches the image bytes.
pub fn verify_image<'a>(
    bytes: &'a [u8],
    trust: &TrustStore<'_>,
    image: &[u8],
) -> Result<ImportManifest<'a>, ManifestError> {
    let manifest = verify(bytes, trust)?;
    if !manifest.digest.matches(image) {
        return Err(ManifestError::DigestMismatch);
    }
    Ok(manifest)
}
