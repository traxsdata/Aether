// SPDX-License-Identifier: Apache-2.0
//! Hash and signature primitives shared by the kernel and release tooling.
//!
//! Aether verifies package and update signatures before it activates anything,
//! so the verifier must run in `no_std` kernel context and must not depend on a
//! platform crypto library. This crate therefore implements SHA-256, SHA-512,
//! and Ed25519 (RFC 8032) directly. It is intentionally small and fully tested
//! on the host against published test vectors and an independent
//! implementation.
//!
//! # Security properties and limits
//!
//! - The verifier is **not constant time**. It only ever processes public data:
//!   public keys, signatures, manifests, and artifact digests. Secret-key
//!   signing helpers are host tooling and must never run on a machine whose
//!   timing is observable to an attacker.
//! - Verification enforces canonical point and scalar encodings and rejects
//!   `S >= L`. Small-order public keys and signatures are not rejected
//!   separately; Aether never derives key agreement from these primitives, so
//!   the RFC 8032 verification equation is sufficient for integrity.
//! - This is not a general-purpose cryptography library. Adding algorithms or
//!   changing encoding rules requires a security review.

#![no_std]

pub mod ed25519;
mod sha256;
mod sha512;

pub use ed25519::{PUBLIC_KEY_LEN, SIGNATURE_LEN, verify};
pub use sha256::{Sha256, sha256};
pub use sha512::{Sha512, sha512};

/// Converts a byte slice to lower-case hexadecimal without allocating.
///
/// The caller provides the destination; it must be at least `2 * input.len()`.
/// Returns the number of bytes written, or `None` if the destination is too
/// small.
#[must_use]
pub fn to_hex(input: &[u8], out: &mut [u8]) -> Option<usize> {
    if out.len() < input.len().checked_mul(2)? {
        return None;
    }
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for (index, byte) in input.iter().enumerate() {
        out[index * 2] = DIGITS[usize::from(byte >> 4)];
        out[index * 2 + 1] = DIGITS[usize::from(byte & 0x0f)];
    }
    Some(input.len() * 2)
}

/// Parses lower-case or upper-case hexadecimal into `out`.
///
/// Returns the number of bytes written. Fails on an odd length, a non-hex
/// character, or a destination that is too small. The parser is deliberately
/// strict so a manifest cannot smuggle alternative encodings past a verifier.
#[must_use]
pub fn from_hex(input: &[u8], out: &mut [u8]) -> Option<usize> {
    if !input.len().is_multiple_of(2) || out.len() < input.len() / 2 {
        return None;
    }
    for (index, pair) in input.as_chunks::<2>().0.iter().enumerate() {
        out[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(input.len() / 2)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
