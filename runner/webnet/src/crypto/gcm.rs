//! AES-GCM (NIST SP 800-38D) with 12-byte nonces and 16-byte tags.
//!
//! GHASH multiplies with masks (no branches or tables on secret data).

use super::aes::Aes;
use super::{ct_eq, wipe};

const R: u128 = 0xe1 << 120;

/// X * H in GF(2^128), GCM's bit order.
fn gf_mul(x: u128, h: u128) -> u128 {
    let mut z = 0u128;
    let mut v = h;
    for i in 0..128 {
        let bit = (x >> (127 - i)) & 1;
        z ^= v & 0u128.wrapping_sub(bit);
        let lsb = v & 1;
        v = (v >> 1) ^ (R & 0u128.wrapping_sub(lsb));
    }
    z
}

struct Ghash {
    h: u128,
    y: u128,
}

impl Ghash {
    fn update_padded(&mut self, data: &[u8]) {
        for chunk in data.chunks(16) {
            let mut b = [0u8; 16];
            b[..chunk.len()].copy_from_slice(chunk);
            self.y = gf_mul(self.y ^ u128::from_be_bytes(b), self.h);
        }
    }
}

pub struct AesGcm {
    aes: Aes,
    h: u128,
}

impl AesGcm {
    /// A 16-byte (AES-128-GCM) or 32-byte (AES-256-GCM) key.
    pub fn new(key: &[u8]) -> AesGcm {
        let aes = Aes::new(key);
        let mut h = [0u8; 16];
        aes.encrypt_block(&mut h);
        AesGcm { aes, h: u128::from_be_bytes(h) }
    }

    fn ctr(&self, nonce: &[u8; 12], data: &mut [u8]) {
        let mut counter = 2u32;
        for chunk in data.chunks_mut(64) {
            let nb = chunk.len().div_ceil(16);
            let mut ks = [[0u8; 16]; 4];
            for k in ks.iter_mut().take(nb) {
                k[..12].copy_from_slice(nonce);
                k[12..].copy_from_slice(&counter.to_be_bytes());
                counter = counter.wrapping_add(1);
            }
            self.aes.encrypt_blocks(&mut ks[..nb]);
            for (i, d) in chunk.iter_mut().enumerate() {
                *d ^= ks[i / 16][i % 16];
            }
            for k in ks.iter_mut() {
                wipe(k);
            }
        }
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let mut g = Ghash { h: self.h, y: 0 };
        g.update_padded(aad);
        g.update_padded(ct);
        let lens = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
        g.y = gf_mul(g.y ^ lens, g.h);
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;
        self.aes.encrypt_block(&mut j0);
        (g.y ^ u128::from_be_bytes(j0)).to_be_bytes()
    }

    /// Encrypts `data` in place and appends the tag.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], data: &mut Vec<u8>) {
        self.ctr(nonce, data);
        let t = self.tag(nonce, aad, data);
        data.extend_from_slice(&t);
    }

    /// Checks the tag and decrypts in place (removing the tag).
    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &mut Vec<u8>) -> Result<(), ()> {
        if data.len() < 16 {
            return Err(());
        }
        let n = data.len() - 16;
        let t = self.tag(nonce, aad, &data[..n]);
        if !ct_eq(&t, &data[n..]) {
            return Err(());
        }
        data.truncate(n);
        self.ctr(nonce, data);
        Ok(())
    }
}

impl Drop for AesGcm {
    fn drop(&mut self) {
        self.h = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    fn check(key: &str, iv: &str, pt: &str, aad: &str, ct: &str, tag: &str) {
        let g = AesGcm::new(&hex(key));
        let nonce: [u8; 12] = hex(iv).try_into().unwrap();
        let mut d = hex(pt);
        g.seal(&nonce, &hex(aad), &mut d);
        let mut want = hex(ct);
        want.extend(hex(tag));
        assert_eq!(d, want);
        g.open(&nonce, &hex(aad), &mut d).unwrap();
        assert_eq!(d, hex(pt));
        let mut bad = want.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0x80;
        assert!(g.open(&nonce, &hex(aad), &mut bad).is_err());
    }

    // Test cases from "The Galois/Counter Mode of Operation (GCM)",
    // McGrew and Viega (the NIST submission), appendix B.
    #[test]
    fn gcm_spec_aes128() {
        check("00000000000000000000000000000000", "000000000000000000000000", "", "", "", "58e2fccefa7e3061367f1d57a4e7455a");
        check(
            "00000000000000000000000000000000",
            "000000000000000000000000",
            "00000000000000000000000000000000",
            "",
            "0388dace60b6a392f328c2b971b2fe78",
            "ab6e47d42cec13bdf53a67b21257bddf",
        );
        check(
            "feffe9928665731c6d6a8f9467308308",
            "cafebabefacedbaddecaf888",
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255",
            "",
            "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091473f5985",
            "4d5c2af327cd64a62cf35abd2ba6fab4",
        );
        check(
            "feffe9928665731c6d6a8f9467308308",
            "cafebabefacedbaddecaf888",
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
            "feedfacedeadbeeffeedfacedeadbeefabaddad2",
            "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091",
            "5bc94fbc3221a5db94fae95ae7121a47",
        );
    }

    #[test]
    fn gcm_spec_aes256() {
        check(
            "0000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000",
            "",
            "",
            "",
            "530f8afbc74536b9a963b4f1c4cb738b",
        );
        check(
            "feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308",
            "cafebabefacedbaddecaf888",
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
            "feedfacedeadbeeffeedfacedeadbeefabaddad2",
            "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662",
            "76fc6ece0f4e1768cddf8853bb2d551b",
        );
    }
}
