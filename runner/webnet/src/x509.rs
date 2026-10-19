//! X.509 certificates (RFC 5280): a strict parser and the checks a TLS
//! client needs: signatures, validity dates, basicConstraints, keyUsage,
//! extKeyUsage, name constraints (DNS and IP), host name matching
//! (RFC 6125) and building a path to a trust anchor.
//!
//! Anything not understood is rejected: an unknown critical extension, a
//! duplicate extension, an unsupported algorithm or key size, trailing data.

use crate::crypto::ecdsa::{self, Curve};
use crate::crypto::rsa::{self, HashAlg};
use crate::der::{self, oid, Reader};
use crate::roots::{RootStore, TrustAnchor};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertError {
    /// The certificate couldn't be parsed (or uses something unsupported).
    BadEncoding(&'static str),
    Expired,
    NotYetValid,
    /// The certificate isn't valid for the host we connected to.
    WrongHost(String),
    /// No path to a trusted root.
    UnknownIssuer,
    BadSignature,
    /// An issuing certificate isn't allowed to issue (not a CA, path length,
    /// key usage, extended key usage, or name constraints).
    NotAllowed(&'static str),
    UnsupportedKey,
    NoCertificate,
}

impl fmt::Display for CertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CertError::BadEncoding(w) => write!(f, "the certificate is malformed or unsupported ({w})"),
            CertError::Expired => write!(f, "the certificate has expired"),
            CertError::NotYetValid => write!(f, "the certificate is not valid yet (or this computer's clock is wrong)"),
            CertError::WrongHost(h) => write!(f, "the certificate is not valid for {h}"),
            CertError::UnknownIssuer => write!(f, "the certificate was not issued by a trusted authority"),
            CertError::BadSignature => write!(f, "a certificate signature is invalid"),
            CertError::NotAllowed(w) => write!(f, "the certificate chain is not allowed: {w}"),
            CertError::UnsupportedKey => write!(f, "the certificate uses an unsupported key type or size"),
            CertError::NoCertificate => write!(f, "the server sent no certificate"),
        }
    }
}

impl std::error::Error for CertError {}

