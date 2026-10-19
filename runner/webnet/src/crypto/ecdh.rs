//! ECDH on the NIST curves P-256 and P-384 (SEC 1, RFC 8446 section
//! 4.2.8.2), for key exchange with servers that don't offer X25519.
//!
//! Constant time: field arithmetic uses fixed-size Montgomery operations
//! with masked corrections, points are added with the complete formulas of
//! Renes, Costello and Batina (2016, algorithm 4 for a = -3), which have no
//! special cases, and the scalar is used in fixed 4-bit windows whose table
//! entry is picked by scanning every entry with masks. Nothing branches on
//! or indexes by the secret scalar; loop bounds depend only on the curve.
//! (The ECDSA code in `ecdsa.rs` is for public data and is not used here.)

use std::sync::OnceLock;

/// Up to 6 limbs (384 bits); a curve uses its first `k`.
type Fe = [u64; 6];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

impl Curve {
    /// Bytes in a scalar or coordinate.
    pub fn bytes(self) -> usize {
        match self {
            Curve::P256 => 32,
            Curve::P384 => 48,
        }
    }

    /// Bytes in an uncompressed point (0x04 || X || Y).
    pub fn point_len(self) -> usize {
        1 + 2 * self.bytes()
    }

    fn params(self) -> &'static Params {
        static P256: OnceLock<Params> = OnceLock::new();
        static P384: OnceLock<Params> = OnceLock::new();
        match self {
            Curve::P256 => P256.get_or_init(|| {
                Params::new(
                    "ffffffff00000001000000000000000000000000ffffffffffffffffffffffff",
                    "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
                    "5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b",
                    "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296",
                    "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5",
                )
            }),
            Curve::P384 => P384.get_or_init(|| {
                Params::new(
                    "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff",
                    "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973",
                    "b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef",
                    "aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7",
                    "3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f",
                )
            }),
        }
    }
}

/// A curve y^2 = x^3 - 3x + b over GF(p), with its Montgomery constants.
/// Built once from public constants; the setup itself need not be constant-time.
struct Params {
    k: usize,
    p: Fe,
    n: Fe,
    /// -p^-1 mod 2^64.
    n0: u64,
    /// R^2 mod p, R = 2^(64k).
    r2: Fe,
    /// b, G in Montgomery form.
    b: Fe,
    gx: Fe,
    gy: Fe,
}

fn hex_fe(s: &str, k: usize) -> Fe {
    let bytes: Vec<u8> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
    from_be(&bytes, k)
}

