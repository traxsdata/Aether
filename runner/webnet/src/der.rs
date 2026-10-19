//! A strict DER reader (X.690): definite, minimal lengths only; one-byte
//! tags; nothing may trail where it shouldn't. Every error is a rejection.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerError;

pub type Result<T> = core::result::Result<T, DerError>;

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const UTF8_STRING: u8 = 0x0c;
pub const PRINTABLE_STRING: u8 = 0x13;
pub const IA5_STRING: u8 = 0x16;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// Context-specific tag [n], constructed.
pub const fn ctx_cons(n: u8) -> u8 {
    0xa0 | n
}
/// Context-specific tag [n], primitive.
pub const fn ctx_prim(n: u8) -> u8 {
    0x80 | n
}

#[derive(Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn rest(&self) -> &'a [u8] {
        self.data
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.data.first().copied()
    }

    /// Reads one element: (tag, contents, the whole encoding).
    pub fn any(&mut self) -> Result<(u8, &'a [u8], &'a [u8])> {
        let d = self.data;
        if d.len() < 2 {
            return Err(DerError);
        }
        let tag = d[0];
        if tag & 0x1f == 0x1f {
            return Err(DerError); // multi-byte tags: never needed here
        }
        let (len, hdr) = if d[1] < 0x80 {
            (d[1] as usize, 2)
        } else {
            let n = (d[1] & 0x7f) as usize;
            if n == 0 || n > 4 || d.len() < 2 + n {
                return Err(DerError); // indefinite or absurd
            }
            if d[2] == 0 {
                return Err(DerError); // not minimal
            }
            let mut l = 0usize;
            for &b in &d[2..2 + n] {
                l = (l << 8) | b as usize;
            }
            if l < 0x80 {
                return Err(DerError); // should have used the short form
            }
            (l, 2 + n)
        };
        if d.len() - hdr < len {
            return Err(DerError);
        }
        let all = &d[..hdr + len];
        let body = &d[hdr..hdr + len];
        self.data = &d[hdr + len..];
        Ok((tag, body, all))
    }

    /// Reads an element that must have tag `tag`; returns its contents.
    pub fn expect(&mut self, tag: u8) -> Result<&'a [u8]> {
        let (t, body, _) = self.any()?;
        if t != tag {
            return Err(DerError);
        }
        Ok(body)
    }

    /// Like `expect`, but returns the whole encoding (header included).
    pub fn expect_raw(&mut self, tag: u8) -> Result<&'a [u8]> {
        let (t, _, all) = self.any()?;
        if t != tag {
            return Err(DerError);
        }
        Ok(all)
    }

    /// Reads the element if the next tag is `tag`.
    pub fn optional(&mut self, tag: u8) -> Result<Option<&'a [u8]>> {
        if self.peek_tag() == Some(tag) {
            self.expect(tag).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn sequence(&mut self) -> Result<Reader<'a>> {
        self.expect(SEQUENCE).map(Reader::new)
    }

    pub fn finish(&self) -> Result<()> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(DerError)
        }
    }

    /// A non-negative INTEGER, as minimal big-endian bytes (no sign byte).
    pub fn uint(&mut self) -> Result<&'a [u8]> {
        let b = self.expect(INTEGER)?;
        parse_uint(b)
    }

    /// A small non-negative INTEGER.
    pub fn small_uint(&mut self) -> Result<u64> {
        let b = self.uint()?;
        if b.len() > 8 {
            return Err(DerError);
        }
        Ok(b.iter().fold(0u64, |a, &x| (a << 8) | x as u64))
    }

    pub fn boolean(&mut self) -> Result<bool> {
        let b = self.expect(BOOLEAN)?;
        match b {
            [0x00] => Ok(false),
            [0xff] => Ok(true),
            _ => Err(DerError),
        }
    }

    /// A BIT STRING with no unused bits (keys, signatures).
    pub fn bit_string_bytes(&mut self) -> Result<&'a [u8]> {
        let b = self.expect(BIT_STRING)?;
        match b.split_first() {
            Some((0, rest)) => Ok(rest),
            _ => Err(DerError),
        }
    }
}

/// Checks INTEGER contents are minimal and non-negative; strips the sign byte.
pub fn parse_uint(b: &[u8]) -> Result<&[u8]> {
    if b.is_empty() {
        return Err(DerError);
    }
    if b[0] & 0x80 != 0 {
        return Err(DerError); // negative
    }
    if b.len() > 1 && b[0] == 0 {
        if b[1] & 0x80 == 0 {
            return Err(DerError); // not minimal
        }
        return Ok(&b[1..]);
    }
    Ok(b)
}

/// Known object identifiers, as their DER contents.
pub mod oid {
    pub const RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
    pub const SHA256_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
    pub const SHA384_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c];
    pub const SHA512_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d];
    pub const RSASSA_PSS: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0a];
    pub const MGF1: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x08];
    pub const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
    pub const P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
    pub const P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
    pub const ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
    pub const ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];
    pub const ECDSA_SHA512: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04];
    pub const SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
    pub const SHA384: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02];
    pub const SHA512: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03];

    pub const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
    pub const EXT_KEY_USAGE_EXT: &[u8] = &[0x55, 0x1d, 0x25];
    pub const KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
    pub const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
    pub const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
    pub const NAME_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x1e];
    pub const CERT_POLICIES: &[u8] = &[0x55, 0x1d, 0x20];
    pub const POLICY_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x24];
    pub const INHIBIT_ANY_POLICY: &[u8] = &[0x55, 0x1d, 0x36];
    pub const SUBJECT_KEY_ID: &[u8] = &[0x55, 0x1d, 0x0e];
    pub const AUTHORITY_KEY_ID: &[u8] = &[0x55, 0x1d, 0x23];
    pub const KP_SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
    pub const ANY_EKU: &[u8] = &[0x55, 0x1d, 0x25, 0x00];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_lengths() {
        assert!(Reader::new(&[0x30, 0x00]).any().is_ok());
        assert!(Reader::new(&[0x30, 0x81, 0x05, 0, 0, 0, 0, 0]).any().is_err()); // long form for < 128
        assert!(Reader::new(&[0x30, 0x80, 0x00, 0x00]).any().is_err()); // indefinite
        assert!(Reader::new(&[0x30, 0x03, 0x01]).any().is_err()); // truncated
        assert!(Reader::new(&[0x30, 0x82, 0x00, 0x80]).any().is_err()); // leading zero length byte
    }

    #[test]
    fn integers() {
        assert_eq!(Reader::new(&[0x02, 0x01, 0x05]).uint().unwrap(), &[5]);
        assert_eq!(Reader::new(&[0x02, 0x02, 0x00, 0x80]).uint().unwrap(), &[0x80]);
        assert!(Reader::new(&[0x02, 0x02, 0x00, 0x05]).uint().is_err());
        assert!(Reader::new(&[0x02, 0x01, 0x80]).uint().is_err());
        assert!(Reader::new(&[0x02, 0x00]).uint().is_err());
        assert!(Reader::new(&[0x01, 0x01, 0x01]).boolean().is_err());
    }
}