fn bad(w: &'static str) -> CertError {
    CertError::BadEncoding(w)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigAlg {
    RsaPkcs1(HashAlg),
    /// RSASSA-PSS with MGF1 over the same hash and salt length = hash length.
    RsaPss(HashAlg),
    Ecdsa(HashAlg),
}

#[derive(Debug, Clone)]
pub enum PublicKey {
    Rsa(rsa::PublicKey),
    /// Uncompressed point.
    Ec(Curve, Vec<u8>),
}

impl PublicKey {
    /// Parses a SubjectPublicKeyInfo.
    pub fn from_spki(spki: &[u8]) -> Result<PublicKey, CertError> {
        let mut outer = Reader::new(spki);
        let mut seq = outer.sequence().map_err(|_| bad("key"))?;
        outer.finish().map_err(|_| bad("key"))?;
        let mut alg = seq.sequence().map_err(|_| bad("key algorithm"))?;
        let key_oid = alg.expect(der::OID).map_err(|_| bad("key algorithm"))?;
        let bits = seq.bit_string_bytes().map_err(|_| bad("key bits"))?;
        seq.finish().map_err(|_| bad("key"))?;
        if key_oid == oid::RSA_ENCRYPTION {
            // Parameters must be NULL.
            if !alg.expect(der::NULL).map_err(|_| bad("RSA key parameters"))?.is_empty() {
                return Err(bad("RSA key parameters"));
            }
            alg.finish().map_err(|_| bad("key algorithm"))?;
            rsa::PublicKey::from_der(bits).map(PublicKey::Rsa).ok_or(CertError::UnsupportedKey)
        } else if key_oid == oid::EC_PUBLIC_KEY {
            let curve_oid = alg.expect(der::OID).map_err(|_| bad("EC parameters"))?;
            alg.finish().map_err(|_| bad("key algorithm"))?;
            let curve = if curve_oid == oid::P256 {
                Curve::P256
            } else if curve_oid == oid::P384 {
                Curve::P384
            } else {
                return Err(CertError::UnsupportedKey);
            };
            let want = match curve {
                Curve::P256 => 65,
                Curve::P384 => 97,
            };
            if bits.len() != want || bits[0] != 4 {
                return Err(CertError::UnsupportedKey);
            }
            Ok(PublicKey::Ec(curve, bits.to_vec()))
        } else {
            Err(CertError::UnsupportedKey)
        }
    }

    /// Checks `sig` over `msg` with algorithm `alg`.
    pub fn verify(&self, alg: SigAlg, msg: &[u8], sig: &[u8]) -> bool {
        match (self, alg) {
            (PublicKey::Rsa(k), SigAlg::RsaPkcs1(h)) => k.verify_pkcs1(h, &h.hash(msg), sig),
            (PublicKey::Rsa(k), SigAlg::RsaPss(h)) => k.verify_pss(h, &h.hash(msg), sig, h.len()),
            (PublicKey::Ec(curve, point), SigAlg::Ecdsa(h)) => {
                let Some((r, s)) = ecdsa::parse_der_sig(sig) else {
                    return false;
                };
                ecdsa::verify(*curve, point, &h.hash(msg), r, s)
            }
            _ => false,
        }
    }
}

fn hash_from_oid(o: &[u8]) -> Option<HashAlg> {
    if o == oid::SHA256 {
        Some(HashAlg::Sha256)
    } else if o == oid::SHA384 {
        Some(HashAlg::Sha384)
    } else if o == oid::SHA512 {
        Some(HashAlg::Sha512)
    } else {
        None
    }
}

/// A hash AlgorithmIdentifier: SEQUENCE { OID, NULL or nothing }.
fn hash_alg_id(body: &[u8]) -> Option<HashAlg> {
    let mut r = Reader::new(body);
    let h = hash_from_oid(r.expect(der::OID).ok()?)?;
    if !r.is_empty()
        && !r.expect(der::NULL).ok()?.is_empty() {
            return None;
        }
    r.finish().ok()?;
    Some(h)
}

/// Parses a signature AlgorithmIdentifier (the contents of the SEQUENCE).
pub fn parse_sig_alg(body: &[u8]) -> Result<SigAlg, CertError> {
    let mut r = Reader::new(body);
    let o = r.expect(der::OID).map_err(|_| bad("signature algorithm"))?;
    let rsa_null = |r: &mut Reader| -> Result<(), CertError> {
        // NULL parameters (absence tolerated, as RFC 4055 allows).
        if !r.is_empty() && !r.expect(der::NULL).map_err(|_| bad("signature parameters"))?.is_empty() {
            return Err(bad("signature parameters"));
        }
        r.finish().map_err(|_| bad("signature parameters"))
    };
    let alg = if o == oid::SHA256_WITH_RSA {
        rsa_null(&mut r)?;
        SigAlg::RsaPkcs1(HashAlg::Sha256)
    } else if o == oid::SHA384_WITH_RSA {
        rsa_null(&mut r)?;
        SigAlg::RsaPkcs1(HashAlg::Sha384)
    } else if o == oid::SHA512_WITH_RSA {
        rsa_null(&mut r)?;
        SigAlg::RsaPkcs1(HashAlg::Sha512)
    } else if o == oid::ECDSA_SHA256 || o == oid::ECDSA_SHA384 || o == oid::ECDSA_SHA512 {
        r.finish().map_err(|_| bad("ECDSA parameters"))?;
        SigAlg::Ecdsa(if o == oid::ECDSA_SHA256 {
            HashAlg::Sha256
        } else if o == oid::ECDSA_SHA384 {
            HashAlg::Sha384
        } else {
            HashAlg::Sha512
        })
    } else if o == oid::RSASSA_PSS {
        let mut p = r.sequence().map_err(|_| bad("PSS parameters"))?;
        r.finish().map_err(|_| bad("PSS parameters"))?;
        // All fields explicit: we don't accept the SHA-1 defaults.
        let h = p.expect(der::ctx_cons(0)).map_err(|_| bad("PSS hash"))?;
        let h = hash_alg_id(Reader::new(h).expect(der::SEQUENCE).map_err(|_| bad("PSS hash"))?)
            .ok_or(bad("PSS hash"))?;
        let m = p.expect(der::ctx_cons(1)).map_err(|_| bad("PSS MGF"))?;
        let mut m = Reader::new(Reader::new(m).expect(der::SEQUENCE).map_err(|_| bad("PSS MGF"))?);
        if m.expect(der::OID).map_err(|_| bad("PSS MGF"))? != oid::MGF1 {
            return Err(bad("PSS MGF"));
        }
        let mh = hash_alg_id(m.expect(der::SEQUENCE).map_err(|_| bad("PSS MGF hash"))?).ok_or(bad("PSS MGF hash"))?;
        m.finish().map_err(|_| bad("PSS MGF"))?;
        let salt = p.expect(der::ctx_cons(2)).map_err(|_| bad("PSS salt"))?;
        let salt = Reader::new(salt).small_uint().map_err(|_| bad("PSS salt"))?;
        if let Some(t) = p.optional(der::ctx_cons(3)).map_err(|_| bad("PSS trailer"))? {
            if Reader::new(t).small_uint().map_err(|_| bad("PSS trailer"))? != 1 {
                return Err(bad("PSS trailer"));
            }
        }
        p.finish().map_err(|_| bad("PSS parameters"))?;
        if mh != h || salt as usize != h.len() {
            return Err(bad("PSS parameters"));
        }
        SigAlg::RsaPss(h)
    } else {
        return Err(bad("unsupported signature algorithm"));
    };
    Ok(alg)
}

/// Seconds since 1970 for a UTC calendar date (proleptic Gregorian).
pub fn unix_time(year: i64, month: u32, day: u32, h: u32, m: u32, s: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    days * 86400 + h as i64 * 3600 + m as i64 * 60 + s as i64
}

fn parse_time(tag: u8, b: &[u8]) -> Result<i64, CertError> {
    let digits = |s: &[u8]| -> Result<u32, CertError> {
        let mut v = 0u32;
        for &c in s {
            if !c.is_ascii_digit() {
                return Err(bad("time"));
            }
            v = v * 10 + (c - b'0') as u32;
        }
        Ok(v)
    };
    let (year, rest) = match tag {
        der::UTC_TIME if b.len() == 13 => {
            let yy = digits(&b[..2])? as i64;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, &b[2..])
        }
        der::GENERALIZED_TIME if b.len() == 15 => (digits(&b[..4])? as i64, &b[4..]),
        _ => return Err(bad("time")),
    };
    if rest[10] != b'Z' {
        return Err(bad("time"));
    }
    let mo = digits(&rest[0..2])?;
    let d = digits(&rest[2..4])?;
    let h = digits(&rest[4..6])?;
    let mi = digits(&rest[6..8])?;
    let s = digits(&rest[8..10])?;
    let mdays = [31, if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=12).contains(&mo) || d == 0 || d > mdays[mo as usize - 1] || h > 23 || mi > 59 || s > 59 {
        return Err(bad("time"));
    }
    Ok(unix_time(year, mo, d, h, mi, s))
}

