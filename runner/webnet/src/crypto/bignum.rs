//! Unsigned big integers and Montgomery arithmetic, enough for RSA and
//! ECDSA signature verification. Values are little-endian 64-bit limbs.
//! These work on public data (keys, signatures) and are not constant-time.

use core::cmp::Ordering;

pub type Limbs = Vec<u64>;

/// Big-endian bytes to `k` limbs. None if the value doesn't fit.
pub fn from_be(b: &[u8], k: usize) -> Option<Limbs> {
    let b = strip(b);
    if b.len() > 8 * k {
        return None;
    }
    let mut out = vec![0u64; k];
    for (i, &byte) in b.iter().rev().enumerate() {
        out[i / 8] |= (byte as u64) << (8 * (i % 8));
    }
    Some(out)
}

/// `len` big-endian bytes (the value must fit).
pub fn to_be(a: &[u64], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for i in 0..len {
        let limb = i / 8;
        if limb < a.len() {
            out[len - 1 - i] = (a[limb] >> (8 * (i % 8))) as u8;
        }
    }
    out
}

fn strip(b: &[u8]) -> &[u8] {
    let z = b.iter().take_while(|&&x| x == 0).count();
    &b[z..]
}

pub fn cmp(a: &[u64], b: &[u64]) -> Ordering {
    debug_assert_eq!(a.len(), b.len());
    for i in (0..a.len()).rev() {
        match a[i].cmp(&b[i]) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    Ordering::Equal
}

pub fn is_zero(a: &[u64]) -> bool {
    a.iter().all(|&x| x == 0)
}

/// a -= b, returning the borrow.
pub fn sub_in(a: &mut [u64], b: &[u64]) -> u64 {
    let mut borrow = 0u64;
    for i in 0..a.len() {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        a[i] = d2;
        borrow = (b1 | b2) as u64;
    }
    borrow
}

/// a += b, returning the carry.
pub fn add_in(a: &mut [u64], b: &[u64]) -> u64 {
    let mut carry = 0u64;
    for i in 0..a.len() {
        let (s1, c1) = a[i].overflowing_add(b[i]);
        let (s2, c2) = s1.overflowing_add(carry);
        a[i] = s2;
        carry = (c1 | c2) as u64;
    }
    carry
}

pub fn bit_len(a: &[u64]) -> usize {
    for i in (0..a.len()).rev() {
        if a[i] != 0 {
            return 64 * i + 64 - a[i].leading_zeros() as usize;
        }
    }
    0
}

/// Montgomery arithmetic modulo an odd `m`.
#[derive(Clone)]
pub struct Mont {
    pub m: Limbs,
    pub k: usize,
    n0: u64,
    r2: Limbs,
}

impl Mont {
    /// None if `m` is even or zero.
    pub fn new(m: Limbs) -> Option<Mont> {
        if m.is_empty() || m[0] & 1 == 0 {
            return None;
        }
        let k = m.len();
        // n0 = -m^-1 mod 2^64 (Newton iteration).
        let mut inv = 1u64;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(m[0].wrapping_mul(inv)));
        }
        let n0 = inv.wrapping_neg();
        // R^2 mod m by doubling 1 (2 * 64 * k) times.
        let mut r = vec![0u64; k];
        r[0] = 1;
        let mut tmp = vec![0u64; k];
        for _ in 0..(128 * k) {
            let top = r[k - 1] >> 63;
            let mut c = 0;
            for x in r.iter_mut() {
                let nc = *x >> 63;
                *x = (*x << 1) | c;
                c = nc;
            }
            if top == 1 || cmp(&r, &m) != Ordering::Less {
                tmp.copy_from_slice(&r);
                sub_in(&mut tmp, &m);
                r.copy_from_slice(&tmp);
            }
        }
        Some(Mont { m, k, n0, r2: r })
    }

    /// a * b * R^-1 mod m, for a, b < m.
    pub fn mul(&self, a: &[u64], b: &[u64]) -> Limbs {
        let k = self.k;
        let m = &self.m;
        let mut t = vec![0u64; k + 2];
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
            let s = t[0] as u128 + u as u128 * m[0] as u128;
            let mut c = s >> 64;
            for j in 1..k {
                let s = t[j] as u128 + u as u128 * m[j] as u128 + c;
                t[j - 1] = s as u64;
                c = s >> 64;
            }
            let s = t[k] as u128 + c;
            t[k - 1] = s as u64;
            let s2 = t[k + 1] as u128 + (s >> 64);
            t[k] = s2 as u64;
            t[k + 1] = 0;
        }
        let mut r: Limbs = t[..k].to_vec();
        if t[k] != 0 || cmp(&r, m) != Ordering::Less {
            sub_in(&mut r, m);
        }
        r
    }

    pub fn to_mont(&self, a: &[u64]) -> Limbs {
        self.mul(a, &self.r2)
    }

    pub fn from_mont(&self, a: &[u64]) -> Limbs {
        let mut one = vec![0u64; self.k];
        one[0] = 1;
        self.mul(a, &one)
    }

    pub fn one(&self) -> Limbs {
        let mut one = vec![0u64; self.k];
        one[0] = 1;
        self.to_mont(&one)
    }

    pub fn add(&self, a: &[u64], b: &[u64]) -> Limbs {
        let mut r = a.to_vec();
        let c = add_in(&mut r, b);
        if c != 0 || cmp(&r, &self.m) != Ordering::Less {
            sub_in(&mut r, &self.m);
        }
        r
    }

    pub fn sub(&self, a: &[u64], b: &[u64]) -> Limbs {
        let mut r = a.to_vec();
        if sub_in(&mut r, b) != 0 {
            add_in(&mut r, &self.m);
        }
        r
    }

    /// base^exp (base in Montgomery form; result in Montgomery form).
    /// `exp` is big-endian bytes.
    pub fn pow(&self, base: &[u64], exp: &[u8]) -> Limbs {
        let mut r = self.one();
        for &byte in strip(exp) {
            for bit in (0..8).rev() {
                r = self.mul(&r, &r);
                if (byte >> bit) & 1 == 1 {
                    r = self.mul(&r, base);
                }
            }
        }
        r
    }

    /// The inverse of a (Montgomery form) when m is prime: a^(m-2).
    pub fn inv_prime(&self, a: &[u64]) -> Limbs {
        let mut e = self.m.clone();
        let two = {
            let mut t = vec![0u64; self.k];
            t[0] = 2;
            t
        };
        sub_in(&mut e, &two);
        self.pow(a, &to_be(&e, 8 * self.k))
    }

    /// Reduces any value with at most `k` limbs whose bit length is at most
    /// that of m plus one bit... in general: by repeated subtraction of
    /// shifted m (simple long division; public data only).
    pub fn reduce(&self, a: &[u64]) -> Limbs {
        let mut r = a.to_vec();
        r.resize(self.k.max(a.len()), 0);
        let mb = bit_len(&self.m);
        let mut mm = self.m.clone();
        mm.resize(r.len(), 0);
        let rb = bit_len(&r);
        if rb >= mb {
            let mut shift = rb - mb;
            loop {
                let s = shl(&mm, shift);
                if cmp(&r, &s) != Ordering::Less {
                    sub_in(&mut r, &s);
                }
                if shift == 0 {
                    break;
                }
                shift -= 1;
            }
        }
        r.truncate(self.k);
        r
    }
}

