//! X25519 (RFC 7748): a Montgomery ladder over GF(2^255 - 19) with 51-bit
//! limbs. Constant time: the ladder swaps with masks, never branches on
//! the scalar, and inversion is a fixed exponentiation.

#[derive(Clone, Copy)]
struct Fe([u64; 5]);

const MASK: u64 = (1 << 51) - 1;

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    fn from_bytes(b: &[u8; 32]) -> Fe {
        let load = |i: usize| -> u64 {
            let mut x = [0u8; 8];
            x.copy_from_slice(&b[i..i + 8]);
            u64::from_le_bytes(x)
        };
        // The top bit is ignored (RFC 7748 section 5).
        Fe([
            load(0) & MASK,
            (load(6) >> 3) & MASK,
            (load(12) >> 6) & MASK,
            (load(19) >> 1) & MASK,
            (load(24) >> 12) & MASK,
        ])
    }

    fn to_bytes(self) -> [u8; 32] {
        let mut h = self.carry().0;
        // Fully reduce: compute h - p and keep it when it doesn't go negative.
        let mut q = (h[0] + 19) >> 51;
        q = (h[1] + q) >> 51;
        q = (h[2] + q) >> 51;
        q = (h[3] + q) >> 51;
        q = (h[4] + q) >> 51;
        h[0] += 19 * q;
        let mut c = h[0] >> 51;
        h[0] &= MASK;
        h[1] += c;
        c = h[1] >> 51;
        h[1] &= MASK;
        h[2] += c;
        c = h[2] >> 51;
        h[2] &= MASK;
        h[3] += c;
        c = h[3] >> 51;
        h[3] &= MASK;
        h[4] += c;
        h[4] &= MASK;
        let mut out = [0u8; 32];
        let w0 = h[0] | (h[1] << 51);
        let w1 = (h[1] >> 13) | (h[2] << 38);
        let w2 = (h[2] >> 26) | (h[3] << 25);
        let w3 = (h[3] >> 39) | (h[4] << 12);
        out[0..8].copy_from_slice(&w0.to_le_bytes());
        out[8..16].copy_from_slice(&w1.to_le_bytes());
        out[16..24].copy_from_slice(&w2.to_le_bytes());
        out[24..32].copy_from_slice(&w3.to_le_bytes());
        out
    }

    fn carry(self) -> Fe {
        let mut h = self.0;
        let mut c;
        c = h[0] >> 51;
        h[0] &= MASK;
        h[1] += c;
        c = h[1] >> 51;
        h[1] &= MASK;
        h[2] += c;
        c = h[2] >> 51;
        h[2] &= MASK;
        h[3] += c;
        c = h[3] >> 51;
        h[3] &= MASK;
        h[4] += c;
        c = h[4] >> 51;
        h[4] &= MASK;
        h[0] += c * 19;
        c = h[0] >> 51;
        h[0] &= MASK;
        h[1] += c;
        Fe(h)
    }

    fn add(self, o: Fe) -> Fe {
        let mut r = [0u64; 5];
        for i in 0..5 {
            r[i] = self.0[i] + o.0[i];
        }
        Fe(r).carry()
    }

    fn sub(self, o: Fe) -> Fe {
        // Add 2p first so the limbs never underflow (inputs are carried, < 2^52).
        const TWO_P: [u64; 5] = [0xfffffffffffda, 0xffffffffffffe, 0xffffffffffffe, 0xffffffffffffe, 0xffffffffffffe];
        let mut r = [0u64; 5];
        for i in 0..5 {
            r[i] = self.0[i] + TWO_P[i] * 2 - o.0[i];
        }
        Fe(r).carry()
    }

    fn mul(self, o: Fe) -> Fe {
        let a = self.0.map(|x| x as u128);
        let b = o.0.map(|x| x as u128);
        let b19 = [b[0], b[1] * 19, b[2] * 19, b[3] * 19, b[4] * 19];
        let t0 = a[0] * b[0] + a[1] * b19[4] + a[2] * b19[3] + a[3] * b19[2] + a[4] * b19[1];
        let t1 = a[0] * b[1] + a[1] * b[0] + a[2] * b19[4] + a[3] * b19[3] + a[4] * b19[2];
        let t2 = a[0] * b[2] + a[1] * b[1] + a[2] * b[0] + a[3] * b19[4] + a[4] * b19[3];
        let t3 = a[0] * b[3] + a[1] * b[2] + a[2] * b[1] + a[3] * b[0] + a[4] * b19[4];
        let t4 = a[0] * b[4] + a[1] * b[3] + a[2] * b[2] + a[3] * b[1] + a[4] * b[0];
        Self::reduce_wide([t0, t1, t2, t3, t4])
    }

    fn reduce_wide(t: [u128; 5]) -> Fe {
        let m = MASK as u128;
        let mut r = [0u64; 5];
        let mut c: u128;
        
        
        
        
        r[0] = (t[0] & m) as u64;
        c = t[0] >> 51;
        let t1 = t[1] + c;
        r[1] = (t1 & m) as u64;
        c = t1 >> 51;
        let t2 = t[2] + c;
        r[2] = (t2 & m) as u64;
        c = t2 >> 51;
        let t3 = t[3] + c;
        r[3] = (t3 & m) as u64;
        c = t3 >> 51;
        let t4 = t[4] + c;
        r[4] = (t4 & m) as u64;
        c = t4 >> 51;
        let x = r[0] as u128 + c * 19;
        r[0] = (x & m) as u64;
        r[1] += (x >> 51) as u64;
        Fe(r)
    }

    fn square(self) -> Fe {
        self.mul(self)
    }

    fn mul121665(self) -> Fe {
        let mut t = [0u128; 5];
        for i in 0..5 {
            t[i] = self.0[i] as u128 * 121665;
        }
        Self::reduce_wide(t)
    }

    fn sq_n(self, n: usize) -> Fe {
        let mut x = self;
        for _ in 0..n {
            x = x.square();
        }
        x
    }

    /// self^(p-2), by the usual fixed addition chain.
    fn invert(self) -> Fe {
        let z2 = self.square();
        let z9 = z2.sq_n(2).mul(self);
        let z11 = z9.mul(z2);
        let z2_5_0 = z11.square().mul(z9);
        let z2_10_0 = z2_5_0.sq_n(5).mul(z2_5_0);
        let z2_20_0 = z2_10_0.sq_n(10).mul(z2_10_0);
        let z2_40_0 = z2_20_0.sq_n(20).mul(z2_20_0);
        let z2_50_0 = z2_40_0.sq_n(10).mul(z2_10_0);
        let z2_100_0 = z2_50_0.sq_n(50).mul(z2_50_0);
        let z2_200_0 = z2_100_0.sq_n(100).mul(z2_100_0);
        let z2_250_0 = z2_200_0.sq_n(50).mul(z2_50_0);
        z2_250_0.sq_n(5).mul(z11)
    }
}