#[derive(Debug, Clone, Default)]
pub struct NameConstraints {
    pub permitted_dns: Vec<String>,
    pub excluded_dns: Vec<String>,
    /// (address, mask), 4 or 16 bytes each.
    pub permitted_ip: Vec<(Vec<u8>, Vec<u8>)>,
    pub excluded_ip: Vec<(Vec<u8>, Vec<u8>)>,
}

/// A parsed certificate.
#[derive(Debug, Clone)]
pub struct Cert {
    pub der: Vec<u8>,
    pub tbs: Vec<u8>,
    pub sig_alg: SigAlg,
    pub signature: Vec<u8>,
    pub issuer: Vec<u8>,
    pub subject: Vec<u8>,
    pub not_before: i64,
    pub not_after: i64,
    pub spki: Vec<u8>,
    pub key: PublicKey,
    /// basicConstraints: (cA, pathLenConstraint).
    pub ca: bool,
    pub path_len: Option<u64>,
    pub has_basic_constraints: bool,
    /// keyUsage bits, bit 0 = digitalSignature (as 1 << 0), bit 5 = keyCertSign.
    pub key_usage: Option<u16>,
    /// extKeyUsage OIDs, when present.
    pub eku: Option<Vec<Vec<u8>>>,
    pub dns_names: Vec<String>,
    pub ip_addrs: Vec<Vec<u8>>,
    pub name_constraints: Option<NameConstraints>,
}

