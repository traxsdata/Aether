// SPDX-License-Identifier: Apache-2.0
//! Signed system update manifests and activation policy.
//!
//! A system image is accompanied by a signed manifest that names the image,
//! its version, a monotonically increasing rollback counter, its channel, its
//! publisher, and the SHA-256 digest of the image bytes.
//!
//! The verifier checks, in order:
//!
//! 1. Framing (`AETHERUP`, version 1) and the bounded manifest fields.
//! 2. The publisher key is in the trust store and the claimed publisher name
//!    matches the trusted entry.
//! 3. The Ed25519 signature over the exact manifest bytes.
//! 4. The image digest.
//!
//! [`check_policy`] is a separate step because activation policy depends on
//! installed state, not on the artifact. A valid signature does not authorise
//! a downgrade or a replay.
//!
//! # Non-claims
//!
//! This crate does not claim Secure Boot, measured boot, TPM-backed counters,
//! or protection against an attacker who can write the boot media. It
//! implements the verifier and policy the kernel can enforce today.

#![no_std]

#[cfg(test)]
extern crate std;

use aether_crypto::{from_hex, sha256, to_hex, verify};
use aether_packaging::manifest::{Channel, ManifestError, Version, is_safe_relative_path};
use aether_packaging::signed::{FrameError, TrustStore, decode_frame};
use aether_packaging::text::{FieldError, Fields};

/// Framing magic for update manifests.
pub const UPDATE_MAGIC: [u8; 8] = *b"AETHERUP";
/// Current update manifest framing version.
pub const UPDATE_FORMAT: u16 = 1;
/// Maximum manifest length.
pub const MAX_UPDATE_MANIFEST_BYTES: usize = 2048;

const KNOWN_KEYS: [&str; 8] = [
    "format",
    "image",
    "version",
    "counter",
    "channel",
    "publisher",
    "publisher-key",
    "image.sha256",
];

/// Update manifest validation errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// The signed record framing is invalid.
    Frame(FrameError),
    /// The framing version is not supported.
    UnsupportedVersion,
    /// The manifest exceeds the size policy.
    ManifestTooLarge,
    /// A field is missing, duplicated, or malformed.
    Field(FieldError),
    /// A manifest value violates policy.
    Manifest(ManifestError),
    /// The image name is not a safe relative path.
    InvalidImageName,
    /// The rollback counter is not a decimal `u64`.
    InvalidCounter,
    /// The publisher key is not trusted.
    UnknownPublisher,
    /// The publisher name does not match the trusted key.
    PublisherMismatch,
    /// The Ed25519 signature is invalid.
    BadSignature,
    /// The image digest does not match the manifest.
    DigestMismatch,
}

impl From<FrameError> for UpdateError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<FieldError> for UpdateError {
    fn from(error: FieldError) -> Self {
        Self::Field(error)
    }
}

impl From<ManifestError> for UpdateError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

/// A validated, authenticated update manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateManifest<'a> {
    /// Image file name, relative to the release directory.
    pub image: &'a str,
    /// Image version.
    pub version: Version,
    /// Monotonically increasing rollback counter.
    pub counter: u64,
    /// Channel the image belongs to.
    pub channel: Channel,
    /// Publisher display name.
    pub publisher: &'a str,
    /// Publisher public key.
    pub publisher_key: [u8; 32],
    /// SHA-256 of the image bytes.
    pub image_digest: [u8; 32],
}

impl<'a> UpdateManifest<'a> {
    /// Parses and validates manifest fields.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, UpdateError> {
        let fields = Fields::parse(bytes, MAX_UPDATE_MANIFEST_BYTES)?;
        fields.reject_unknown(&KNOWN_KEYS)?;
        if fields.require("format")? != "1" {
            return Err(UpdateError::UnsupportedVersion);
        }
        let image = fields.require("image")?;
        if !is_safe_relative_path(image) {
            return Err(UpdateError::InvalidImageName);
        }
        let version = Version::parse(fields.require("version")?)?;
        let counter_text = fields.require("counter")?;
        if counter_text.is_empty() || !counter_text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(UpdateError::InvalidCounter);
        }
        let counter = counter_text
            .parse()
            .map_err(|_| UpdateError::InvalidCounter)?;
        let channel = Channel::parse(fields.require("channel")?)?;
        let publisher = fields.require("publisher")?;
        if publisher.is_empty() || publisher.len() > aether_packaging::manifest::MAX_PUBLISHER {
            return Err(UpdateError::Manifest(ManifestError::InvalidPublisher));
        }
        let mut publisher_key = [0u8; 32];
        if from_hex(
            fields.require("publisher-key")?.as_bytes(),
            &mut publisher_key,
        ) != Some(32)
        {
            return Err(UpdateError::Manifest(ManifestError::InvalidPublisherKey));
        }
        let mut image_digest = [0u8; 32];
        if from_hex(
            fields.require("image.sha256")?.as_bytes(),
            &mut image_digest,
        ) != Some(32)
        {
            return Err(UpdateError::Manifest(ManifestError::InvalidDigest));
        }
        Ok(Self {
            image,
            version,
            counter,
            channel,
            publisher,
            publisher_key,
            image_digest,
        })
    }

    /// Writes the canonical manifest text.
    pub fn render(&self, out: &mut [u8]) -> Option<usize> {
        let mut writer = Writer::new(out);
        writer.field("format", "1")?;
        writer.field("image", self.image)?;
        let mut version = [0u8; 16];
        let length = self.version.write(&mut version)?;
        writer.field("version", core::str::from_utf8(&version[..length]).ok()?)?;
        let mut counter = [0u8; 20];
        let length = decimal_u64(self.counter, &mut counter);
        writer.field("counter", core::str::from_utf8(&counter[..length]).ok()?)?;
        writer.field("channel", self.channel.name())?;
        writer.field("publisher", self.publisher)?;
        let mut key = [0u8; 64];
        let length = to_hex(&self.publisher_key, &mut key)?;
        writer.field("publisher-key", core::str::from_utf8(&key[..length]).ok()?)?;
        let mut digest = [0u8; 64];
        let length = to_hex(&self.image_digest, &mut digest)?;
        writer.field(
            "image.sha256",
            core::str::from_utf8(&digest[..length]).ok()?,
        )?;
        Some(writer.written)
    }
}

