//! RSA signature verification (RFC 8017): RSASSA-PKCS1-v1_5 and RSASSA-PSS
//! with MGF1, for 2048- to 4096-bit keys. Public data only.

use super::bignum::{self, cmp, from_be, Mont};
use super::{ct_eq, Hash, Sha256, Sha384, Sha512};
use core::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlg {
    Sha256,
    Sha384,
    Sha512,
}

impl HashAlg {
    pub fn hash(self, data: &[u8]) -> Vec<u8> {
        match self {
            HashAlg::Sha256 => Sha256::hash(data),
            HashAlg::Sha384 => Sha384::hash(data),
            HashAlg::Sha512 => Sha512::hash(data),
        }
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(self) -> usize {
        match self {
            HashAlg::Sha256 => 32,
            HashAlg::Sha384 => 48,
            HashAlg::Sha512 => 64,
        }
    }

    fn digest_info_prefix(self) -> &'static [u8] {
        match self {
            HashAlg::Sha256 => &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00,
                0x04, 0x20,
            ],
            HashAlg::Sha384 => &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 0x05, 0x00,
                0x04, 0x30,
            ],
            HashAlg::Sha512 => &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03, 0x05, 0x00,
                0x04, 0x40,
            ],
        }
    }
}

pub const MIN_BITS: usize = 2048;
pub const MAX_BITS: usize = 4096;

/// An RSA public key: modulus and exponent as big-endian bytes.
#[derive(Clone, Debug)]
pub struct PublicKey {
    pub n: Vec<u8>,
    pub e: Vec<u8>,
}

impl PublicKey {
    /// Parses an RSAPublicKey (SEQUENCE { n INTEGER, e INTEGER }).
    pub fn from_der(der: &[u8]) -> Option<PublicKey> {
        let mut outer = crate::der::Reader::new(der);
        let mut seq = outer.sequence().ok()?;
        outer.finish().ok()?;
        let n = seq.uint().ok()?.to_vec();
        let e = seq.uint().ok()?.to_vec();
        seq.finish().ok()?;
        let k = PublicKey { n, e };
        k.check().then_some(k)
    }

    pub fn bits(&self) -> usize {
        let n = &self.n;
        if n.is_empty() {
            return 0;
        }
        8 * n.len() - n[0].leading_zeros() as usize
    }

    fn check(&self) -> bool {
        let bits = self.bits();
        // e: odd, at least 3, at most 32 bits (rules out e = 1 and absurd values).
        let e_ok = !self.e.is_empty()
            && self.e.len() <= 4
            && self.e[self.e.len() - 1] & 1 == 1
            && !(self.e.len() == 1 && self.e[0] < 3);
        (MIN_BITS..=MAX_BITS).contains(&bits) && self.n[self.n.len() - 1] & 1 == 1 && e_ok
    }

    /// s^e mod n as k bytes (k = modulus length), or None if s is out of range.
    fn public_op(&self, sig: &[u8]) -> Option<Vec<u8>> {
        if !self.check() {
            return None;
        }
        let k = self.n.len();
        if sig.len() != k {
            return None;
        }
        let limbs = k.div_ceil(8);
        let n = from_be(&self.n, limbs)?;
        let s = from_be(sig, limbs)?;
        if cmp(&s, &n) != Ordering::Less {
            return None;
        }
        let m = Mont::new(n)?;
        let r = m.from_mont(&m.pow(&m.to_mont(&s), &self.e));
        Some(bignum::to_be(&r, k))
    }

    /// RSASSA-PKCS1-v1_5 over a precomputed hash. The encoding is rebuilt and
    /// compared whole, so no parser sees attacker-controlled padding.
    pub fn verify_pkcs1(&self, alg: HashAlg, hash: &[u8], sig: &[u8]) -> bool {
        if hash.len() != alg.len() {
            return false;
        }
        let Some(em) = self.public_op(sig) else {
            return false;
        };
        let k = em.len();
        let prefix = alg.digest_info_prefix();
        let t_len = prefix.len() + hash.len();
        if k < t_len + 11 {
            return false;
        }
        let mut want = vec![0xffu8; k];
        want[0] = 0;
        want[1] = 1;
        want[k - t_len - 1] = 0;
        want[k - t_len..k - hash.len()].copy_from_slice(prefix);
        want[k - hash.len()..].copy_from_slice(hash);
        ct_eq(&em, &want)
    }