pub const KU_DIGITAL_SIGNATURE: u16 = 1 << 0;
pub const KU_KEY_CERT_SIGN: u16 = 1 << 5;

impl Cert {
    pub fn parse(data: &[u8]) -> Result<Cert, CertError> {
        let mut outer = Reader::new(data);
        let mut cert = outer.sequence().map_err(|_| bad("certificate"))?;
        outer.finish().map_err(|_| bad("trailing data"))?;
        let tbs_raw = cert.expect_raw(der::SEQUENCE).map_err(|_| bad("tbsCertificate"))?;
        let outer_alg = cert.expect(der::SEQUENCE).map_err(|_| bad("signature algorithm"))?;
        let signature = cert.bit_string_bytes().map_err(|_| bad("signature"))?;
        cert.finish().map_err(|_| bad("certificate"))?;
        let sig_alg = parse_sig_alg(outer_alg)?;

        let mut tbs = Reader::new(tbs_raw).sequence().map_err(|_| bad("tbsCertificate"))?;
        // version [0] EXPLICIT; we require v3.
        let v = tbs.expect(der::ctx_cons(0)).map_err(|_| bad("version"))?;
        if Reader::new(v).small_uint().map_err(|_| bad("version"))? != 2 {
            return Err(bad("not a version 3 certificate"));
        }
        // serial: INTEGER, at most 20 bytes (be a bit lenient on sign).
        let serial = tbs.expect(der::INTEGER).map_err(|_| bad("serial"))?;
        if serial.is_empty() || serial.len() > 21 {
            return Err(bad("serial"));
        }
        let inner_alg = tbs.expect(der::SEQUENCE).map_err(|_| bad("signature algorithm"))?;
        if inner_alg != outer_alg {
            return Err(bad("signature algorithms differ"));
        }
        let issuer = tbs.expect_raw(der::SEQUENCE).map_err(|_| bad("issuer"))?;
        let mut validity = tbs.sequence().map_err(|_| bad("validity"))?;
        let (t1, b1, _) = validity.any().map_err(|_| bad("validity"))?;
        let (t2, b2, _) = validity.any().map_err(|_| bad("validity"))?;
        validity.finish().map_err(|_| bad("validity"))?;
        let not_before = parse_time(t1, b1)?;
        let not_after = parse_time(t2, b2)?;
        let subject = tbs.expect_raw(der::SEQUENCE).map_err(|_| bad("subject"))?;
        let spki = tbs.expect_raw(der::SEQUENCE).map_err(|_| bad("subject key"))?;
        let key = PublicKey::from_spki(spki)?;
        // issuerUniqueID [1], subjectUniqueID [2]: obsolete; reject.
        if matches!(tbs.peek_tag(), Some(0x81) | Some(0x82) | Some(0xa1) | Some(0xa2)) {
            return Err(bad("unique identifiers"));
        }
        let mut c = Cert {
            der: data.to_vec(),
            tbs: tbs_raw.to_vec(),
            sig_alg,
            signature: signature.to_vec(),
            issuer: issuer.to_vec(),
            subject: subject.to_vec(),
            not_before,
            not_after,
            spki: spki.to_vec(),
            key,
            ca: false,
            path_len: None,
            has_basic_constraints: false,
            key_usage: None,
            eku: None,
            dns_names: Vec::new(),
            ip_addrs: Vec::new(),
            name_constraints: None,
        };
        if let Some(exts) = tbs.optional(der::ctx_cons(3)).map_err(|_| bad("extensions"))? {
            let mut wrap = Reader::new(exts);
            let mut list = wrap.sequence().map_err(|_| bad("extensions"))?;
            wrap.finish().map_err(|_| bad("extensions"))?;
            let mut seen: Vec<&[u8]> = Vec::new();
            while !list.is_empty() {
                let mut e = list.sequence().map_err(|_| bad("extension"))?;
                let id = e.expect(der::OID).map_err(|_| bad("extension"))?;
                let critical = if e.peek_tag() == Some(der::BOOLEAN) {
                    e.boolean().map_err(|_| bad("extension"))?
                } else {
                    false
                };
                let value = e.expect(der::OCTET_STRING).map_err(|_| bad("extension"))?;
                e.finish().map_err(|_| bad("extension"))?;
                if seen.contains(&id) {
                    return Err(bad("duplicate extension"));
                }
                seen.push(id);
                c.extension(id, critical, value)?;
            }
        }
        tbs.finish().map_err(|_| bad("tbsCertificate"))?;
        Ok(c)
    }

