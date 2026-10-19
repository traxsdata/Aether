//! AES-128 and AES-256 (FIPS 197), encryption only (all GCM needs).
//!
//! Constant time: there is no S-box table. SubBytes is computed for up to
//! 64 bytes at once in bitsliced form, as the multiplicative inverse in
//! GF(2^8) (x^254, a fixed chain of multiplications) followed by the affine
//! map. Everything else is byte arithmetic without secret-dependent indexes
//! or branches.

type Planes = [u64; 8];

fn to_planes(bytes: &[u8]) -> Planes {
    let mut p = [0u64; 8];
    for (j, &b) in bytes.iter().enumerate() {
        for (i, pl) in p.iter_mut().enumerate() {
            *pl |= (((b >> i) & 1) as u64) << j;
        }
    }
    p
}

fn from_planes(p: &Planes, bytes: &mut [u8]) {
    for (j, out) in bytes.iter_mut().enumerate() {
        let mut b = 0u8;
        for (i, pl) in p.iter().enumerate() {
            b |= (((pl >> j) & 1) as u8) << i;
        }
        *out = b;
    }
}

/// Bitsliced multiplication in GF(2^8) modulo x^8 + x^4 + x^3 + x + 1.
fn gf_mul(a: &Planes, b: &Planes) -> Planes {
    let mut p = [0u64; 15];
    for i in 0..8 {
        for j in 0..8 {
            p[i + j] ^= a[i] & b[j];
        }
    }
    for k in (8..15).rev() {
        let t = p[k];
        p[k - 8] ^= t;
        p[k - 7] ^= t;
        p[k - 5] ^= t;
        p[k - 4] ^= t;
    }
    let mut r = [0u64; 8];
    r.copy_from_slice(&p[..8]);
    r
}

/// The AES S-box applied to every byte of `bytes` (at most 64).
pub fn sub_bytes(bytes: &mut [u8]) {
    debug_assert!(bytes.len() <= 64);
    let x = to_planes(bytes);
    let x2 = gf_mul(&x, &x);
    let x3 = gf_mul(&x2, &x);
    let x6 = gf_mul(&x3, &x3);
    let x12 = gf_mul(&x6, &x6);
    let x15 = gf_mul(&x12, &x3);
    let mut t = x15;
    for _ in 0..4 {
        t = gf_mul(&t, &t); // x^240 after four squarings
    }
    let x252 = gf_mul(&t, &x12);
    let inv = gf_mul(&x252, &x2); // x^254 = x^-1 (and 0 -> 0)
    let mut s = [0u64; 8];
    for i in 0..8 {
        s[i] = inv[i] ^ inv[(i + 7) % 8] ^ inv[(i + 6) % 8] ^ inv[(i + 5) % 8] ^ inv[(i + 4) % 8];
        if (0x63 >> i) & 1 == 1 {
            s[i] = !s[i];
        }
    }
    from_planes(&s, bytes);
}

fn xtime(b: u8) -> u8 {
    (b << 1) ^ (0x1b & 0u8.wrapping_sub(b >> 7))
}

pub struct Aes {
    rk: Vec<[u8; 16]>,
}