impl Params {
    fn new(p: &str, n: &str, b: &str, gx: &str, gy: &str) -> Params {
        let k = p.len() / 16;
        let p = hex_fe(p, k);
        let mut inv = 1u64;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(p[0].wrapping_mul(inv)));
        }
        // R^2 mod p by doubling 1, 2 * 64k times (reducing each time).
        let mut r: Fe = [0; 6];
        r[0] = 1;
        for _ in 0..(128 * k) {
            let mut c = 0;
            for x in r.iter_mut().take(k) {
                let nc = *x >> 63;
                *x = (*x << 1) | c;
                c = nc;
            }
            let (d, borrow) = sub_raw(&r, &p, k);
            if c == 1 || borrow == 0 {
                r = d;
            }
        }
        let mut c = Params { k, p, n: hex_fe(n, k), n0: inv.wrapping_neg(), r2: r, b: [0; 6], gx: [0; 6], gy: [0; 6] };
        c.b = c.to_mont(&hex_fe(b, k));
        c.gx = c.to_mont(&hex_fe(gx, k));
        c.gy = c.to_mont(&hex_fe(gy, k));
        c
    }

    fn add(&self, a: &Fe, b: &Fe) -> Fe {
        let (s, c) = add_raw(a, b, self.k);
        self.reduce_once(&s, c)
    }

    fn sub(&self, a: &Fe, b: &Fe) -> Fe {
        let (d, borrow) = sub_raw(a, b, self.k);
        let (e, _) = add_raw(&d, &self.p, self.k);
        select(0u64.wrapping_sub(borrow), &e, &d)
    }

    /// Reduces hi * R + r (known to be < 2p) below p.
    fn reduce_once(&self, r: &Fe, hi: u64) -> Fe {
        let (d, borrow) = sub_raw(r, &self.p, self.k);
        // Keep d when the value was >= p: a carry out of R, or no borrow.
        let use_d = (hi | (borrow ^ 1)) & 1;
        select(0u64.wrapping_sub(use_d), &d, r)
    }

    /// a * b * R^-1 mod p (Montgomery multiplication, CIOS).
    fn mul(&self, a: &Fe, b: &Fe) -> Fe {
        let k = self.k;
        let p = &self.p;
        let mut t = [0u64; 8];
        for i in 0..k {
            let mut c: u128 = 0;
            for j in 0..k {
                let s = t[j] as u128 + a[j] as u128 * b[i] as u128 + c;
                t[j] = s as u64;
                c = s >> 64;
            }
            let s = t[k] as u128 + c;
            t[k] = s as u64;
            t[k + 1] = (s >> 64) as u64;
            let u = t[0].wrapping_mul(self.n0);
            let s = t[0] as u128 + u as u128 * p[0] as u128;
            let mut c = s >> 64;
            for j in 1..k {
                let s = t[j] as u128 + u as u128 * p[j] as u128 + c;
                t[j - 1] = s as u64;
                c = s >> 64;
            }
            let s = t[k] as u128 + c;
            t[k - 1] = s as u64;
            let s2 = t[k + 1] as u128 + (s >> 64);
            t[k] = s2 as u64;
            t[k + 1] = 0;
        }
        let mut r: Fe = [0; 6];
        r[..k].copy_from_slice(&t[..k]);
        self.reduce_once(&r, t[k])
    }

    fn to_mont(&self, a: &Fe) -> Fe {
        self.mul(a, &self.r2)
    }

    fn out_of_mont(&self, a: &Fe) -> Fe {
        self.mul(a, &[1, 0, 0, 0, 0, 0])
    }

    fn one(&self) -> Fe {
        self.to_mont(&[1, 0, 0, 0, 0, 0])
    }

    /// a^(p-2) = a^-1 (Montgomery form in and out). The exponent is
    /// public, and every bit costs the same: square, multiply, select.
    fn inv(&self, a: &Fe) -> Fe {
        let (e, _) = sub_raw(&self.p, &[2, 0, 0, 0, 0, 0], self.k);
        let mut r = self.one();
        for i in (0..64 * self.k).rev() {
            r = self.mul(&r, &r);
            let t = self.mul(&r, a);
            let bit = (e[i / 64] >> (i % 64)) & 1;
            r = select(0u64.wrapping_sub(bit), &t, &r);
        }
        r
    }
}

/// a - b over k limbs and the borrow (0 or 1).
fn sub_raw(a: &Fe, b: &Fe, k: usize) -> (Fe, u64) {
    let mut r: Fe = [0; 6];
    let mut borrow = 0u64;
    for i in 0..k {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        r[i] = d2;
        borrow = (b1 | b2) as u64;
    }
    (r, borrow)
}

/// a + b over k limbs and the carry (0 or 1).
fn add_raw(a: &Fe, b: &Fe, k: usize) -> (Fe, u64) {
    let mut r: Fe = [0; 6];
    let mut carry = 0u64;
    for i in 0..k {
        let (s1, c1) = a[i].overflowing_add(b[i]);
        let (s2, c2) = s1.overflowing_add(carry);
        r[i] = s2;
        carry = (c1 | c2) as u64;
    }
    (r, carry)
}

/// `a` where mask is all ones, `b` where it is zero.
fn select(mask: u64, a: &Fe, b: &Fe) -> Fe {
    let mut r: Fe = [0; 6];
    for i in 0..6 {
        r[i] = (a[i] & mask) | (b[i] & !mask);
    }
    r
}

/// 1 when a is zero, else 0, without a branch.
fn is_zero(a: &Fe) -> u64 {
    let x = a.iter().fold(0, |acc, &w| acc | w);
    1 ^ ((x | x.wrapping_neg()) >> 63)
}

/// Big-endian bytes (exactly 8k of them) to k limbs.
fn from_be(b: &[u8], k: usize) -> Fe {
    let mut r: Fe = [0; 6];
    for i in 0..k {
        let mut w = [0u8; 8];
        w.copy_from_slice(&b[8 * (k - 1 - i)..8 * (k - i)]);
        r[i] = u64::from_be_bytes(w);
    }
    r
}

fn to_be(a: &Fe, k: usize) -> Vec<u8> {
    let mut out = vec![0u8; 8 * k];
    for i in 0..k {
        out[8 * (k - 1 - i)..8 * (k - i)].copy_from_slice(&a[i].to_be_bytes());
    }
    out
}