    fn extension(&mut self, id: &[u8], critical: bool, v: &[u8]) -> Result<(), CertError> {
        let mut outer = Reader::new(v);
        if id == oid::BASIC_CONSTRAINTS {
            let mut s = outer.sequence().map_err(|_| bad("basicConstraints"))?;
            self.has_basic_constraints = true;
            if s.peek_tag() == Some(der::BOOLEAN) {
                self.ca = s.boolean().map_err(|_| bad("basicConstraints"))?;
            }
            if s.peek_tag() == Some(der::INTEGER) {
                self.path_len = Some(s.small_uint().map_err(|_| bad("basicConstraints"))?);
            }
            s.finish().map_err(|_| bad("basicConstraints"))?;
        } else if id == oid::KEY_USAGE {
            let b = outer.expect(der::BIT_STRING).map_err(|_| bad("keyUsage"))?;
            if b.is_empty() || b[0] > 7 || b.len() > 3 || (b.len() == 1 && b[0] != 0) {
                return Err(bad("keyUsage"));
            }
            let mut bits = 0u16;
            for i in 0..16 {
                let byte = 1 + i / 8;
                if byte < b.len() && (b[byte] >> (7 - i % 8)) & 1 == 1 {
                    bits |= 1 << i;
                }
            }
            self.key_usage = Some(bits);
        } else if id == oid::EXT_KEY_USAGE_EXT {
            let mut s = outer.sequence().map_err(|_| bad("extKeyUsage"))?;
            let mut v = Vec::new();
            while !s.is_empty() {
                v.push(s.expect(der::OID).map_err(|_| bad("extKeyUsage"))?.to_vec());
            }
            if v.is_empty() {
                return Err(bad("extKeyUsage"));
            }
            self.eku = Some(v);
        } else if id == oid::SUBJECT_ALT_NAME {
            let mut s = outer.sequence().map_err(|_| bad("subjectAltName"))?;
            while !s.is_empty() {
                let (tag, body, _) = s.any().map_err(|_| bad("subjectAltName"))?;
                match tag {
                    0x82 => {
                        let name = std::str::from_utf8(body).map_err(|_| bad("dNSName"))?;
                        if !name.is_ascii() {
                            return Err(bad("dNSName"));
                        }
                        self.dns_names.push(name.to_ascii_lowercase());
                    }
                    0x87 => {
                        if body.len() != 4 && body.len() != 16 {
                            return Err(bad("iPAddress"));
                        }
                        self.ip_addrs.push(body.to_vec());
                    }
                    _ => {} // other name forms: not used for TLS servers
                }
            }
        } else if id == oid::NAME_CONSTRAINTS {
            self.name_constraints = Some(parse_name_constraints(&mut outer)?);
        } else if critical
            && !(id == oid::CERT_POLICIES
                || id == oid::POLICY_CONSTRAINTS
                || id == oid::INHIBIT_ANY_POLICY
                || id == oid::SUBJECT_KEY_ID
                || id == oid::AUTHORITY_KEY_ID)
        {
            return Err(bad("unknown critical extension"));
        } else {
            return Ok(()); // a non-critical extension we don't use
        }
        outer.finish().map_err(|_| bad("extension"))
    }