fn shl(a: &[u64], n: usize) -> Limbs {
    let mut r = vec![0u64; a.len()];
    let (w, b) = (n / 64, n % 64);
    for i in (0..a.len()).rev() {
        if i < w {
            break;
        }
        let mut v = a[i - w] << b;
        if b > 0 && i > w {
            v |= a[i - w - 1] >> (64 - b);
        }
        r[i] = v;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mont_small() {
        // m = 2^61 - 1 (prime): compare with u128 arithmetic.
        let m = (1u64 << 61) - 1;
        let mt = Mont::new(vec![m]).unwrap();
        for (a, b) in [(3u64, 5u64), (m - 1, m - 2), (123456789, 987654321)] {
            let r = mt.from_mont(&mt.mul(&mt.to_mont(&[a]), &mt.to_mont(&[b])));
            assert_eq!(r[0] as u128, (a as u128 * b as u128) % m as u128);
        }
        let x = mt.to_mont(&[7]);
        let inv = mt.from_mont(&mt.inv_prime(&x));
        assert_eq!((inv[0] as u128 * 7) % m as u128, 1);
        assert_eq!(mt.reduce(&[m + 5]), vec![5]);
    }

    #[test]
    fn mont_multi_limb() {
        // m = 2^127 - 1 (prime), two limbs.
        let m = vec![u64::MAX, (1u64 << 63) - 1];
        let mt = Mont::new(m.clone()).unwrap();
        let a = vec![0x1234_5678_9abc_def0, 0x0fed_cba9_8765_4321];
        let ai = mt.inv_prime(&mt.to_mont(&a));
        let one = mt.from_mont(&mt.mul(&ai, &mt.to_mont(&a)));
        assert_eq!(one, vec![1, 0]);
        assert_eq!(from_be(&[1, 2], 1).unwrap(), vec![0x0102]);
        assert_eq!(to_be(&[0x0102], 3), vec![0, 1, 2]);
    }
}
