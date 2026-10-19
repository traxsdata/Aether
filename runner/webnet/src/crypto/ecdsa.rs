//! ECDSA signature verification on P-256 and P-384 (FIPS 186-4 / SEC 1).
//! Public data only, so the arithmetic need not be constant-time.

use super::bignum::{self, cmp, from_be, is_zero, Limbs, Mont};
use core::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

struct Params {
    bytes: usize,
    p: &'static str,
    n: &'static str,
    b: &'static str,
    gx: &'static str,
    gy: &'static str,
}

const P256: Params = Params {
    bytes: 32,
    p: "ffffffff00000001000000000000000000000000ffffffffffffffffffffffff",
    n: "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
    b: "5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b",
    gx: "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296",
    gy: "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5",
};

const P384: Params = Params {
    bytes: 48,
    p: "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff",
    n: "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973",
    b: "b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef",
    gx: "aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7",
    gy: "3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f",
};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

struct Ctx {
    bytes: usize,
    fp: Mont,
    fn_: Mont,
    b: Limbs, // Montgomery form
    three: Limbs,
    g: Point,
}

#[derive(Clone)]
struct Point {
    x: Limbs,
    y: Limbs,
    z: Limbs, // Jacobian; z = 0 is the point at infinity
}

impl Ctx {
    fn new(c: Curve) -> Ctx {
        let pr = match c {
            Curve::P256 => &P256,
            Curve::P384 => &P384,
        };
        let k = pr.bytes / 8;
        let fp = Mont::new(from_be(&unhex(pr.p), k).unwrap()).unwrap();
        let fn_ = Mont::new(from_be(&unhex(pr.n), k).unwrap()).unwrap();
        let b = fp.to_mont(&from_be(&unhex(pr.b), k).unwrap());
        let mut three = vec![0u64; k];
        three[0] = 3;
        let three = fp.to_mont(&three);
        let g = Point {
            x: fp.to_mont(&from_be(&unhex(pr.gx), k).unwrap()),
            y: fp.to_mont(&from_be(&unhex(pr.gy), k).unwrap()),
            z: fp.one(),
        };
        Ctx { bytes: pr.bytes, fp, fn_, b, three, g }
    }

    fn on_curve(&self, x: &Limbs, y: &Limbs) -> bool {
        // y^2 = x^3 - 3x + b
        let f = &self.fp;
        let y2 = f.mul(y, y);
        let x3 = f.mul(&f.mul(x, x), x);
        let rhs = f.add(&f.sub(&x3, &f.mul(&self.three, x)), &self.b);
        cmp(&y2, &rhs) == Ordering::Equal
    }

    fn inf(&self) -> Point {
        let k = self.fp.k;
        Point { x: self.fp.one(), y: self.fp.one(), z: vec![0; k] }
    }

    fn double(&self, p: &Point) -> Point {
        let f = &self.fp;
        if is_zero(&p.z) || is_zero(&p.y) {
            return self.inf();
        }
        let delta = f.mul(&p.z, &p.z);
        let gamma = f.mul(&p.y, &p.y);
        let beta = f.mul(&p.x, &gamma);
        let alpha = f.mul(&self.three, &f.mul(&f.sub(&p.x, &delta), &f.add(&p.x, &delta)));
        let beta2 = f.add(&beta, &beta);
        let beta4 = f.add(&beta2, &beta2);
        let beta8 = f.add(&beta4, &beta4);
        let x3 = f.sub(&f.mul(&alpha, &alpha), &beta8);
        let yz = f.add(&p.y, &p.z);
        let z3 = f.sub(&f.sub(&f.mul(&yz, &yz), &gamma), &delta);
        let g2 = f.mul(&gamma, &gamma);
        let g4 = f.add(&g2, &g2);
        let g8 = f.add(&g4, &g4);
        let g8 = f.add(&g8, &g8);
        let y3 = f.sub(&f.mul(&alpha, &f.sub(&beta4, &x3)), &g8);
        Point { x: x3, y: y3, z: z3 }
    }