    /// Checks the validity period against `now` (Unix seconds).
    pub fn check_time(&self, now: i64) -> Result<(), CertError> {
        if now < self.not_before {
            Err(CertError::NotYetValid)
        } else if now > self.not_after {
            Err(CertError::Expired)
        } else {
            Ok(())
        }
    }

    pub fn allows_server_auth(&self) -> bool {
        match &self.eku {
            None => true,
            Some(v) => v.iter().any(|o| o == oid::KP_SERVER_AUTH),
        }
    }

    /// Does this (leaf) certificate name `host`? RFC 6125: subjectAltName
    /// only (the subject's common name is not used), "*." wildcards only in
    /// the leftmost label, matching exactly one label.
    pub fn matches_host(&self, host: &str) -> bool {
        let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
        if let Some(ip) = parse_ip(&host) {
            return self.ip_addrs.contains(&ip);
        }
        self.dns_names.iter().any(|n| dns_name_matches(n, &host))
    }
}

/// A host's IP address bytes when it is a literal (v4 dotted, or v6 in brackets).
pub fn parse_ip(host: &str) -> Option<Vec<u8>> {
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6.parse::<std::net::Ipv6Addr>().ok().map(|a| a.octets().to_vec());
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut out = Vec::with_capacity(4);
    for p in parts {
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) || (p.len() > 1 && p.starts_with('0')) {
            return None;
        }
        let v: u32 = p.parse().ok()?;
        if v > 255 {
            return None;
        }
        out.push(v as u8);
    }
    Some(out)
}

fn valid_dns(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|l| {
            !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// A certificate DNS name against a (lowercase) host name.
pub fn dns_name_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.strip_suffix('.').unwrap_or(pattern);
    if let Some(rest) = pattern.strip_prefix("*.") {
        // At least two labels after the wildcard: "*.com" never matches.
        if !valid_dns(rest) || !rest.contains('.') {
            return false;
        }
        let Some((first, tail)) = host.split_once('.') else {
            return false;
        };
        !first.is_empty() && valid_dns(host) && tail == rest
    } else {
        valid_dns(pattern) && pattern == host
    }
}

fn parse_name_constraints(r: &mut Reader) -> Result<NameConstraints, CertError> {
    let mut nc = NameConstraints::default();
    let mut s = r.sequence().map_err(|_| bad("nameConstraints"))?;
    for (tag, excluded) in [(der::ctx_cons(0), false), (der::ctx_cons(1), true)] {
        if let Some(body) = s.optional(tag).map_err(|_| bad("nameConstraints"))? {
            let mut list = Reader::new(body);
            while !list.is_empty() {
                let mut sub = list.sequence().map_err(|_| bad("nameConstraints"))?;
                let (t, b, _) = sub.any().map_err(|_| bad("nameConstraints"))?;
                // minimum/maximum must be absent (RFC 5280 4.2.1.10).
                sub.finish().map_err(|_| bad("nameConstraints"))?;
                match t {
                    0x82 => {
                        let n = std::str::from_utf8(b).map_err(|_| bad("nameConstraints"))?.to_ascii_lowercase();
                        if excluded { nc.excluded_dns.push(n) } else { nc.permitted_dns.push(n) }
                    }
                    0x87 => {
                        if b.len() != 8 && b.len() != 32 {
                            return Err(bad("nameConstraints"));
                        }
                        let (a, m) = b.split_at(b.len() / 2);
                        let e = (a.to_vec(), m.to_vec());
                        if excluded { nc.excluded_ip.push(e) } else { nc.permitted_ip.push(e) }
                    }
                    _ => {} // directory names, e-mail, URIs: not used for TLS server names
                }
            }
        }
    }
    s.finish().map_err(|_| bad("nameConstraints"))?;
    Ok(nc)
}

fn dns_in_subtree(name: &str, base: &str) -> bool {
    let name = name.strip_prefix("*.").map(|r| r.to_string()).unwrap_or_else(|| name.to_string());
    if base.is_empty() {
        return true;
    }
    if let Some(b) = base.strip_prefix('.') {
        return name.ends_with(&format!(".{b}"));
    }
    name == base || name.ends_with(&format!(".{base}"))
}

fn ip_in(ip: &[u8], net: &(Vec<u8>, Vec<u8>)) -> bool {
    ip.len() == net.0.len() && ip.iter().zip(&net.0).zip(&net.1).all(|((a, n), m)| a & m == n & m)
}

impl NameConstraints {
    fn allows(&self, leaf: &Cert) -> bool {
        for n in &leaf.dns_names {
            if self.excluded_dns.iter().any(|b| dns_in_subtree(n, b)) {
                return false;
            }
            if !self.permitted_dns.is_empty() && !self.permitted_dns.iter().any(|b| dns_in_subtree(n, b)) {
                return false;
            }
        }
        for ip in &leaf.ip_addrs {
            if self.excluded_ip.iter().any(|n| ip_in(ip, n)) {
                return false;
            }
            if !self.permitted_ip.is_empty() && !self.permitted_ip.iter().any(|n| ip_in(ip, n)) {
                return false;
            }
        }
        true
    }
}

const MAX_DEPTH: usize = 8;
const MAX_SIG_CHECKS: usize = 64;

struct Search<'a> {
    leaf: &'a Cert,
    pool: &'a [Cert],
    roots: &'a RootStore,
    now: i64,
    sig_checks: usize,
    last_err: CertError,
}