fn decimal_u64(mut value: u64, buffer: &mut [u8; 20]) -> usize {
    let mut index = buffer.len();
    loop {
        index -= 1;
        buffer[index] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let length = buffer.len() - index;
    buffer.copy_within(index.., 0);
    length
}

/// Verifies an update manifest and the image it describes.
pub fn verify_update<'a>(
    manifest_bytes: &'a [u8],
    image: &[u8],
    trust: &TrustStore<'_>,
) -> Result<UpdateManifest<'a>, UpdateError> {
    let frame = decode_frame(manifest_bytes, &UPDATE_MAGIC)?;
    if frame.version != UPDATE_FORMAT {
        return Err(UpdateError::UnsupportedVersion);
    }
    if frame.body.len() > MAX_UPDATE_MANIFEST_BYTES {
        return Err(UpdateError::ManifestTooLarge);
    }
    let manifest = UpdateManifest::parse(frame.body)?;
    let publisher = trust
        .find(&manifest.publisher_key)
        .ok_or(UpdateError::UnknownPublisher)?;
    if publisher.name != manifest.publisher {
        return Err(UpdateError::PublisherMismatch);
    }
    if !verify(&manifest.publisher_key, frame.body, &frame.signature) {
        return Err(UpdateError::BadSignature);
    }
    if sha256(image) != manifest.image_digest {
        return Err(UpdateError::DigestMismatch);
    }
    Ok(manifest)
}

/// State of the currently installed image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstalledState {
    /// Installed version.
    pub version: Version,
    /// Installed rollback counter.
    pub counter: u64,
    /// Installed channel.
    pub channel: Channel,
}

/// Activation policy options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PolicyOptions {
    /// Explicitly allow re-installing or rolling back to an older version
    /// while still requiring a newer counter. Used for a deliberate recovery
    /// action, never for a normal update. Defaults to false.
    pub allow_older_version: bool,
}

/// Policy rejection reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    /// The counter is not greater than the installed counter.
    Rollback,
    /// The version is not newer than the installed version.
    Downgrade,
    /// The update belongs to another channel.
    ChannelMismatch,
}

/// Applies activation policy to an authenticated manifest.
pub fn check_policy(
    manifest: &UpdateManifest<'_>,
    installed: InstalledState,
    options: PolicyOptions,
) -> Result<(), PolicyError> {
    if manifest.counter <= installed.counter {
        return Err(PolicyError::Rollback);
    }
    if manifest.channel != installed.channel {
        return Err(PolicyError::ChannelMismatch);
    }
    if manifest.version <= installed.version && !options.allow_older_version {
        return Err(PolicyError::Downgrade);
    }
    Ok(())
}

struct Writer<'a> {
    out: &'a mut [u8],
    written: usize,
}

impl<'a> Writer<'a> {
    fn new(out: &'a mut [u8]) -> Self {
        Self { out, written: 0 }
    }

    fn field(&mut self, key: &str, value: &str) -> Option<()> {
        let line = self.written.checked_add(key.len() + 1 + value.len() + 1)?;
        let slice = self.out.get_mut(self.written..line)?;
        slice[..key.len()].copy_from_slice(key.as_bytes());
        slice[key.len()] = b'=';
        slice[key.len() + 1..key.len() + 1 + value.len()].copy_from_slice(value.as_bytes());
        slice[key.len() + 1 + value.len()] = b'\n';
        self.written = line;
        Some(())
    }
}

#[cfg(test)]
mod tests;
