// SPDX-License-Identifier: Apache-2.0
//! Arithmetic modulo the Ed25519 group order
//! `L = 2^252 + 27742317777372353535851937790883648493`.
//!
//! Only public values are reduced. The implementation favours clarity over
//! constant time: it uses fixed-width 512-bit magnitudes, a single folding
//! step per iteration (`2^252 = -delta mod L`), and canonical comparisons.

use core::cmp::Ordering;

/// The group order limbs. Exposed to tests only.
#[cfg(test)]
pub(crate) const ORDER: [u64; 4] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0x0000000000000000,
    0x1000000000000000,
];

/// `L - 2^252`, the additive inverse of `2^252` modulo `L`.
const DELTA: [u64; 8] = [0x5812631a5cf5d3ed, 0x14def9dea2f79cd6, 0, 0, 0, 0, 0, 0];

const L_WIDE: [u64; 8] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0x0000000000000000,
    0x1000000000000000,
    0,
    0,
    0,
    0,
];

fn to_limbs(bytes: &[u8]) -> [u64; 8] {
    let mut limbs = [0u64; 8];
    for (index, chunk) in bytes.chunks(8).enumerate() {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        limbs[index] = u64::from_le_bytes(word);
    }
    limbs
}

fn from_limbs(limbs: &[u64; 8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let (words, _) = out.as_chunks_mut::<8>();
    for (word, limb) in words.iter_mut().zip(limbs) {
        word.copy_from_slice(&limb.to_le_bytes());
    }
    out
}

fn bit_len(value: &[u64; 8]) -> usize {
    for index in (0..8).rev() {
        if value[index] != 0 {
            return index * 64 + (64 - value[index].leading_zeros() as usize);
        }
    }
    0
}

fn compare(a: &[u64; 8], b: &[u64; 8]) -> Ordering {
    for index in (0..8).rev() {
        match a[index].cmp(&b[index]) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

fn is_zero(value: &[u64; 8]) -> bool {
    value.iter().all(|limb| *limb == 0)
}

fn sub(a: &[u64; 8], b: &[u64; 8]) -> [u64; 8] {
    let mut out = [0u64; 8];
    let mut borrow = 0u128;
    for (slot, (a_limb, b_limb)) in out.iter_mut().zip(a.iter().zip(b)) {
        let wide = u128::from(*a_limb) + (1u128 << 64) - u128::from(*b_limb) - borrow;
        *slot = wide as u64;
        // A set bit 64 means no borrow was needed.
        borrow = 1 - ((wide >> 64) & 1);
    }
    out
}

fn add(a: &[u64; 8], b: &[u64; 8]) -> [u64; 8] {
    let mut out = [0u64; 8];
    let mut carry = 0u128;
    for (slot, (a_limb, b_limb)) in out.iter_mut().zip(a.iter().zip(b)) {
        let wide = u128::from(*a_limb) + u128::from(*b_limb) + carry;
        *slot = wide as u64;
        carry = wide >> 64;
    }
    out
}

/// Multiplies two 512-bit magnitudes, keeping the low 512 bits.
///
/// Inputs are bounded by the call sites: `hi * delta` is below `2^384` and
/// `k * s` is below `2^506`.
fn mul(a: &[u64; 8], b: &[u64; 8]) -> [u64; 8] {
    let mut out = [0u64; 8];
    for (i, a_limb) in a.iter().enumerate() {
        let mut carry = 0u128;
        for (j, b_limb) in b.iter().enumerate() {
            if i + j >= 8 {
                break;
            }
            let value = u128::from(*a_limb) * u128::from(*b_limb) + u128::from(out[i + j]) + carry;
            out[i + j] = value as u64;
            carry = value >> 64;
        }
    }
    out
}

fn shift_right(value: &[u64; 8], bits: usize) -> [u64; 8] {
    let mut out = [0u64; 8];
    let limb_shift = bits / 64;
    let bit_shift = bits % 64;
    for (index, slot) in out.iter_mut().enumerate() {
        let source = index + limb_shift;
        if source >= 8 {
            break;
        }
        let mut word = value[source] >> bit_shift;
        if bit_shift != 0
            && let Some(next) = value.get(source + 1)
        {
            word |= next << (64 - bit_shift);
        }
        *slot = word;
    }
    out
}

fn low_252(value: &[u64; 8]) -> [u64; 8] {
    let mut out = *value;
    out[3] &= (1 << 60) - 1;
    for limb in out.iter_mut().skip(4) {
        *limb = 0;
    }
    out
}

/// Reduces a 512-bit magnitude modulo `L`.
fn reduce(mut magnitude: [u64; 8], mut negative: bool) -> [u64; 8] {
    for _ in 0..8 {
        if bit_len(&magnitude) <= 252 {
            break;
        }
        let high = shift_right(&magnitude, 252);
        let low = low_252(&magnitude);
        let product = mul(&high, &DELTA);
        let (next_negative, next) = match compare(&low, &product) {
            Ordering::Less => (true, sub(&product, &low)),
            _ => (false, sub(&low, &product)),
        };
        negative = if negative {
            !next_negative
        } else {
            next_negative
        };
        magnitude = next;
    }

    if negative {
        if is_zero(&magnitude) {
            return [0u64; 8];
        }
        magnitude = sub(&L_WIDE, &magnitude);
    }
    while compare(&magnitude, &L_WIDE) != Ordering::Less {
        magnitude = sub(&magnitude, &L_WIDE);
    }
    magnitude
}

/// Reduces a 64-byte little-endian value modulo `L`.
#[must_use]
pub(crate) fn reduce_bytes(bytes: &[u8; 64]) -> [u8; 32] {
    from_limbs(&reduce(to_limbs(bytes), false))
}

/// Reduces a 32-byte little-endian value modulo `L`.
#[must_use]
pub(crate) fn reduce_32(bytes: &[u8; 32]) -> [u8; 32] {
    from_limbs(&reduce(to_limbs(bytes), false))
}

/// Returns `true` if a 32-byte little-endian scalar is below `L`.
#[must_use]
pub(crate) fn is_canonical(bytes: &[u8; 32]) -> bool {
    compare(&to_limbs(bytes), &L_WIDE) == Ordering::Less
}

/// Computes `(a + k * s) mod L`.
///
/// All inputs must be canonical (below `L`).
#[must_use]
pub(crate) fn mul_add(a: &[u8; 32], k: &[u8; 32], s: &[u8; 32]) -> [u8; 32] {
    let product = mul(&to_limbs(k), &to_limbs(s));
    let sum = add(&product, &to_limbs(a));
    from_limbs(&reduce(sum, false))
}