fn cswap(swap: u64, a: &mut Fe, b: &mut Fe) {
    let mask = 0u64.wrapping_sub(swap);
    for i in 0..5 {
        let t = mask & (a.0[i] ^ b.0[i]);
        a.0[i] ^= t;
        b.0[i] ^= t;
    }
}

/// X25519(scalar, u). Returns the shared u-coordinate.
pub fn x25519(scalar: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut k = *scalar;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    let x1 = Fe::from_bytes(u);
    let mut x2 = Fe::ONE;
    let mut z2 = Fe::ZERO;
    let mut x3 = x1;
    let mut z3 = Fe::ONE;
    let mut swap = 0u64;
    for t in (0..255).rev() {
        let kt = ((k[t >> 3] >> (t & 7)) & 1) as u64;
        swap ^= kt;
        cswap(swap, &mut x2, &mut x3);
        cswap(swap, &mut z2, &mut z3);
        swap = kt;
        let a = x2.add(z2);
        let aa = a.square();
        let b = x2.sub(z2);
        let bb = b.square();
        let e = aa.sub(bb);
        let c = x3.add(z3);
        let d = x3.sub(z3);
        let da = d.mul(a);
        let cb = c.mul(b);
        x3 = da.add(cb).square();
        z3 = x1.mul(da.sub(cb).square());
        x2 = aa.mul(bb);
        z2 = e.mul(aa.add(e.mul121665()));
    }
    cswap(swap, &mut x2, &mut x3);
    cswap(swap, &mut z2, &mut z3);
    super::wipe(&mut k);
    x2.mul(z2.invert()).to_bytes()
}

/// The public key for a private key: X25519(k, 9).
pub fn public_key(private: &[u8; 32]) -> [u8; 32] {
    let mut nine = [0u8; 32];
    nine[0] = 9;
    x25519(private, &nine)
}

/// The shared secret, or None when it is all zeros (a low-order point from
/// the peer; RFC 8446 section 7.4.2 says to abort).
pub fn shared(private: &[u8; 32], peer: &[u8; 32]) -> Option<[u8; 32]> {
    let s = x25519(private, peer);
    let z = s.iter().fold(0u8, |a, &b| a | b);
    if core::hint::black_box(z) == 0 {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    fn a32(s: &str) -> [u8; 32] {
        hex(s).try_into().unwrap()
    }

    #[test]
    fn rfc7748_5_2() {
        let k = a32("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
        let u = a32("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
        assert_eq!(x25519(&k, &u), a32("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552"));
        let k = a32("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d");
        let u = a32("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493");
        assert_eq!(x25519(&k, &u), a32("95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957"));
    }

    #[test]
    fn rfc7748_iterated() {
        let mut k = a32("0900000000000000000000000000000000000000000000000000000000000000");
        let mut u = k;
        for _ in 0..1000 {
            let r = x25519(&k, &u);
            u = k;
            k = r;
        }
        assert_eq!(k, a32("684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51"));
    }

    #[test]
    fn rfc7748_6_1() {
        let a = a32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let b = a32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        let pa = public_key(&a);
        assert_eq!(pa, a32("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"));
        let pb = public_key(&b);
        assert_eq!(pb, a32("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"));
        let s = a32("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(shared(&a, &pb).unwrap(), s);
        assert_eq!(shared(&b, &pa).unwrap(), s);
        assert!(shared(&a, &[0u8; 32]).is_none());
    }
}