    fn add(&self, p: &Point, q: &Point) -> Point {
        let f = &self.fp;
        if is_zero(&p.z) {
            return q.clone();
        }
        if is_zero(&q.z) {
            return p.clone();
        }
        let z1z1 = f.mul(&p.z, &p.z);
        let z2z2 = f.mul(&q.z, &q.z);
        let u1 = f.mul(&p.x, &z2z2);
        let u2 = f.mul(&q.x, &z1z1);
        let s1 = f.mul(&f.mul(&p.y, &q.z), &z2z2);
        let s2 = f.mul(&f.mul(&q.y, &p.z), &z1z1);
        let h = f.sub(&u2, &u1);
        let r0 = f.sub(&s2, &s1);
        if is_zero(&h) {
            return if is_zero(&r0) { self.double(p) } else { self.inf() };
        }
        let h2 = f.add(&h, &h);
        let i = f.mul(&h2, &h2);
        let j = f.mul(&h, &i);
        let r = f.add(&r0, &r0);
        let v = f.mul(&u1, &i);
        let x3 = f.sub(&f.sub(&f.mul(&r, &r), &j), &f.add(&v, &v));
        let s1j = f.mul(&s1, &j);
        let y3 = f.sub(&f.mul(&r, &f.sub(&v, &x3)), &f.add(&s1j, &s1j));
        let zz = f.add(&p.z, &q.z);
        let z3 = f.mul(&f.sub(&f.sub(&f.mul(&zz, &zz), &z1z1), &z2z2), &h);
        Point { x: x3, y: y3, z: z3 }
    }

    /// u1*G + u2*Q (Shamir's trick). Scalars are big-endian bytes.
    fn mul2(&self, u1: &[u8], q: &Point, u2: &[u8]) -> Point {
        let gq = self.add(&self.g, q);
        let mut r = self.inf();
        for i in 0..u1.len() {
            for bit in (0..8).rev() {
                r = self.double(&r);
                let a = (u1[i] >> bit) & 1;
                let b = (u2[i] >> bit) & 1;
                r = match (a, b) {
                    (1, 1) => self.add(&r, &gq),
                    (1, 0) => self.add(&r, &self.g),
                    (0, 1) => self.add(&r, q),
                    _ => r,
                };
            }
        }
        r
    }

    fn affine_x(&self, p: &Point) -> Option<Limbs> {
        if is_zero(&p.z) {
            return None;
        }
        let f = &self.fp;
        let zi = f.inv_prime(&p.z);
        let zi2 = f.mul(&zi, &zi);
        Some(f.from_mont(&f.mul(&p.x, &zi2)))
    }
}

/// Verifies an ECDSA signature (r, s as big-endian integers) over `hash`
/// with the uncompressed public key `pubkey` (0x04 || X || Y).
pub fn verify(curve: Curve, pubkey: &[u8], hash: &[u8], r: &[u8], s: &[u8]) -> bool {
    let c = Ctx::new(curve);
    let nb = c.bytes;
    let k = nb / 8;
    if pubkey.len() != 1 + 2 * nb || pubkey[0] != 4 {
        return false;
    }
    let (Some(x), Some(y)) = (from_be(&pubkey[1..1 + nb], k), from_be(&pubkey[1 + nb..], k)) else {
        return false;
    };
    if cmp(&x, &c.fp.m) != Ordering::Less || cmp(&y, &c.fp.m) != Ordering::Less {
        return false;
    }
    let (xm, ym) = (c.fp.to_mont(&x), c.fp.to_mont(&y));
    if !c.on_curve(&xm, &ym) {
        return false;
    }
    let q = Point { x: xm, y: ym, z: c.fp.one() };
    let (Some(r), Some(s)) = (from_be(r, k), from_be(s, k)) else {
        return false;
    };
    let n = &c.fn_;
    if is_zero(&r) || is_zero(&s) || cmp(&r, &n.m) != Ordering::Less || cmp(&s, &n.m) != Ordering::Less {
        return false;
    }
    // e: the leftmost bits of the hash (both orders are whole bytes long).
    let h = if hash.len() > nb { &hash[..nb] } else { hash };
    let e = n.reduce(&from_be(h, k).unwrap());
    let w = n.inv_prime(&n.to_mont(&s));
    let u1 = n.from_mont(&n.mul(&n.to_mont(&e), &w));
    let u2 = n.from_mont(&n.mul(&n.to_mont(&r), &w));
    let pt = c.mul2(&bignum::to_be(&u1, nb), &q, &bignum::to_be(&u2, nb));
    let Some(xr) = c.affine_x(&pt) else {
        return false;
    };
    let v = n.reduce(&xr);
    cmp(&v, &r) == Ordering::Equal
}