/// A point in homogeneous projective coordinates (x = X/Z, y = Y/Z), in
/// Montgomery form. The identity is (0 : 1 : 0).
#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
}

impl Point {
    fn identity(c: &Params) -> Point {
        Point { x: [0; 6], y: c.one(), z: [0; 6] }
    }

    /// The complete addition (RCB16 algorithm 4): valid for every pair of
    /// points, including P + P and the identity.
    fn add(&self, q: &Point, c: &Params) -> Point {
        let b = &c.b;
        let (x1, y1, z1) = (&self.x, &self.y, &self.z);
        let (x2, y2, z2) = (&q.x, &q.y, &q.z);
        let mut t0 = c.mul(x1, x2);
        let mut t1 = c.mul(y1, y2);
        let mut t2 = c.mul(z1, z2);
        let mut t3 = c.add(x1, y1);
        let mut t4 = c.add(x2, y2);
        t3 = c.mul(&t3, &t4);
        t4 = c.add(&t0, &t1);
        t3 = c.sub(&t3, &t4);
        t4 = c.add(y1, z1);
        let mut x3 = c.add(y2, z2);
        t4 = c.mul(&t4, &x3);
        x3 = c.add(&t1, &t2);
        t4 = c.sub(&t4, &x3);
        x3 = c.add(x1, z1);
        let mut y3 = c.add(x2, z2);
        x3 = c.mul(&x3, &y3);
        y3 = c.add(&t0, &t2);
        y3 = c.sub(&x3, &y3);
        let mut z3 = c.mul(b, &t2);
        x3 = c.sub(&y3, &z3);
        z3 = c.add(&x3, &x3);
        x3 = c.add(&x3, &z3);
        z3 = c.sub(&t1, &x3);
        x3 = c.add(&t1, &x3);
        y3 = c.mul(b, &y3);
        t1 = c.add(&t2, &t2);
        t2 = c.add(&t1, &t2);
        y3 = c.sub(&y3, &t2);
        y3 = c.sub(&y3, &t0);
        t1 = c.add(&y3, &y3);
        y3 = c.add(&t1, &y3);
        t1 = c.add(&t0, &t0);
        t0 = c.add(&t1, &t0);
        t0 = c.sub(&t0, &t2);
        t1 = c.mul(&t4, &y3);
        t2 = c.mul(&t0, &y3);
        y3 = c.mul(&x3, &z3);
        y3 = c.add(&y3, &t2);
        x3 = c.mul(&t3, &x3);
        x3 = c.sub(&x3, &t1);
        z3 = c.mul(&t4, &z3);
        t1 = c.mul(&t3, &t0);
        z3 = c.add(&z3, &t1);
        Point { x: x3, y: y3, z: z3 }
    }

    fn select(mask: u64, a: &Point, b: &Point) -> Point {
        Point { x: select(mask, &a.x, &b.x), y: select(mask, &a.y, &b.y), z: select(mask, &a.z, &b.z) }
    }

    /// k * self, k a big-endian scalar of the curve's size.
    fn mul(&self, k: &[u8], c: &Params) -> Point {
        let mut table = [Point::identity(c); 16];
        for i in 1..16 {
            table[i] = table[i - 1].add(self, c);
        }
        let mut r = Point::identity(c);
        for i in 0..2 * k.len() {
            for _ in 0..4 {
                r = r.add(&r, c);
            }
            let nib = ((k[i / 2] >> (4 * (1 - i % 2))) & 15) as u64;
            let mut t = Point::identity(c);
            for (j, e) in table.iter().enumerate() {
                let d = j as u64 ^ nib;
                let eq = 0u64.wrapping_sub(1 ^ ((d | d.wrapping_neg()) >> 63));
                t = Point::select(eq, e, &t);
            }
            r = r.add(&t, c);
        }
        r
    }

    /// The affine coordinates (normal form), or None for the identity.
    fn affine(&self, c: &Params) -> Option<(Fe, Fe)> {
        let zi = c.inv(&self.z);
        let xy = (c.out_of_mont(&c.mul(&self.x, &zi)), c.out_of_mont(&c.mul(&self.y, &zi)));
        if core::hint::black_box(is_zero(&self.z)) == 1 {
            None
        } else {
            Some(xy)
        }
    }
}

/// True when the big-endian scalar is a valid private key (1..n-1).
pub fn valid_scalar(curve: Curve, k: &[u8]) -> bool {
    let c = curve.params();
    if k.len() != curve.bytes() {
        return false;
    }
    let v = from_be(k, c.k);
    let ok = sub_raw(&v, &c.n, c.k).1 & (1 ^ is_zero(&v));
    core::hint::black_box(ok) == 1
}

