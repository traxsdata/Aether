// SPDX-License-Identifier: Apache-2.0
//! Ed25519 signatures (RFC 8032) over Curve25519.
//!
//! [`verify`] is the only routine the kernel links. [`sign`] and
//! [`public_key_from_seed`] exist for release tooling and tests; the kernel
//! never holds a signing key.

mod field;
mod group;
mod scalar;

use crate::sha512::{Sha512, sha512};
use group::Curve;

/// Length of a public key in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Length of a signature in bytes.
pub const SIGNATURE_LEN: usize = 64;

/// Verifies an Ed25519 signature over `message`.
///
/// Returns `false` for malformed keys or signatures, non-canonical encodings,
/// and `S >= L`. The check uses the RFC 8032 verification equation
/// `[S]B = R + [k]A`.
#[must_use]
pub fn verify(
    public_key: &[u8; PUBLIC_KEY_LEN],
    message: &[u8],
    signature: &[u8; SIGNATURE_LEN],
) -> bool {
    let curve = Curve::new();
    let Some(public_point) = curve.decompress(public_key) else {
        return false;
    };
    // A small-order public key can make the verification equation accept
    // signatures for messages the key holder never signed. Aether only trusts
    // keys pinned in a trust store, but rejecting them keeps the verifier safe
    // if that assumption ever changes.
    if is_small_order(&curve, &public_point) {
        return false;
    }

    let mut encoded_r = [0u8; 32];
    encoded_r.copy_from_slice(&signature[..32]);
    let mut s = [0u8; 32];
    s.copy_from_slice(&signature[32..]);
    if !scalar::is_canonical(&s) {
        return false;
    }

    let mut hasher = Sha512::new();
    hasher.update(&encoded_r);
    hasher.update(public_key);
    hasher.update(message);
    let k = scalar::reduce_bytes(&hasher.finish());

    let left = curve.scalar_mult_base(&s);
    let right = curve.scalar_mult(&public_point, &k);
    let difference = curve.add(&left, &curve.neg(&right));
    curve.compress(&difference) == encoded_r
}

/// Returns `true` if `[8]point` is the identity.
fn is_small_order(curve: &Curve, point: &group::Point) -> bool {
    let mut multiple = *point;
    for _ in 0..3 {
        multiple = curve.double(&multiple);
    }
    curve.compress(&multiple) == curve.compress(&curve.identity())
}

/// Derives the public key for a 32-byte Ed25519 seed.
///
/// This expands secret material. It is intended for host-side release tooling.
#[must_use]
pub fn public_key_from_seed(seed: &[u8; 32]) -> [u8; PUBLIC_KEY_LEN] {
    let (scalar, _) = expand_seed(seed);
    let curve = Curve::new();
    curve.compress(&curve.scalar_mult_base(&scalar))
}

/// Signs `message` with a 32-byte Ed25519 seed.
///
/// This is developer tooling. It is variable time and must not run where an
/// attacker can observe timing. The kernel does not link this routine and
/// never holds a signing key.
#[must_use]
pub fn sign(seed: &[u8; 32], message: &[u8]) -> [u8; SIGNATURE_LEN] {
    let (secret, prefix) = expand_seed(seed);
    let curve = Curve::new();
    let public_key = curve.compress(&curve.scalar_mult_base(&secret));

    let mut hasher = Sha512::new();
    hasher.update(&prefix);
    hasher.update(message);
    let nonce = scalar::reduce_bytes(&hasher.finish());
    let encoded_r = curve.compress(&curve.scalar_mult_base(&nonce));

    let mut hasher = Sha512::new();
    hasher.update(&encoded_r);
    hasher.update(&public_key);
    hasher.update(message);
    let challenge = scalar::reduce_bytes(&hasher.finish());

    let secret_reduced = scalar::reduce_32(&secret);
    let s = scalar::mul_add(&nonce, &challenge, &secret_reduced);

    let mut signature = [0u8; SIGNATURE_LEN];
    signature[..32].copy_from_slice(&encoded_r);
    signature[32..].copy_from_slice(&s);
    signature
}

/// Expands a seed into the clamped secret scalar and the nonce prefix.
fn expand_seed(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let expanded = sha512(seed);
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&expanded[..32]);
    secret[0] &= 248;
    secret[31] &= 127;
    secret[31] |= 64;
    let mut prefix = [0u8; 32];
    prefix.copy_from_slice(&expanded[32..]);
    (secret, prefix)
}

/// Test-only access to internal arithmetic.
///
/// The kernel links only [`verify`]; these helpers prove the field and group
/// constants are correct without exposing them publicly.
#[cfg(test)]
pub(crate) mod testing {
    use super::field::{self};
    use super::group::Curve;
    use super::scalar;

    /// The group order limbs.
    pub(crate) const ORDER: [u64; 4] = scalar::ORDER;

    /// The group order encoded little-endian.
    pub(crate) const ORDER_BYTES: [u8; 32] = {
        let mut out = [0u8; 32];
        let mut limb = 0;
        while limb < 4 {
            let mut byte = 0;
            while byte < 8 {
                out[limb * 8 + byte] = (ORDER[limb] >> (byte * 8)) as u8;
                byte += 1;
            }
            limb += 1;
        }
        out
    };

    /// Reduces a 64-byte value modulo the group order.
    pub(crate) fn reduce(bytes: &[u8; 64]) -> [u8; 32] {
        scalar::reduce_bytes(bytes)
    }

    /// Returns whether a scalar is below the group order.
    pub(crate) fn is_canonical(bytes: &[u8; 32]) -> bool {
        scalar::is_canonical(bytes)
    }

    /// Encodes the base point.
    pub(crate) fn base_encoding() -> [u8; 32] {
        let curve = Curve::new();
        curve.compress(&curve.base())
    }

    /// Encodes `[scalar]B` for a 32-byte scalar.
    pub(crate) fn multiply_base(scalar: &[u8; 32]) -> [u8; 32] {
        let curve = Curve::new();
        curve.compress(&curve.scalar_mult_base(scalar))
    }

    /// Checks the derived curve constants against their defining equations.
    pub(crate) fn constants_ok() -> bool {
        let curve = Curve::new();
        let d_ok = field::equals(
            &field::mul(&curve.d, &field::small(121666)),
            &field::neg(&field::small(121665)),
        );
        let d2_ok = field::equals(&curve.d2, &field::add(&curve.d, &curve.d));
        let sqrt_ok = field::equals(&field::square(&curve.sqrt_m1), &field::neg(&field::one()));
        d_ok && d2_ok && sqrt_ok
    }
}