impl Aes {
    /// A 16-byte (AES-128) or 32-byte (AES-256) key.
    pub fn new(key: &[u8]) -> Aes {
        let nk = key.len() / 4;
        assert!(key.len() == 16 || key.len() == 32, "AES key must be 16 or 32 bytes");
        let nr = nk + 6;
        let total = 4 * (nr + 1);
        let mut w: Vec<[u8; 4]> = Vec::with_capacity(total);
        for i in 0..nk {
            w.push([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
        }
        let mut rcon = 1u8;
        for i in nk..total {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [t[1], t[2], t[3], t[0]];
                sub_bytes(&mut t);
                t[0] ^= rcon;
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                sub_bytes(&mut t);
            }
            let p = w[i - nk];
            w.push([p[0] ^ t[0], p[1] ^ t[1], p[2] ^ t[2], p[3] ^ t[3]]);
        }
        let mut rk = Vec::with_capacity(nr + 1);
        for r in 0..=nr {
            let mut k = [0u8; 16];
            for c in 0..4 {
                k[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
            rk.push(k);
        }
        for x in w.iter_mut() {
            super::wipe(x);
        }
        Aes { rk }
    }

    /// Encrypts up to four blocks in place, sharing the bitsliced S-box.
    fn encrypt4(&self, blocks: &mut [[u8; 16]]) {
        debug_assert!(blocks.len() <= 4);
        let n = blocks.len();
        let nr = self.rk.len() - 1;
        for b in blocks.iter_mut() {
            for i in 0..16 {
                b[i] ^= self.rk[0][i];
            }
        }
        let mut buf = [0u8; 64];
        for round in 1..=nr {
            for (k, b) in blocks.iter().enumerate() {
                buf[16 * k..16 * k + 16].copy_from_slice(b);
            }
            sub_bytes(&mut buf[..16 * n]);
            for (k, b) in blocks.iter_mut().enumerate() {
                let s = &buf[16 * k..16 * k + 16];
                // ShiftRows: state byte (row r, column c) is s[4c + r].
                let mut t = [0u8; 16];
                for c in 0..4 {
                    for r in 0..4 {
                        t[4 * c + r] = s[4 * ((c + r) % 4) + r];
                    }
                }
                if round != nr {
                    for c in 0..4 {
                        let a = [t[4 * c], t[4 * c + 1], t[4 * c + 2], t[4 * c + 3]];
                        let all = a[0] ^ a[1] ^ a[2] ^ a[3];
                        for r in 0..4 {
                            t[4 * c + r] = a[r] ^ all ^ xtime(a[r] ^ a[(r + 1) % 4]);
                        }
                    }
                }
                for i in 0..16 {
                    b[i] = t[i] ^ self.rk[round][i];
                }
            }
        }
        super::wipe(&mut buf);
    }

    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        self.encrypt4(core::slice::from_mut(block));
    }

    pub fn encrypt_blocks(&self, blocks: &mut [[u8; 16]]) {
        for chunk in blocks.chunks_mut(4) {
            self.encrypt4(chunk);
        }
    }
}

impl Drop for Aes {
    fn drop(&mut self) {
        for k in self.rk.iter_mut() {
            super::wipe(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    #[test]
    fn sbox_values() {
        // Spot values of the FIPS 197 S-box (figure 7).
        let mut b = [0x00u8, 0x01, 0x53, 0xff, 0x10, 0xc9, 0x7c, 0x9a];
        sub_bytes(&mut b);
        assert_eq!(b, [0x63, 0x7c, 0xed, 0x16, 0xca, 0xdd, 0x10, 0xb8]);
        // It is a permutation.
        let mut all: Vec<u8> = (0..=255u8).collect();
        for c in all.chunks_mut(64) {
            sub_bytes(c);
        }
        let mut seen = [false; 256];
        for &x in &all {
            assert!(!seen[x as usize]);
            seen[x as usize] = true;
        }
    }

    #[test]
    fn fips197_appendix_c() {
        let pt: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        let a = Aes::new(&hex("000102030405060708090a0b0c0d0e0f"));
        let mut b = pt;
        a.encrypt_block(&mut b);
        assert_eq!(b.to_vec(), hex("69c4e0d86a7b0430d8cdb78070b4c55a"));
        let a = Aes::new(&hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"));
        let mut b = pt;
        a.encrypt_block(&mut b);
        assert_eq!(b.to_vec(), hex("8ea2b7ca516745bfeafc49904b496089"));
        // Batched gives the same as one at a time.
        let mut many = [pt; 6];
        a.encrypt_blocks(&mut many);
        for m in many {
            assert_eq!(m, b);
        }
    }

    #[test]
    fn fips197_appendix_b() {
        let a = Aes::new(&hex("2b7e151628aed2a6abf7158809cf4f3c"));
        let mut b: [u8; 16] = hex("3243f6a8885a308d313198a2e0370734").try_into().unwrap();
        a.encrypt_block(&mut b);
        assert_eq!(b.to_vec(), hex("3925841d02dc09fbdc118597196a0b32"));
    }
}
