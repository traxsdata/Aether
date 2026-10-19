// SPDX-License-Identifier: Apache-2.0
//! Arithmetic in the field `GF(2^255 - 19)`.
//!
//! Elements use five 51-bit limbs. Limbs may temporarily exceed 51 bits after
//! addition or subtraction; every operation reduces them again, and encoding
//! canonicalises the value. The code is not constant time and only processes
//! public group elements and scalars.

/// A field element in radix `2^51`.
pub(crate) type Fe = [u64; 5];

const MASK: u64 = (1 << 51) - 1;

/// `p - 2`, the exponent used for inversion.
const P_MINUS_2: [u8; 32] = {
    let mut bytes = [0xff; 32];
    bytes[0] = 0xeb;
    bytes[31] = 0x7f;
    bytes
};

/// `(p - 5) / 8`, the exponent used for the square root.
pub(crate) const POW_22523: [u8; 32] = {
    let mut bytes = [0xff; 32];
    bytes[0] = 0xfd;
    bytes[31] = 0x0f;
    bytes
};

/// `(p - 1) / 4`, the exponent whose power of two is `sqrt(-1)`.
pub(crate) const SQRT_M1_EXP: [u8; 32] = {
    let mut bytes = [0xff; 32];
    bytes[0] = 0xfb;
    bytes[31] = 0x1f;
    bytes
};

/// Returns zero.
pub(crate) const fn zero() -> Fe {
    [0; 5]
}

/// Returns one.
pub(crate) const fn one() -> Fe {
    [1, 0, 0, 0, 0]
}

/// Returns a small constant.
pub(crate) const fn small(value: u64) -> Fe {
    [value, 0, 0, 0, 0]
}

fn load8(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// Loads a field element from 32 little-endian bytes.
///
/// Bit 255 is ignored; callers that need canonical decoding (point
/// decompression) compare the re-encoded value.
pub(crate) fn from_bytes(bytes: &[u8; 32]) -> Fe {
    [
        load8(&bytes[0..8]) & MASK,
        (load8(&bytes[6..14]) >> 3) & MASK,
        (load8(&bytes[12..20]) >> 6) & MASK,
        (load8(&bytes[19..27]) >> 1) & MASK,
        (load8(&bytes[24..32]) >> 12) & MASK,
    ]
}

/// Carries every limb until all limbs are below `2^51`.
fn carry_all(h: &mut Fe) {
    for _ in 0..8 {
        let mut carry = 0u64;
        for limb in h.iter_mut() {
            let value = *limb + carry;
            carry = value >> 51;
            *limb = value & MASK;
        }
        if carry == 0 {
            return;
        }
        h[0] += carry.wrapping_mul(19);
    }
    // Values produced by this module are far below the bound that would need
    // eight passes; the extra carry keeps the invariant even so.
    let carry = h[0] >> 51;
    h[0] &= MASK;
    h[1] += carry;
}

/// Encodes a field element canonically in 32 little-endian bytes.
pub(crate) fn to_bytes(h: &Fe) -> [u8; 32] {
    let mut t = *h;
    carry_all(&mut t);

    // `q` is one exactly when the value is at least `p`.
    let mut q = (t[0] + 19) >> 51;
    q = (t[1] + q) >> 51;
    q = (t[2] + q) >> 51;
    q = (t[3] + q) >> 51;
    q = (t[4] + q) >> 51;
    t[0] += 19 * q;
    t[1] += t[0] >> 51;
    t[0] &= MASK;
    t[2] += t[1] >> 51;
    t[1] &= MASK;
    t[3] += t[2] >> 51;
    t[2] &= MASK;
    t[4] += t[3] >> 51;
    t[3] &= MASK;
    t[4] &= MASK;

    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&(t[0] | (t[1] << 51)).to_le_bytes());
    out[8..16].copy_from_slice(&((t[1] >> 13) | (t[2] << 38)).to_le_bytes());
    out[16..24].copy_from_slice(&((t[2] >> 26) | (t[3] << 25)).to_le_bytes());
    out[24..32].copy_from_slice(&((t[3] >> 39) | (t[4] << 12)).to_le_bytes());
    out
}