impl Search<'_> {
    fn verify_sig(&mut self, key: &PublicKey, c: &Cert) -> bool {
        if self.sig_checks >= MAX_SIG_CHECKS {
            return false;
        }
        self.sig_checks += 1;
        key.verify(c.sig_alg, &c.tbs, &c.signature)
    }

    /// Finds a path from `cur` (already accepted) up to a trust anchor.
    /// `used` holds pool indexes on the current path; `depth` counts the
    /// intermediates below `cur`, which constrain its pathLen.
    fn extend(&mut self, cur: &Cert, used: &mut Vec<usize>, below: usize) -> bool {
        if used.len() > MAX_DEPTH {
            self.last_err = CertError::NotAllowed("the chain is too long");
            return false;
        }
        // A trust anchor that issued `cur`?
        for anchor in self.roots.by_subject(&cur.issuer) {
            if self.anchor_ok(anchor, cur) {
                return true;
            }
        }
        // An intermediate from what the server sent.
        for i in 0..self.pool.len() {
            if used.contains(&i) {
                continue;
            }
            let cand = &self.pool[i];
            if cand.subject != cur.issuer {
                continue;
            }
            if let Err(e) = self.issuer_ok(cand, below) {
                self.last_err = e;
                continue;
            }
            if !self.verify_sig(&cand.key, cur) {
                self.last_err = CertError::BadSignature;
                continue;
            }
            used.push(i);
            if self.extend(cand, used, below + 1) {
                return true;
            }
            used.pop();
        }
        false
    }

    fn anchor_ok(&mut self, anchor: &TrustAnchor, cur: &Cert) -> bool {
        if let Some(na) = anchor.not_after {
            if self.now > na {
                self.last_err = CertError::Expired;
                return false;
            }
        }
        let Ok(key) = PublicKey::from_spki(&anchor.spki) else {
            return false;
        };
        if !self.verify_sig(&key, cur) {
            self.last_err = CertError::BadSignature;
            return false;
        }
        true
    }

    fn issuer_ok(&self, c: &Cert, below: usize) -> Result<(), CertError> {
        c.check_time(self.now)?;
        if !c.has_basic_constraints || !c.ca {
            return Err(CertError::NotAllowed("an issuing certificate is not a CA"));
        }
        if let Some(pl) = c.path_len {
            if (below as u64) > pl {
                return Err(CertError::NotAllowed("path length constraint exceeded"));
            }
        }
        if let Some(ku) = c.key_usage {
            if ku & KU_KEY_CERT_SIGN == 0 {
                return Err(CertError::NotAllowed("an issuing certificate may not sign certificates"));
            }
        }
        if !c.allows_server_auth() {
            return Err(CertError::NotAllowed("an issuing certificate is not for web servers"));
        }
        if let Some(nc) = &c.name_constraints {
            if !nc.allows(self.leaf) {
                return Err(CertError::NotAllowed("name constraints exclude this host"));
            }
        }
        Ok(())
    }
}

