// SPDX-License-Identifier: Apache-2.0
//! Application package format and verification policy.
//!
//! This crate defines the `.aether` container and the rules the system applies
//! before an application is installed. It is `no_std` because the kernel runs
//! the same verifier that the release tools use: a package must not pass a
//! host check and fail, or silently pass, on the device.
//!
//! # Format overview
//!
//! A package is a canonical signed record:
//!
//! ```text
//! magic       8 bytes   "AETHERPK"
//! version     u16 LE    currently 1
//! manifest    u32 LE length, then that many UTF-8 bytes
//! payload     u32 LE length, then that many bytes
//! signature   64 bytes  Ed25519 over the manifest bytes
//! ```
//!
//! The manifest is a fixed set of `key=value` lines. The payload is a bounded
//! file archive. The signature covers the manifest, which contains the SHA-256
//! digest of the payload, so one signature authenticates both.
//!
//! # Verification order
//!
//! 1. Bounded framing and UTF-8.
//! 2. Manifest structure and value policy (identity, architecture, runtime,
//!    permissions, limits).
//! 3. Publisher key lookup in the trust store and publisher-name match.
//! 4. Ed25519 signature over the exact manifest bytes.
//! 5. SHA-256 of the payload.
//! 6. Payload archive structure, file names, and entry point.
//!
//! Every step fails closed. A valid signature is evidence about authorship and
//! integrity, not evidence that the software is benign.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod manifest;
pub mod package;
pub mod payload;
pub mod signed;
pub mod text;

pub use manifest::{
    Architecture, Channel, Limits, Manifest, ManifestFields, Permissions, Runtime, Version,
};
pub use package::{PackageError, PackageFile, VerifiedPackage, verify_package};
pub use signed::{TrustStore, TrustedPublisher};

/// Maximum encoded manifest length.
pub const MAX_MANIFEST_BYTES: usize = 4096;
/// Maximum payload length accepted by the prototype verifier.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// Maximum number of files in a payload.
pub const MAX_FILES: usize = 16;
/// Maximum length of a file name inside a payload.
pub const MAX_FILE_NAME: usize = 16;
/// Maximum size of one payload file in the prototype filesystem.
pub const MAX_FILE_BYTES: usize = 4096;
/// Maximum number of manifest fields.
pub const MAX_FIELDS: usize = 16;

/// Largest package that can be framed without allocating.
pub const MAX_PACKAGE_BYTES: usize = 8 + 2 + 4 + MAX_MANIFEST_BYTES + 4 + MAX_PAYLOAD_BYTES + 64;

/// Frame magic for application packages.
pub const PACKAGE_MAGIC: [u8; 8] = *b"AETHERPK";
/// Current package framing version.
pub const PACKAGE_FORMAT: u16 = 1;

#[cfg(test)]
mod tests;