/// A new random private key.
pub fn random_scalar(curve: Curve) -> Vec<u8> {
    let mut k = vec![0u8; curve.bytes()];
    loop {
        super::random(&mut k);
        if valid_scalar(curve, &k) {
            return k;
        }
    }
}

/// The uncompressed public key (0x04 || X || Y) for a private key.
pub fn public_key(curve: Curve, private: &[u8]) -> Vec<u8> {
    assert!(valid_scalar(curve, private), "invalid private key");
    let c = curve.params();
    let g = Point { x: c.gx, y: c.gy, z: c.one() };
    let (x, y) = g.mul(private, c).affine(c).expect("k*G is never the identity for 0 < k < n");
    let mut out = vec![4u8];
    out.extend_from_slice(&to_be(&x, c.k));
    out.extend_from_slice(&to_be(&y, c.k));
    out
}

/// Parses and checks a peer's uncompressed point (public data).
fn decode_point(curve: Curve, peer: &[u8]) -> Option<Point> {
    let c = curve.params();
    let n = curve.bytes();
    if peer.len() != 1 + 2 * n || peer[0] != 4 {
        return None;
    }
    let x = from_be(&peer[1..1 + n], c.k);
    let y = from_be(&peer[1 + n..], c.k);
    if sub_raw(&x, &c.p, c.k).1 == 0 || sub_raw(&y, &c.p, c.k).1 == 0 {
        return None; // a coordinate >= p
    }
    let (xm, ym) = (c.to_mont(&x), c.to_mont(&y));
    // y^2 = x^3 - 3x + b
    let y2 = c.mul(&ym, &ym);
    let x3 = c.mul(&c.mul(&xm, &xm), &xm);
    let three_x = c.add(&c.add(&xm, &xm), &xm);
    let rhs = c.add(&c.sub(&x3, &three_x), &c.b);
    if y2 != rhs {
        return None;
    }
    Some(Point { x: xm, y: ym, z: c.one() })
}