/// Validates a server's chain (leaf first, as sent) for `host` at time
/// `now`, against `roots`. Returns the leaf on success.
pub fn verify_chain(chain: &[Vec<u8>], host: &str, now: i64, roots: &RootStore) -> Result<Cert, CertError> {
    let leaf_der = chain.first().ok_or(CertError::NoCertificate)?;
    if chain.len() > 10 {
        return Err(CertError::NotAllowed("too many certificates"));
    }
    let leaf = Cert::parse(leaf_der)?;
    leaf.check_time(now)?;
    if !leaf.matches_host(host) {
        return Err(CertError::WrongHost(host.to_string()));
    }
    if let Some(ku) = leaf.key_usage {
        if ku & KU_DIGITAL_SIGNATURE == 0 {
            return Err(CertError::NotAllowed("the server's key may not sign"));
        }
    }
    if !leaf.allows_server_auth() {
        return Err(CertError::NotAllowed("the certificate is not for web servers"));
    }
    // Intermediates that don't parse are skipped, not fatal: servers send
    // extra certificates, and only the path we build matters.
    let pool: Vec<Cert> = chain[1..].iter().filter_map(|d| Cert::parse(d).ok()).collect();
    let mut s = Search { leaf: &leaf, pool: &pool, roots, now, sig_checks: 0, last_err: CertError::UnknownIssuer };
    let mut used = Vec::new();
    if s.extend(&leaf, &mut used, 0) {
        Ok(leaf)
    } else {
        Err(s.last_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(unix_time(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(unix_time(2000, 3, 1, 0, 0, 0), 951868800);
        assert_eq!(parse_time(der::UTC_TIME, b"491231235959Z").unwrap(), unix_time(2049, 12, 31, 23, 59, 59));
        assert_eq!(parse_time(der::UTC_TIME, b"500101000000Z").unwrap(), unix_time(1950, 1, 1, 0, 0, 0));
        assert_eq!(parse_time(der::GENERALIZED_TIME, b"20240229120000Z").unwrap(), 1709208000);
        assert!(parse_time(der::GENERALIZED_TIME, b"20230229120000Z").is_err());
        assert!(parse_time(der::UTC_TIME, b"491231235959+0100").is_err());
        assert!(parse_time(der::UTC_TIME, b"4912312359Z").is_err());
    }

    #[test]
    fn host_matching() {
        assert!(dns_name_matches("example.com", "example.com"));
        assert!(!dns_name_matches("example.com", "www.example.com"));
        assert!(dns_name_matches("*.example.com", "www.example.com"));
        assert!(!dns_name_matches("*.example.com", "a.b.example.com"));
        assert!(!dns_name_matches("*.example.com", "example.com"));
        assert!(!dns_name_matches("*.com", "example.com"));
        assert!(!dns_name_matches("w*.example.com", "www.example.com"));
        assert!(!dns_name_matches("*.*.example.com", "a.b.example.com"));
        assert!(!dns_name_matches("www.*.com", "www.example.com"));
        assert_eq!(parse_ip("10.0.0.1"), Some(vec![10, 0, 0, 1]));
        assert_eq!(parse_ip("10.0.0.01"), None);
        assert_eq!(parse_ip("example.com"), None);
    }

    #[test]
    fn name_constraint_subtrees() {
        assert!(dns_in_subtree("a.example.com", "example.com"));
        assert!(dns_in_subtree("example.com", "example.com"));
        assert!(!dns_in_subtree("badexample.com", "example.com"));
        assert!(!dns_in_subtree("example.com", ".example.com"));
        assert!(dns_in_subtree("*.example.com", "example.com"));
    }
}
