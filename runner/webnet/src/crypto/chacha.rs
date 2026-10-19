//! ChaCha20, Poly1305 and the ChaCha20-Poly1305 AEAD (RFC 8439).

use super::{ct_eq, wipe};

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// One 64-byte ChaCha20 block.
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut s = [0u32; 16];
    s[0] = 0x61707865;
    s[1] = 0x3320646e;
    s[2] = 0x79622d32;
    s[3] = 0x6b206574;
    for i in 0..8 {
        s[4 + i] = le32(&key[4 * i..]);
    }
    s[12] = counter;
    for i in 0..3 {
        s[13 + i] = le32(&nonce[4 * i..]);
    }
    let init = s;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

/// XORs the ChaCha20 key stream (starting at block `counter`) into `data`.
pub fn chacha20_xor(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    let mut ctr = counter;
    for chunk in data.chunks_mut(64) {
        let mut ks = chacha20_block(key, ctr, nonce);
        for (d, k) in chunk.iter_mut().zip(ks.iter()) {
            *d ^= k;
        }
        wipe(&mut ks);
        ctr = ctr.wrapping_add(1);
    }
}

/// Poly1305 with 26-bit limbs: no secret-dependent branches.
pub struct Poly1305 {
    r: [u32; 5],
    h: [u32; 5],
    pad: [u32; 4],
    buf: [u8; 16],
    n: usize,
}

impl Poly1305 {
    pub fn new(key: &[u8; 32]) -> Self {
        let r = [
            le32(&key[0..]) & 0x3ffffff,
            (le32(&key[3..]) >> 2) & 0x3ffff03,
            (le32(&key[6..]) >> 4) & 0x3ffc0ff,
            (le32(&key[9..]) >> 6) & 0x3f03fff,
            (le32(&key[12..]) >> 8) & 0x00fffff,
        ];
        let pad = [le32(&key[16..]), le32(&key[20..]), le32(&key[24..]), le32(&key[28..])];
        Poly1305 { r, h: [0; 5], pad, buf: [0; 16], n: 0 }
    }

    fn block(&mut self, m: &[u8], hibit: u32) {
        let [r0, r1, r2, r3, r4] = self.r.map(|x| x as u64);
        let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
        let mut h0 = self.h[0] as u64 + (le32(&m[0..]) & 0x3ffffff) as u64;
        let mut h1 = self.h[1] as u64 + ((le32(&m[3..]) >> 2) & 0x3ffffff) as u64;
        let mut h2 = self.h[2] as u64 + ((le32(&m[6..]) >> 4) & 0x3ffffff) as u64;
        let mut h3 = self.h[3] as u64 + ((le32(&m[9..]) >> 6) & 0x3ffffff) as u64;
        let mut h4 = self.h[4] as u64 + ((le32(&m[12..]) >> 8) | hibit) as u64;

        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let mut d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let mut d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let mut d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let mut d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;

        let mut c = d0 >> 26;
        h0 = d0 & 0x3ffffff;
        d1 += c;
        c = d1 >> 26;
        h1 = d1 & 0x3ffffff;
        d2 += c;
        c = d2 >> 26;
        h2 = d2 & 0x3ffffff;
        d3 += c;
        c = d3 >> 26;
        h3 = d3 & 0x3ffffff;
        d4 += c;
        c = d4 >> 26;
        h4 = d4 & 0x3ffffff;
        h0 += c * 5;
        c = h0 >> 26;
        h0 &= 0x3ffffff;
        h1 += c;
        self.h = [h0 as u32, h1 as u32, h2 as u32, h3 as u32, h4 as u32];
    }

    pub fn update(&mut self, mut data: &[u8]) {
        if self.n > 0 {
            let k = (16 - self.n).min(data.len());
            self.buf[self.n..self.n + k].copy_from_slice(&data[..k]);
            self.n += k;
            data = &data[k..];
            if self.n < 16 {
                return;
            }
            let b = self.buf;
            self.block(&b, 1 << 24);
            self.n = 0;
        }
        while data.len() >= 16 {
            self.block(&data[..16], 1 << 24);
            data = &data[16..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.n = data.len();
    }

    /// Pads what's buffered with zeros to a 16-byte boundary (the AEAD's padding).
    pub fn pad16(&mut self) {
        if self.n > 0 {
            for b in &mut self.buf[self.n..] {
                *b = 0;
            }
            let b = self.buf;
            self.block(&b, 1 << 24);
            self.n = 0;
        }
    }

    pub fn finish(mut self) -> [u8; 16] {
        if self.n > 0 {
            let mut b = [0u8; 16];
            b[..self.n].copy_from_slice(&self.buf[..self.n]);
            b[self.n] = 1;
            self.block(&b, 0);
        }
        let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.h;
        // Full carry.
        let mut c = h1 >> 26;
        h1 &= 0x3ffffff;
        h2 += c;
        c = h2 >> 26;
        h2 &= 0x3ffffff;
        h3 += c;
        c = h3 >> 26;
        h3 &= 0x3ffffff;
        h4 += c;
        c = h4 >> 26;
        h4 &= 0x3ffffff;
        h0 += c * 5;
        c = h0 >> 26;
        h0 &= 0x3ffffff;
        h1 += c;
        // g = h + 5 - 2^130; pick g when it doesn't underflow, without branching.
        let mut g0 = h0.wrapping_add(5);
        c = g0 >> 26;
        g0 &= 0x3ffffff;
        let mut g1 = h1.wrapping_add(c);
        c = g1 >> 26;
        g1 &= 0x3ffffff;
        let mut g2 = h2.wrapping_add(c);
        c = g2 >> 26;
        g2 &= 0x3ffffff;
        let mut g3 = h3.wrapping_add(c);
        c = g3 >> 26;
        g3 &= 0x3ffffff;
        let g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);
        let mask = (g4 >> 31).wrapping_sub(1); // all ones when g4 didn't go negative
        let nmask = !mask;
        h0 = (h0 & nmask) | (g0 & mask);
        h1 = (h1 & nmask) | (g1 & mask);
        h2 = (h2 & nmask) | (g2 & mask);
        h3 = (h3 & nmask) | (g3 & mask);
        h4 = (h4 & nmask) | (g4 & mask);
        // h mod 2^128, plus pad.
        let w0 = (h0 | (h1 << 26)) as u64;
        let w1 = ((h1 >> 6) | (h2 << 20)) as u64;
        let w2 = ((h2 >> 12) | (h3 << 14)) as u64;
        let w3 = ((h3 >> 18) | (h4 << 8)) as u64;
        let mut f = w0 + self.pad[0] as u64;
        let o0 = f as u32;
        f = w1 + self.pad[1] as u64 + (f >> 32);
        let o1 = f as u32;
        f = w2 + self.pad[2] as u64 + (f >> 32);
        let o2 = f as u32;
        f = w3 + self.pad[3] as u64 + (f >> 32);
        let o3 = f as u32;
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&o0.to_le_bytes());
        out[4..8].copy_from_slice(&o1.to_le_bytes());
        out[8..12].copy_from_slice(&o2.to_le_bytes());
        out[12..16].copy_from_slice(&o3.to_le_bytes());
        out
    }
}

