// SPDX-License-Identifier: Apache-2.0
//! The Edwards curve `-x^2 + y^2 = 1 + d x^2 y^2` over `GF(2^255 - 19)`.
//!
//! Points use extended coordinates `(X:Y:Z:T)` with `x = X/Z`, `y = Y/Z`, and
//! `T = XY/Z`. The formulas are the standard "extended coordinates" addition
//! and doubling for `a = -1` (Hisil et al., 2008). Operations are variable
//! time; they only handle public data.

use super::field::{self, Fe};

/// Curve constants derived from the field parameters.
pub(crate) struct Curve {
    pub(crate) d: Fe,
    pub(crate) d2: Fe,
    pub(crate) sqrt_m1: Fe,
}

/// A point in extended coordinates.
#[derive(Clone, Copy)]
pub(crate) struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

impl Curve {
    /// Derives `d`, `2d`, and `sqrt(-1)`.
    ///
    /// The constants are computed instead of transcribed so a copy error cannot
    /// silently weaken verification; the tests assert their defining equations.
    #[must_use]
    pub(crate) fn new() -> Self {
        let d = field::neg(&field::mul(
            &field::small(121665),
            &field::invert(&field::small(121666)),
        ));
        let sqrt_m1 = field::pow_bytes(&field::small(2), &field::SQRT_M1_EXP);
        Self {
            d,
            d2: field::add(&d, &d),
            sqrt_m1,
        }
    }

    /// Returns the standard base point.
    #[must_use]
    pub(crate) fn base(&self) -> Point {
        let y = field::mul(&field::small(4), &field::invert(&field::small(5)));
        let mut encoded = field::to_bytes(&y);
        encoded[31] &= 0x7f;
        match self.decompress(&encoded) {
            Some(point) => point,
            None => unreachable_point(),
        }
    }

    /// Decodes a compressed point, rejecting non-canonical encodings and
    /// values that are not on the curve.
    pub(crate) fn decompress(&self, bytes: &[u8; 32]) -> Option<Point> {
        let mut encoded_y = *bytes;
        let sign = encoded_y[31] >> 7;
        encoded_y[31] &= 0x7f;
        let y = field::from_bytes(&encoded_y);
        if field::to_bytes(&y) != encoded_y {
            return None;
        }

        let y2 = field::square(&y);
        let u = field::sub(&y2, &field::one());
        let v = field::add(&field::mul(&self.d, &y2), &field::one());
        let v3 = field::mul(&field::square(&v), &v);
        let v7 = field::mul(&field::square(&v3), &v);
        let mut x = field::mul(
            &field::mul(&u, &v3),
            &field::pow_bytes(&field::mul(&u, &v7), &field::POW_22523),
        );
        let check = field::mul(&v, &field::square(&x));
        if field::equals(&check, &u) {
            // x is a square root of u/v.
        } else if field::equals(&check, &field::neg(&u)) {
            x = field::mul(&x, &self.sqrt_m1);
        } else {
            return None;
        }
        if field::is_negative(&x) != (sign == 1) {
            x = field::neg(&x);
        }
        if field::is_zero(&x) && sign == 1 {
            return None;
        }

        Some(Point {
            x,
            y,
            z: field::one(),
            t: field::mul(&x, &y),
        })
    }

    /// Adds two points.
    pub(crate) fn add(&self, a: &Point, b: &Point) -> Point {
        let a_xy = field::sub(&a.y, &a.x);
        let b_xy = field::sub(&b.y, &b.x);
        let a_sum = field::add(&a.y, &a.x);
        let b_sum = field::add(&b.y, &b.x);

        let aa = field::mul(&a_xy, &b_xy);
        let bb = field::mul(&a_sum, &b_sum);
        let cc = field::mul(&field::mul(&a.t, &self.d2), &b.t);
        let dd = field::mul(&field::add(&a.z, &a.z), &b.z);

        let e = field::sub(&bb, &aa);
        let f = field::sub(&dd, &cc);
        let g = field::add(&dd, &cc);
        let h = field::add(&bb, &aa);

        Point {
            x: field::mul(&e, &f),
            y: field::mul(&g, &h),
            z: field::mul(&f, &g),
            t: field::mul(&e, &h),
        }
    }

    /// Doubles a point.
    pub(crate) fn double(&self, p: &Point) -> Point {
        let a = field::square(&p.x);
        let b = field::square(&p.y);
        let c = field::add(&field::square(&p.z), &field::square(&p.z));
        let d = field::neg(&a);
        let sum = field::square(&field::add(&p.x, &p.y));
        let e = field::sub(&field::sub(&sum, &a), &b);
        let g = field::add(&d, &b);
        let f = field::sub(&g, &c);
        let h = field::sub(&d, &b);

        Point {
            x: field::mul(&e, &f),
            y: field::mul(&g, &h),
            z: field::mul(&f, &g),
            t: field::mul(&e, &h),
        }
    }

    /// Negates a point.
    pub(crate) fn neg(&self, p: &Point) -> Point {
        Point {
            x: field::neg(&p.x),
            y: p.y,
            z: p.z,
            t: field::neg(&p.t),
        }
    }

    /// Multiplies `point` by a 256-bit little-endian scalar.
    pub(crate) fn scalar_mult(&self, point: &Point, scalar: &[u8; 32]) -> Point {
        let mut result = self.identity();
        for bit in (0..256).rev() {
            result = self.double(&result);
            if (scalar[bit / 8] >> (bit % 8)) & 1 == 1 {
                result = self.add(&result, point);
            }
        }
        result
    }

    /// Multiplies the base point by a scalar.
    pub(crate) fn scalar_mult_base(&self, scalar: &[u8; 32]) -> Point {
        self.scalar_mult(&self.base(), scalar)
    }

    /// Returns the identity element.
    #[must_use]
    pub(crate) fn identity(&self) -> Point {
        Point {
            x: field::zero(),
            y: field::one(),
            z: field::one(),
            t: field::zero(),
        }
    }

    /// Encodes a point in compressed form.
    #[must_use]
    pub(crate) fn compress(&self, point: &Point) -> [u8; 32] {
        let z_inverse = field::invert(&point.z);
        let x = field::mul(&point.x, &z_inverse);
        let y = field::mul(&point.y, &z_inverse);
        let mut encoded = field::to_bytes(&y);
        if field::is_negative(&x) {
            encoded[31] |= 0x80;
        }
        encoded
    }
}

fn unreachable_point() -> ! {
    // The base point is a fixed curve constant; a failure here means the field
    // implementation is broken, which the host tests detect first.
    panic!("aether-crypto: base point is not on the curve")
}