    /// RSASSA-PSS with MGF1 using the same hash, salt length `salt_len`.
    pub fn verify_pss(&self, alg: HashAlg, hash: &[u8], sig: &[u8], salt_len: usize) -> bool {
        let h_len = alg.len();
        if hash.len() != h_len {
            return false;
        }
        let Some(em_full) = self.public_op(sig) else {
            return false;
        };
        let mod_bits = self.bits();
        let em_bits = mod_bits - 1;
        let em_len = em_bits.div_ceil(8);
        // em_full is k bytes; when em_len < k, the first byte must be zero.
        let k = em_full.len();
        if k > em_len && em_full[0] != 0 {
            return false;
        }
        let em = &em_full[k - em_len..];
        if em_len < h_len + salt_len + 2 || em[em_len - 1] != 0xbc {
            return false;
        }
        let db_len = em_len - h_len - 1;
        let masked = &em[..db_len];
        let h = &em[db_len..db_len + h_len];
        let top_bits = 8 * em_len - em_bits;
        let top_mask = if top_bits == 0 { 0u8 } else { 0xffu8 << (8 - top_bits) };
        if masked[0] & top_mask != 0 {
            return false;
        }
        let mask = mgf1(alg, h, db_len);
        let mut db: Vec<u8> = masked.iter().zip(&mask).map(|(a, b)| a ^ b).collect();
        db[0] &= !top_mask;
        let ps_len = db_len - salt_len - 1;
        if db[..ps_len].iter().any(|&b| b != 0) || db[ps_len] != 1 {
            return false;
        }
        let salt = &db[ps_len + 1..];
        let mut m = vec![0u8; 8];
        m.extend_from_slice(hash);
        m.extend_from_slice(salt);
        ct_eq(&alg.hash(&m), h)
    }
}

fn mgf1(alg: HashAlg, seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 64);
    let mut c = 0u32;
    while out.len() < len {
        let mut d = seed.to_vec();
        d.extend_from_slice(&c.to_be_bytes());
        out.extend(alg.hash(&d));
        c += 1;
    }
    out.truncate(len);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    // A 2048-bit key and signatures made once with OpenSSL (openssl genrsa;
    // openssl dgst -sign, with -sigopt rsa_padding_mode:pss for PSS).
    include!("rsa_vectors.rs");

    #[test]
    fn pkcs1_and_pss() {
        let k = PublicKey { n: hex(N), e: hex(E) };
        assert!(k.check());
        let h = HashAlg::Sha256.hash(MSG);
        assert!(k.verify_pkcs1(HashAlg::Sha256, &h, &hex(SIG_PKCS1_SHA256)));
        assert!(!k.verify_pkcs1(HashAlg::Sha256, &HashAlg::Sha256.hash(b"other"), &hex(SIG_PKCS1_SHA256)));
        assert!(!k.verify_pss(HashAlg::Sha256, &h, &hex(SIG_PKCS1_SHA256), 32));
        assert!(k.verify_pss(HashAlg::Sha256, &h, &hex(SIG_PSS_SHA256), 32));
        assert!(!k.verify_pkcs1(HashAlg::Sha256, &h, &hex(SIG_PSS_SHA256)));
        let mut bad = hex(SIG_PSS_SHA256);
        bad[100] ^= 4;
        assert!(!k.verify_pss(HashAlg::Sha256, &h, &bad, 32));
        let h384 = HashAlg::Sha384.hash(MSG);
        assert!(k.verify_pkcs1(HashAlg::Sha384, &h384, &hex(SIG_PKCS1_SHA384)));
        let h512 = HashAlg::Sha512.hash(MSG);
        assert!(k.verify_pss(HashAlg::Sha512, &h512, &hex(SIG_PSS_SHA512), 64));
        // Signature >= n, wrong length.
        assert!(!k.verify_pkcs1(HashAlg::Sha256, &h, &vec![0xff; 256]));
        assert!(!k.verify_pkcs1(HashAlg::Sha256, &h, &hex(SIG_PKCS1_SHA256)[1..]));
    }

    #[test]
    fn key_limits() {
        let k = PublicKey { n: vec![0xff; 128], e: vec![1, 0, 1] };
        assert!(!k.check()); // 1024 bits
        let k = PublicKey { n: vec![0xff; 256], e: vec![1] };
        assert!(!k.check());
    }
}