/// The ECDH shared secret (the x coordinate of private * peer), or None
/// when the peer's point is malformed or not on the curve. Both curves
/// have cofactor 1, so every point on the curve other than the identity
/// (which has no uncompressed encoding) has order n.
pub fn shared(curve: Curve, private: &[u8], peer: &[u8]) -> Option<Vec<u8>> {
    if !valid_scalar(curve, private) {
        return None;
    }
    let c = curve.params();
    let q = decode_point(curve, peer)?;
    let (x, _) = q.mul(private, c).affine(c)?;
    Some(to_be(&x, c.k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    #[test]
    fn constants() {
        let c = Curve::P256.params();
        // R mod p = 2^256 - p.
        assert_eq!(c.one(), [1, 0xffff_ffff_0000_0000, 0xffff_ffff_ffff_ffff, 0x0000_0000_ffff_fffe, 0, 0]);
        assert_eq!(c.n0, 1);
        for curve in [Curve::P256, Curve::P384] {
            let c = curve.params();
            let a = c.to_mont(&[12345, 6789, 1, 2, 0, 0]);
            let mut one = [0u64; 6];
            one[0] = 1;
            assert_eq!(c.out_of_mont(&c.mul(&a, &c.inv(&a))), one);
            let mut k1 = vec![0u8; curve.bytes()];
            *k1.last_mut().unwrap() = 1;
            let g = public_key(curve, &k1);
            assert!(decode_point(curve, &g).is_some());
            assert_eq!(&g[1..1 + curve.bytes()], &to_be(&c.out_of_mont(&c.gx), c.k)[..]);
        }
    }

    #[test]
    fn small_multiples() {
        // 2G and 3G on P-256 (SEC 2 / NIST test data), and (n-1)G = -G.
        let mut k = [0u8; 32];
        k[31] = 2;
        let p2 = public_key(Curve::P256, &k);
        assert_eq!(&p2[1..33], &hex("7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978")[..]);
        assert_eq!(&p2[33..], &hex("07775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1")[..]);
        k[31] = 3;
        let p3 = public_key(Curve::P256, &k);
        assert_eq!(&p3[1..33], &hex("5ecbe4d1a6330a44c8f7ef951d4bf165e6c6b721efada985fb41661bc6e7fd6c")[..]);
        for curve in [Curve::P256, Curve::P384] {
            let c = curve.params();
            let mut nm1 = to_be(&c.n, c.k);
            *nm1.last_mut().unwrap() -= 1;
            let p = public_key(curve, &nm1);
            let n = curve.bytes();
            assert_eq!(&p[1..1 + n], &to_be(&c.out_of_mont(&c.gx), c.k)[..]);
            let (neg_y, _) = sub_raw(&c.p, &c.out_of_mont(&c.gy), c.k);
            assert_eq!(&p[1 + n..], &to_be(&neg_y, c.k)[..]);
        }
    }

    #[test]
    fn ecdh_against_openssl() {
        // Keys made with `openssl genpkey`, secrets with `openssl pkeyutl -derive`.
        let cases = [
            (
                Curve::P256,
                "0bc820117fbc3d2d436406cf59943a5202fd72aeb7c148a737b53307c6f3dcdd",
                "046b4a35aac33139bfc541bbec09cd29b65565635ca3658a6b4e6ded38dfeaef61\
                 8cf12c22ea8d4ad2d31ace28e63eef54b5cda9025bba15b82308cba02d204191",
                "bc2e70ec9cd7aa49c1d6c65952f3b88277d53ea64fa885d863659c9f0960936c",
                "043362cd365a762a7968ea782902a278aa9bc0b943d177107494109b90b2429003\
                 098d14e5702fa17ba6a601354d8729ce03124210c799d70edbf4d20d1a1592d0",
                "2487fbfe86415d7b29a46dfba17a79fcc6d7df99b3497836f1849fd3c9207d00",
            ),
            (
                Curve::P384,
                "a6f420648a0e0156bf075c7242fe8a26d6ed9621aa185dcc09b3c7c96823c6c0905b1447735450993c28ecd934f5bdd6",
                "0446cc9cc7131965ead9eacc5ad0c7db4737ba2032fd9db83fbb6d852c19a9bb6b82c6d9658e96722e659373d0b835\
                 1413d4128dc7cbd796289c1856cd10ca52c0396b90c715e69cbf174032a401d033b323807957a59546cb8c203cb2051bcae6",
                "06cc8e854d2f6b3eb2074298823a5fc2be21233703d50b9d83f3d41c993c38718b29a3a17319c6487cb270ee349dc45c",
                "040fa45c29d08af319d7016f6a275f708649f1a20bcf3ea4433797486ee6a0f18dc74f1b253e06d577067e5e1d11e8ee\
                 91510bc97138f3961ce1b1fe3992ab4055377dc900ddbc9e7463ff76969426b9f53a772f4927e064c75614f311f0b60637",
                "3532bf515288a731fbe99e5b56b0d2d2fa8641a6f50decab157d461db5bdf0b7ad3b3108e8c415b745fa46ee292fbc06",
            ),
        ];
        for (curve, a, pa, b, pb, s) in cases {
            let (a, pa, b, pb, s) = (hex(a), hex(pa), hex(b), hex(pb), hex(s));
            assert_eq!(public_key(curve, &a), pa);
            assert_eq!(public_key(curve, &b), pb);
            assert_eq!(shared(curve, &a, &pb), Some(s.clone()));
            assert_eq!(shared(curve, &b, &pa), Some(s));
        }
    }

    #[test]
    fn rejects_bad_points_and_scalars() {
        for curve in [Curve::P256, Curve::P384] {
            let n = curve.bytes();
            let a = random_scalar(curve);
            let b = random_scalar(curve);
            let mut pb = public_key(curve, &b);
            assert_eq!(shared(curve, &a, &pb), shared(curve, &b, &public_key(curve, &a)));
            assert!(shared(curve, &a, &pb[..2 * n]).is_none()); // short
            pb[2 * n] ^= 1; // off the curve
            assert!(shared(curve, &a, &pb).is_none());
            let mut big = vec![0xffu8; 1 + 2 * n];
            big[0] = 4; // coordinates >= p
            assert!(shared(curve, &a, &big).is_none());
            assert!(shared(curve, &a, &vec![0u8; 1 + 2 * n]).is_none());
            assert!(!valid_scalar(curve, &vec![0u8; n]));
            let c = curve.params();
            let order = to_be(&c.n, c.k);
            assert!(!valid_scalar(curve, &order));
            let mut nm1 = order.clone();
            *nm1.last_mut().unwrap() -= 1;
            assert!(valid_scalar(curve, &nm1));
            assert!(!valid_scalar(curve, &a[1..])); // wrong length
        }
    }
}
