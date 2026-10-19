// SPDX-License-Identifier: Apache-2.0
//! Package verification.

use crate::manifest::{Manifest, ManifestError};
use crate::payload::{Payload, PayloadError};
use crate::signed::{FrameError, TrustStore, decode_frame};
use crate::{MAX_MANIFEST_BYTES, MAX_PAYLOAD_BYTES, PACKAGE_FORMAT, PACKAGE_MAGIC};
use aether_crypto::{sha256, verify};

/// File in a verified package.
pub use crate::payload::PayloadFile as PackageFile;

/// Package verification errors.
///
/// The variants are intentionally coarse: the kernel reports the reason to the
/// console but never accepts a partially verified package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageError {
    /// The record framing is invalid.
    Frame(FrameError),
    /// The manifest is invalid.
    Manifest(ManifestError),
    /// The package format version is not supported.
    UnsupportedVersion,
    /// The manifest length exceeds the policy limit.
    ManifestTooLarge,
    /// The payload length exceeds the policy limit.
    PayloadTooLarge,
    /// The publisher key is not trusted.
    UnknownPublisher,
    /// The manifest publisher name does not match the trusted key's owner.
    PublisherMismatch,
    /// The Ed25519 signature is invalid.
    BadSignature,
    /// The payload does not match the manifest digest.
    DigestMismatch,
    /// The payload archive is malformed.
    Payload(PayloadError),
    /// The declared entry point is not in the payload.
    EntryMissing,
}

impl From<FrameError> for PackageError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<ManifestError> for PackageError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<PayloadError> for PackageError {
    fn from(error: PayloadError) -> Self {
        Self::Payload(error)
    }
}

/// A package that passed every check.
#[derive(Debug)]
pub struct VerifiedPackage<'a> {
    /// Validated manifest.
    pub manifest: Manifest<'a>,
    /// Parsed payload archive.
    pub payload: Payload<'a>,
}

impl VerifiedPackage<'_> {
    /// Iterates over payload files.
    pub fn files(&self) -> impl Iterator<Item = &PackageFile<'_>> {
        self.payload.files()
    }
}

/// Verifies a package against a trust store.
///
/// The returned value borrows from `bytes`; the caller must keep the package
/// image alive while it installs files.
pub fn verify_package<'a>(
    bytes: &'a [u8],
    trust: &TrustStore<'_>,
) -> Result<VerifiedPackage<'a>, PackageError> {
    let frame = decode_frame(bytes, &PACKAGE_MAGIC)?;
    if frame.version != PACKAGE_FORMAT {
        return Err(PackageError::UnsupportedVersion);
    }
    if frame.body.len() > MAX_MANIFEST_BYTES {
        return Err(PackageError::ManifestTooLarge);
    }
    if frame.payload.len() > MAX_PAYLOAD_BYTES {
        return Err(PackageError::PayloadTooLarge);
    }

    let manifest = Manifest::parse(frame.body)?;
    let publisher = trust
        .find(&manifest.publisher_key)
        .ok_or(PackageError::UnknownPublisher)?;
    if publisher.name != manifest.publisher {
        return Err(PackageError::PublisherMismatch);
    }
    if !verify(&manifest.publisher_key, frame.body, &frame.signature) {
        return Err(PackageError::BadSignature);
    }
    if sha256(frame.payload) != manifest.payload_digest {
        return Err(PackageError::DigestMismatch);
    }

    let payload = Payload::parse(frame.payload)?;
    if payload.get(manifest.entry).is_none() {
        return Err(PackageError::EntryMissing);
    }
    Ok(VerifiedPackage { manifest, payload })
}