impl Drop for Poly1305 {
    fn drop(&mut self) {
        self.r = [0; 5];
        self.pad = [0; 4];
        wipe(&mut self.buf);
    }
}

pub struct ChaCha20Poly1305 {
    key: [u8; 32],
}

impl ChaCha20Poly1305 {
    pub fn new(key: &[u8]) -> Self {
        let mut k = [0u8; 32];
        k.copy_from_slice(key);
        ChaCha20Poly1305 { key: k }
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let mut otk = chacha20_block(&self.key, 0, nonce);
        let mut pk = [0u8; 32];
        pk.copy_from_slice(&otk[..32]);
        wipe(&mut otk);
        let mut p = Poly1305::new(&pk);
        wipe(&mut pk);
        p.update(aad);
        p.pad16();
        p.update(ct);
        p.pad16();
        let mut lens = [0u8; 16];
        lens[..8].copy_from_slice(&(aad.len() as u64).to_le_bytes());
        lens[8..].copy_from_slice(&(ct.len() as u64).to_le_bytes());
        p.update(&lens);
        p.finish()
    }

    /// Encrypts `data` in place and appends the 16-byte tag.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], data: &mut Vec<u8>) {
        chacha20_xor(&self.key, 1, nonce, data);
        let t = self.tag(nonce, aad, data);
        data.extend_from_slice(&t);
    }

    /// Checks the tag at the end of `data` and decrypts in place (the tag is
    /// removed). On failure `data` is left as ciphertext and Err is returned.
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
        chacha20_xor(&self.key, 1, nonce, data);
        Ok(())
    }
}

impl Drop for ChaCha20Poly1305 {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    fn arr<const N: usize>(v: &[u8]) -> [u8; N] {
        v.try_into().unwrap()
    }

    #[test]
    fn block_rfc8439_2_3_2() {
        let key: [u8; 32] = arr(&hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"));
        let nonce: [u8; 12] = arr(&hex("000000090000004a00000000"));
        let b = chacha20_block(&key, 1, &nonce);
        assert_eq!(
            b.to_vec(),
            hex("10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e\
                 d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e")
        );
    }

    #[test]
    fn poly1305_rfc8439_2_5_2() {
        let key: [u8; 32] = arr(&hex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b"));
        let mut p = Poly1305::new(&key);
        p.update(b"Cryptographic Forum Research Group");
        assert_eq!(p.finish().to_vec(), hex("a8061dc1305136c6c22b8baf0c0127a9"));
    }

    #[test]
    fn aead_rfc8439_2_8_2() {
        let key = hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
        let nonce: [u8; 12] = arr(&hex("070000004041424344454647"));
        let aad = hex("50515253c0c1c2c3c4c5c6c7");
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let a = ChaCha20Poly1305::new(&key);
        let mut d = pt.to_vec();
        a.seal(&nonce, &aad, &mut d);
        let n = d.len() - 16;
        assert_eq!(&d[n..], &hex("1ae10b594f09e26a7e902ecbd0600691")[..]);
        assert_eq!(
            &d[..16],
            &hex("d31a8d34648e60db7b86afbc53ef7ec2")[..]
        );
        let mut e = d.clone();
        a.open(&nonce, &aad, &mut e).unwrap();
        assert_eq!(&e[..], &pt[..]);
        // Any flipped bit must fail.
        for i in [0, 10, n - 1, n, n + 15] {
            let mut bad = d.clone();
            bad[i] ^= 1;
            assert!(a.open(&nonce, &aad, &mut bad).is_err());
        }
        let mut bad = d.clone();
        assert!(a.open(&nonce, b"other", &mut bad).is_err());
    }
}