/// Adds two field elements.
pub(crate) fn add(a: &Fe, b: &Fe) -> Fe {
    let mut a = *a;
    let mut b = *b;
    carry_all(&mut a);
    carry_all(&mut b);
    [
        a[0] + b[0],
        a[1] + b[1],
        a[2] + b[2],
        a[3] + b[3],
        a[4] + b[4],
    ]
}

/// Subtracts two field elements.
pub(crate) fn sub(a: &Fe, b: &Fe) -> Fe {
    // Normalise first so every limb is below `2^51`, then add `2p` before
    // subtracting so no limb can wrap.
    let mut a = *a;
    let mut b = *b;
    carry_all(&mut a);
    carry_all(&mut b);
    const TWO_P0: u64 = 2 * (MASK - 18);
    const TWO_P: u64 = 2 * MASK;
    [
        a[0] + TWO_P0 - b[0],
        a[1] + TWO_P - b[1],
        a[2] + TWO_P - b[2],
        a[3] + TWO_P - b[3],
        a[4] + TWO_P - b[4],
    ]
}

/// Negates a field element.
pub(crate) fn neg(a: &Fe) -> Fe {
    sub(&zero(), a)
}

fn reduce_wide(mut t: [u128; 5]) -> Fe {
    for _ in 0..2 {
        let mut carry = t[0] >> 51;
        t[0] &= u128::from(MASK);
        t[1] += carry;
        carry = t[1] >> 51;
        t[1] &= u128::from(MASK);
        t[2] += carry;
        carry = t[2] >> 51;
        t[2] &= u128::from(MASK);
        t[3] += carry;
        carry = t[3] >> 51;
        t[3] &= u128::from(MASK);
        t[4] += carry;
        carry = t[4] >> 51;
        t[4] &= u128::from(MASK);
        t[0] += carry * 19;
    }
    let mut h = [
        t[0] as u64,
        t[1] as u64,
        t[2] as u64,
        t[3] as u64,
        t[4] as u64,
    ];
    let carry = h[0] >> 51;
    h[0] &= MASK;
    h[1] += carry;
    h
}

/// Multiplies two field elements.
pub(crate) fn mul(a: &Fe, b: &Fe) -> Fe {
    let (a0, a1, a2, a3, a4) = (
        u128::from(a[0]),
        u128::from(a[1]),
        u128::from(a[2]),
        u128::from(a[3]),
        u128::from(a[4]),
    );
    let (b0, b1, b2, b3, b4) = (
        u128::from(b[0]),
        u128::from(b[1]),
        u128::from(b[2]),
        u128::from(b[3]),
        u128::from(b[4]),
    );
    reduce_wide([
        a0 * b0 + 19 * (a1 * b4 + a2 * b3 + a3 * b2 + a4 * b1),
        a0 * b1 + a1 * b0 + 19 * (a2 * b4 + a3 * b3 + a4 * b2),
        a0 * b2 + a1 * b1 + a2 * b0 + 19 * (a3 * b4 + a4 * b3),
        a0 * b3 + a1 * b2 + a2 * b1 + a3 * b0 + 19 * (a4 * b4),
        a0 * b4 + a1 * b3 + a2 * b2 + a3 * b1 + a4 * b0,
    ])
}

/// Squares a field element.
pub(crate) fn square(a: &Fe) -> Fe {
    mul(a, a)
}

/// Raises `z` to a 256-bit little-endian exponent, most significant bit first.
pub(crate) fn pow_bytes(z: &Fe, exponent: &[u8; 32]) -> Fe {
    let mut result = one();
    for bit in (0..256).rev() {
        result = square(&result);
        if (exponent[bit / 8] >> (bit % 8)) & 1 == 1 {
            result = mul(&result, z);
        }
    }
    result
}

/// Computes `z^-1`, with `0^-1` defined as `0`.
pub(crate) fn invert(z: &Fe) -> Fe {
    pow_bytes(z, &P_MINUS_2)
}

/// Returns `true` if the canonical encoding has its lowest bit set.
pub(crate) fn is_negative(z: &Fe) -> bool {
    to_bytes(z)[0] & 1 == 1
}

/// Returns `true` if the element is zero.
pub(crate) fn is_zero(z: &Fe) -> bool {
    to_bytes(z) == [0u8; 32]
}

/// Returns `true` if two elements are equal modulo `p`.
pub(crate) fn equals(a: &Fe, b: &Fe) -> bool {
    to_bytes(a) == to_bytes(b)
}
