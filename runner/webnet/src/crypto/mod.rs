//! The cryptography TLS 1.3 and 1.2 need, written from the standards.
//!
//! Secret-dependent code (X25519, P-256 and P-384 ECDH, AES, GHASH,
//! ChaCha20, Poly1305, tag checks) has no secret-dependent branches or
//! table indexes. Signature verification (ECDSA, RSA) works on public
//! data only and is not constant-time.

pub mod aes;
pub mod bignum;
pub mod chacha;
pub mod ecdh;
pub mod ecdsa;
pub mod gcm;
pub mod rsa;
pub mod sha2;
pub mod x25519;

pub use sha2::{Sha256, Sha384, Sha512};

/// A hash function as HMAC and HKDF use it.
pub trait Hash: Clone {
    const OUT: usize;
    const BLOCK: usize;
    fn new() -> Self;
    fn update(&mut self, data: &[u8]);
    fn finish(self) -> Vec<u8>;
    fn hash(data: &[u8]) -> Vec<u8> {
        let mut h = Self::new();
        h.update(data);
        h.finish()
    }
}

/// Compares two byte strings in time that depends only on their lengths.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    // Keep the compiler from turning the loop into an early exit.
    core::hint::black_box(d) == 0
}

/// Overwrites a secret buffer with zeros in a way the optimizer keeps.
pub fn wipe(b: &mut [u8]) {
    for x in b.iter_mut() {
        unsafe { core::ptr::write_volatile(x, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// HMAC (RFC 2104) with any `Hash`.
#[derive(Clone)]
pub struct Hmac<H: Hash> {
    inner: H,
    outer: H,
}

impl<H: Hash> Hmac<H> {
    pub fn new(key: &[u8]) -> Self {
        let mut k = vec![0u8; H::BLOCK];
        if key.len() > H::BLOCK {
            let d = H::hash(key);
            k[..d.len()].copy_from_slice(&d);
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut ipad = k.clone();
        let mut opad = k.clone();
        for b in ipad.iter_mut() {
            *b ^= 0x36;
        }
        for b in opad.iter_mut() {
            *b ^= 0x5c;
        }
        let mut inner = H::new();
        inner.update(&ipad);
        let mut outer = H::new();
        outer.update(&opad);
        wipe(&mut k);
        wipe(&mut ipad);
        wipe(&mut opad);
        Hmac { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finish(self) -> Vec<u8> {
        let ih = self.inner.finish();
        let mut o = self.outer;
        o.update(&ih);
        o.finish()
    }

    pub fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut h = Self::new(key);
        h.update(data);
        h.finish()
    }
}

/// HKDF-Extract (RFC 5869).
pub fn hkdf_extract<H: Hash>(salt: &[u8], ikm: &[u8]) -> Vec<u8> {
    let zero;
    let salt = if salt.is_empty() {
        zero = vec![0u8; H::OUT];
        &zero[..]
    } else {
        salt
    };
    Hmac::<H>::mac(salt, ikm)
}

/// HKDF-Expand (RFC 5869). `len` is at most 255 * H::OUT.
pub fn hkdf_expand<H: Hash>(prk: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    assert!(len <= 255 * H::OUT);
    let mut out = Vec::with_capacity(len);
    let mut t: Vec<u8> = Vec::new();
    let mut i = 1u8;
    while out.len() < len {
        let mut h = Hmac::<H>::new(prk);
        h.update(&t);
        h.update(info);
        h.update(&[i]);
        t = h.finish();
        let k = (len - out.len()).min(t.len());
        out.extend_from_slice(&t[..k]);
        i = i.wrapping_add(1);
    }
    out
}

/// Fills `buf` from the operating system's random generator (the
/// `getrandom` system call, which æther's Linux layer answers from its
/// kernel generator). Panics if no secure randomness is available: TLS
/// must never run on weak randomness.
pub fn random(buf: &mut [u8]) {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let mut done = 0;
        let mut tries = 0;
        while done < buf.len() {
            let r: i64;
            unsafe {
                core::arch::asm!(
                    "syscall",
                    inlateout("rax") 318i64 => r,
                    in("rdi") buf[done..].as_mut_ptr(),
                    in("rsi") buf.len() - done,
                    in("rdx") 0u64,
                    lateout("rcx") _, lateout("r11") _,
                    options(nostack),
                );
            }
            if r > 0 {
                done += r as usize;
            } else {
                tries += 1;
                // -EINTR: try again; anything else is fatal.
                if r != -4 || tries > 100 {
                    panic!("no secure random numbers available (getrandom: {r})");
                }
            }
        }
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(buf))
            .expect("no secure random numbers available");
    }
}

#[cfg(test)]
pub fn hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_rfc4231() {
        // Test case 1 and 2 (RFC 4231 section 4).
        let k = hex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
        assert_eq!(
            Hmac::<Sha256>::mac(&k, b"Hi There"),
            hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
        );
        assert_eq!(
            Hmac::<Sha384>::mac(&k, b"Hi There"),
            hex("afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6")
        );
        assert_eq!(
            Hmac::<Sha256>::mac(b"Jefe", b"what do ya want for nothing?"),
            hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
        // Test case 6: a key longer than the block size.
        let k = vec![0xaau8; 131];
        assert_eq!(
            Hmac::<Sha256>::mac(&k, b"Test Using Larger Than Block-Size Key - Hash Key First"),
            hex("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
        );
    }

    #[test]
    fn hkdf_rfc5869() {
        // Test case 1.
        let ikm = hex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
        let salt = hex("000102030405060708090a0b0c");
        let info = hex("f0f1f2f3f4f5f6f7f8f9");
        let prk = hkdf_extract::<Sha256>(&salt, &ikm);
        assert_eq!(prk, hex("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"));
        assert_eq!(
            hkdf_expand::<Sha256>(&prk, &info, 42),
            hex("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865")
        );
        // Test case 3: no salt, no info.
        let prk = hkdf_extract::<Sha256>(&[], &ikm);
        assert_eq!(
            hkdf_expand::<Sha256>(&prk, &[], 42),
            hex("8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8")
        );
    }

    #[test]
    fn ct_eq_works() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn random_differs() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        random(&mut a);
        random(&mut b);
        assert_ne!(a, b);
    }
}