/// Splits a DER Ecdsa-Sig-Value (SEQUENCE { r INTEGER, s INTEGER }).
pub fn parse_der_sig(sig: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut outer = crate::der::Reader::new(sig);
    let mut seq = outer.sequence().ok()?;
    outer.finish().ok()?;
    let r = seq.uint().ok()?;
    let s = seq.uint().ok()?;
    seq.finish().ok()?;
    Some((r, s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{hex, Hash, Sha256, Sha384};

    #[test]
    fn generators_are_on_their_curves() {
        for c in [Curve::P256, Curve::P384] {
            let ctx = Ctx::new(c);
            assert!(ctx.on_curve(&ctx.g.x, &ctx.g.y));
            // n * G is the point at infinity: (n-1)G = -G, so (n-1)G + G = O.
            let mut nm1 = ctx.fn_.m.clone();
            nm1[0] -= 1;
            let nb = ctx.bytes;
            let zero = vec![0u8; nb];
            let p = ctx.mul2(&bignum::to_be(&nm1, nb), &ctx.g, &zero);
            let x = ctx.affine_x(&p).unwrap();
            assert_eq!(x, ctx.fp.from_mont(&ctx.g.x));
            assert!(is_zero(&ctx.add(&p, &ctx.g).z));
        }
    }

    #[test]
    fn rfc6979_p256_sha256() {
        // RFC 6979 appendix A.2.5, message "sample", SHA-256.
        let mut pk = vec![4u8];
        pk.extend(hex("60FED4BA255A9D31C961EB74C6356D68C049B8923B61FA6CE669622E60F29FB6"));
        pk.extend(hex("7903FE1008B8BC99A41AE9E95628BC64F2F1B20C2D7E9F5177A3C294D4462299"));
        let r = hex("EFD48B2AACB6A8FD1140DD9CD45E81D69D2C877B56AAF991C34D0EA84EAF3716");
        let s = hex("F7CB1C942D657C41D436C7A1B6E29F65F3E900DBB9AFF4064DC4AB2F843ACDA8");
        let h = Sha256::hash(b"sample");
        assert!(verify(Curve::P256, &pk, &h, &r, &s));
        assert!(!verify(Curve::P256, &pk, &Sha256::hash(b"samplf"), &r, &s));
        let mut s2 = s.clone();
        s2[31] ^= 1;
        assert!(!verify(Curve::P256, &pk, &h, &r, &s2));
        let mut pk2 = pk.clone();
        pk2[64] ^= 1; // off the curve
        assert!(!verify(Curve::P256, &pk2, &h, &r, &s));
    }

    #[test]
    fn rfc6979_p384_sha384() {
        // RFC 6979 appendix A.2.6, message "sample", SHA-384.
        let mut pk = vec![4u8];
        pk.extend(hex("EC3A4E415B4E19A4568618029F427FA5DA9A8BC4AE92E02E06AAE5286B300C64DEF8F0EA9055866064A254515480BC13"));
        pk.extend(hex("8015D9B72D7D57244EA8EF9AC0C621896708A59367F9DFB9F54CA84B3F1C9DB1288B231C3AE0D4FE7344FD2533264720"));
        let r = hex("94EDBB92A5ECB8AAD4736E56C691916B3F88140666CE9FA73D64C4EA95AD133C81A648152E44ACF96E36DD1E80FABE46");
        let s = hex("99EF4AEB15F178CEA1FE40DB2603138F130E740A19624526203B6351D0A3A94FA329C145786E679E7B82C71A38628AC8");
        let h = Sha384::hash(b"sample");
        assert!(verify(Curve::P384, &pk, &h, &r, &s));
        assert!(!verify(Curve::P384, &pk, &Sha384::hash(b"test"), &r, &s));
    }
}
