//! A TLS client: TLS 1.3 (RFC 8446), and TLS 1.2 (RFC 5246) for servers
//! that don't speak 1.3 yet. No 0-RTT, no resumption, no renegotiation, no
//! client certificates (an empty one is sent if asked).
//!
//! Key exchange: X25519, P-256 or P-384. Shares for X25519 and P-256 are
//! sent up front; a server that wants P-384 asks with HelloRetryRequest. TLS 1.3 suites: TLS_CHACHA20_POLY1305_SHA256,
//! TLS_AES_128_GCM_SHA256, TLS_AES_256_GCM_SHA384. Server signatures:
//! ECDSA P-256/SHA-256, ECDSA P-384/SHA-384, RSA-PSS (rsae) with SHA-256,
//! -384, -512. The server's certificate chain is validated by `x509`.
//!
//! TLS 1.2 is held to what is still considered safe: ECDHE key exchange
//! only (forward secrecy; no static RSA, no finite-field DH), AEAD ciphers
//! only (AES-GCM, ChaCha20-Poly1305; no CBC, so no padding oracles), the
//! extended master secret (RFC 7627) when the server supports it, and the
//! RFC 8446 downgrade check: a server that speaks 1.3 but answers 1.2 is
//! refused. RSA PKCS#1 v1.5 signatures, which 1.3 forbids in the handshake,
//! are accepted on a 1.2 ServerKeyExchange, as 1.2 servers use them.

use crate::crypto::chacha::ChaCha20Poly1305;
use crate::crypto::ecdsa::Curve;
use crate::crypto::gcm::AesGcm;
use crate::crypto::rsa::HashAlg;
use crate::crypto::{self, ct_eq, ecdh, hkdf_expand, hkdf_extract, wipe, x25519, Hash, Hmac, Sha256, Sha384};
use crate::roots::RootStore;
use crate::x509::{self, CertError, PublicKey, SigAlg};
use std::fmt;
use std::io::{self, Read, Write};

pub const TLS_AES_128_GCM_SHA256: u16 = 0x1301;
pub const TLS_AES_256_GCM_SHA384: u16 = 0x1302;
pub const TLS_CHACHA20_POLY1305_SHA256: u16 = 0x1303;
// TLS 1.2: ECDHE + AEAD only.
pub const TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256: u16 = 0xc02b;
pub const TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384: u16 = 0xc02c;
pub const TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256: u16 = 0xc02f;
pub const TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384: u16 = 0xc030;
pub const TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256: u16 = 0xcca8;
pub const TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256: u16 = 0xcca9;

/// Our preference order: TLS 1.3 suites, then TLS 1.2 ones (ChaCha20 first:
/// it is the fastest in portable constant-time software).
pub const DEFAULT_SUITES: &[u16] = &[
    TLS_CHACHA20_POLY1305_SHA256,
    TLS_AES_128_GCM_SHA256,
    TLS_AES_256_GCM_SHA384,
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
];

fn is_tls13_suite(s: u16) -> bool {
    matches!(s, TLS_AES_128_GCM_SHA256 | TLS_AES_256_GCM_SHA384 | TLS_CHACHA20_POLY1305_SHA256)
}

fn is_tls12_suite(s: u16) -> bool {
    matches!(
        s,
        TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
            | TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
            | TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
            | TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
            | TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256
            | TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256
    )
}

/// The TLS 1.2 suites that need an ECDSA certificate (the rest need RSA).
fn tls12_suite_is_ecdsa(s: u16) -> bool {
    matches!(
        s,
        TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
            | TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
            | TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256
    )
}

fn is_chacha(s: u16) -> bool {
    matches!(
        s,
        TLS_CHACHA20_POLY1305_SHA256
            | TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256
            | TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256
    )
}

fn key_len(s: u16) -> usize {
    match s {
        TLS_AES_128_GCM_SHA256 | TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256 | TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256 => 16,
        _ => 32,
    }
}

fn suite_kdf(s: u16) -> Kdf {
    match s {
        TLS_AES_256_GCM_SHA384 | TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384 | TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384 => {
            Kdf::Sha384
        }
        _ => Kdf::Sha256,
    }
}

const CT_CCS: u8 = 20;
const CT_ALERT: u8 = 21;
const CT_HANDSHAKE: u8 = 22;
const CT_APPDATA: u8 = 23;

const HS_HELLO_REQUEST: u8 = 0;
const HS_CLIENT_HELLO: u8 = 1;
const HS_SERVER_HELLO: u8 = 2;
const HS_NEW_SESSION_TICKET: u8 = 4;
const HS_ENCRYPTED_EXTENSIONS: u8 = 8;
const HS_CERTIFICATE: u8 = 11;
const HS_SERVER_KEY_EXCHANGE: u8 = 12;
const HS_CERTIFICATE_REQUEST: u8 = 13;
const HS_SERVER_HELLO_DONE: u8 = 14;
const HS_CERTIFICATE_VERIFY: u8 = 15;
const HS_CLIENT_KEY_EXCHANGE: u8 = 16;
const HS_FINISHED: u8 = 20;
const HS_KEY_UPDATE: u8 = 24;

const EXT_SERVER_NAME: u16 = 0;
const EXT_SUPPORTED_GROUPS: u16 = 10;
const EXT_EC_POINT_FORMATS: u16 = 11;
const EXT_SIGNATURE_ALGORITHMS: u16 = 13;
const EXT_ALPN: u16 = 16;
const EXT_EXTENDED_MASTER_SECRET: u16 = 23;
const EXT_SUPPORTED_VERSIONS: u16 = 43;
const EXT_COOKIE: u16 = 44;
const EXT_KEY_SHARE: u16 = 51;
const EXT_RENEGOTIATION_INFO: u16 = 0xff01;

const GROUP_SECP256R1: u16 = 0x0017;
const GROUP_SECP384R1: u16 = 0x0018;
const GROUP_X25519: u16 = 0x001d;

/// The key exchange groups we offer, in our order of preference.
const GROUPS: &[u16] = &[GROUP_X25519, GROUP_SECP256R1, GROUP_SECP384R1];
/// The groups we send key shares for in the first ClientHello.
const FIRST_SHARES: &[u16] = &[GROUP_X25519, GROUP_SECP256R1];

/// The length of a key share (public key) in `group`.
fn share_len(group: u16) -> Option<usize> {
    match group {
        GROUP_X25519 => Some(32),
        GROUP_SECP256R1 => Some(ecdh::Curve::P256.point_len()),
        GROUP_SECP384R1 => Some(ecdh::Curve::P384.point_len()),
        _ => None,
    }
}

/// An ephemeral key pair for one group. The private key is wiped on drop.
struct Share {
    group: u16,
    private: Vec<u8>,
    public: Vec<u8>,
}

impl Share {
    fn new(group: u16) -> Share {
        match group {
            GROUP_X25519 => {
                let mut k = [0u8; 32];
                crypto::random(&mut k);
                let public = x25519::public_key(&k).to_vec();
                let share = Share { group, private: k.to_vec(), public };
                wipe(&mut k);
                share
            }
            _ => {
                let curve = if group == GROUP_SECP256R1 { ecdh::Curve::P256 } else { ecdh::Curve::P384 };
                let private = ecdh::random_scalar(curve);
                let public = ecdh::public_key(curve, &private);
                Share { group, private, public }
            }
        }
    }

    /// The shared secret with the peer's key share, or None if that is invalid.
    fn agree(&self, peer: &[u8]) -> Option<Vec<u8>> {
        if Some(peer.len()) != share_len(self.group) {
            return None;
        }
        match self.group {
            GROUP_X25519 => {
                let mut k = [0u8; 32];
                k.copy_from_slice(&self.private);
                let mut p = [0u8; 32];
                p.copy_from_slice(peer);
                let s = x25519::shared(&k, &p).map(|s| s.to_vec());
                wipe(&mut k);
                s
            }
            GROUP_SECP256R1 => ecdh::shared(ecdh::Curve::P256, &self.private, peer),
            _ => ecdh::shared(ecdh::Curve::P384, &self.private, peer),
        }
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        wipe(&mut self.private);
    }
}

/// The last 8 bytes of a TLS 1.2 ServerHello random from a server that
/// supports TLS 1.3 (RFC 8446 section 4.1.3): someone is downgrading us.
const DOWNGRADE_TLS12: [u8; 8] = *b"DOWNGRD\x01";
const DOWNGRADE_TLS11: [u8; 8] = *b"DOWNGRD\x00";

const SIG_ECDSA_P256_SHA256: u16 = 0x0403;
const SIG_ECDSA_P384_SHA384: u16 = 0x0503;
const SIG_RSA_PSS_RSAE_SHA256: u16 = 0x0804;
const SIG_RSA_PSS_RSAE_SHA384: u16 = 0x0805;
const SIG_RSA_PSS_RSAE_SHA512: u16 = 0x0806;
const SIG_RSA_PKCS1_SHA256: u16 = 0x0401;
const SIG_RSA_PKCS1_SHA384: u16 = 0x0501;
const SIG_RSA_PKCS1_SHA512: u16 = 0x0601;
const SIG_ECDSA_P521_SHA512: u16 = 0x0603;

/// What we accept in a TLS 1.3 CertificateVerify.
const HANDSHAKE_SIGS: &[u16] = &[
    SIG_ECDSA_P256_SHA256,
    SIG_ECDSA_P384_SHA384,
    SIG_RSA_PSS_RSAE_SHA256,
    SIG_RSA_PSS_RSAE_SHA384,
    SIG_RSA_PSS_RSAE_SHA512,
];
/// What we accept in certificates (PKCS#1 v1.5 is still how most are signed).
const CERT_SIGS: &[u16] = &[
    SIG_ECDSA_P256_SHA256,
    SIG_ECDSA_P384_SHA384,
    SIG_RSA_PSS_RSAE_SHA256,
    SIG_RSA_PSS_RSAE_SHA384,
    SIG_RSA_PSS_RSAE_SHA512,
    SIG_RSA_PKCS1_SHA256,
    SIG_RSA_PKCS1_SHA384,
    SIG_RSA_PKCS1_SHA512,
];
const EXT_SIGNATURE_ALGORITHMS_CERT: u16 = 50;
/// What we offer in signature_algorithms: the TLS 1.3 handshake schemes,
/// plus PKCS#1 v1.5 for a TLS 1.2 ServerKeyExchange (a 1.3 server must not
/// use those in CertificateVerify, and `check_certificate_verify` refuses them).
const OFFERED_SIGS: &[u16] = &[
    SIG_ECDSA_P256_SHA256,
    SIG_ECDSA_P384_SHA384,
    SIG_RSA_PSS_RSAE_SHA256,
    SIG_RSA_PSS_RSAE_SHA384,
    SIG_RSA_PSS_RSAE_SHA512,
    SIG_RSA_PKCS1_SHA256,
    SIG_RSA_PKCS1_SHA384,
    SIG_RSA_PKCS1_SHA512,
];

/// SHA-256("HelloRetryRequest"): a ServerHello with this random is a HelloRetryRequest.
const HRR_RANDOM: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91, 0xc2, 0xa2, 0x11,
    0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

const MAX_PLAINTEXT: usize = 1 << 14;
const MAX_CIPHERTEXT: usize = (1 << 14) + 256;
const MAX_HANDSHAKE_MSG: usize = 1 << 17;

// Alert descriptions.
const A_CLOSE_NOTIFY: u8 = 0;
const A_UNEXPECTED_MESSAGE: u8 = 10;
const A_BAD_RECORD_MAC: u8 = 20;
const A_RECORD_OVERFLOW: u8 = 22;
const A_HANDSHAKE_FAILURE: u8 = 40;
const A_BAD_CERTIFICATE: u8 = 42;
const A_UNSUPPORTED_CERTIFICATE: u8 = 43;
const A_CERTIFICATE_EXPIRED: u8 = 45;
const A_ILLEGAL_PARAMETER: u8 = 47;
const A_UNKNOWN_CA: u8 = 48;
const A_DECODE_ERROR: u8 = 50;
const A_DECRYPT_ERROR: u8 = 51;
const A_PROTOCOL_VERSION: u8 = 70;
const A_UNSUPPORTED_EXTENSION: u8 = 110;
const A_NO_APPLICATION_PROTOCOL: u8 = 120;

fn alert_name(d: u8) -> &'static str {
    match d {
        0 => "close_notify",
        10 => "unexpected_message",
        20 => "bad_record_mac",
        22 => "record_overflow",
        40 => "handshake_failure",
        42 => "bad_certificate",
        43 => "unsupported_certificate",
        44 => "certificate_revoked",
        45 => "certificate_expired",
        46 => "certificate_unknown",
        47 => "illegal_parameter",
        48 => "unknown_ca",
        49 => "access_denied",
        50 => "decode_error",
        51 => "decrypt_error",
        70 => "protocol_version",
        71 => "insufficient_security",
        80 => "internal_error",
        86 => "inappropriate_fallback",
        90 => "user_canceled",
        109 => "missing_extension",
        110 => "unsupported_extension",
        112 => "unrecognized_name",
        116 => "certificate_required",
        120 => "no_application_protocol",
        _ => "unknown alert",
    }
}

#[derive(Debug)]
pub enum TlsError {
    Io(io::Error),
    /// The server sent a fatal alert.
    Alert(u8),
    /// The server speaks neither TLS 1.3 nor 1.2.
    NoTls13,
    Cert(CertError),
    /// The server broke the protocol (what it did).
    Protocol(&'static str),
    /// A record failed authentication.
    BadRecord,
    /// The server proved nothing: its signature or Finished didn't check out.
    BadHandshakeSignature,
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::Io(e) => write!(f, "{e}"),
            TlsError::Alert(A_PROTOCOL_VERSION) => {
                write!(f, "the server supports neither TLS 1.3 nor TLS 1.2 (protocol_version alert)")
            }
            TlsError::Alert(A_HANDSHAKE_FAILURE) => write!(
                f,
                "the server ended the secure connection (handshake_failure): it offered no encryption this \
                 browser accepts (it may only support old, insecure ciphers)"
            ),
            TlsError::Alert(d) => write!(f, "the server ended the secure connection ({})", alert_name(*d)),
            TlsError::NoTls13 => {
                write!(f, "the server only supports TLS 1.1 or older, which are insecure (TLS 1.2 or 1.3 is required)")
            }
            TlsError::Cert(e) => write!(f, "{e}"),
            TlsError::Protocol(w) => write!(f, "secure connection failed: {w}"),
            TlsError::BadRecord => write!(f, "secure connection failed: data was altered in transit"),
            TlsError::BadHandshakeSignature => {
                write!(f, "secure connection failed: the server could not prove it owns its certificate")
            }
        }
    }
}

impl std::error::Error for TlsError {}

impl From<io::Error> for TlsError {
    fn from(e: io::Error) -> TlsError {
        TlsError::Io(e)
    }
}

impl From<TlsError> for io::Error {
    fn from(e: TlsError) -> io::Error {
        match e {
            TlsError::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        }
    }
}

fn proto(w: &'static str) -> TlsError {
    TlsError::Protocol(w)
}

// ---- Hash-agnostic key schedule -------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kdf {
    Sha256,
    Sha384,
}

impl Kdf {
    fn len(self) -> usize {
        match self {
            Kdf::Sha256 => 32,
            Kdf::Sha384 => 48,
        }
    }
    fn hash(self, d: &[u8]) -> Vec<u8> {
        match self {
            Kdf::Sha256 => Sha256::hash(d),
            Kdf::Sha384 => Sha384::hash(d),
        }
    }
    fn hmac(self, k: &[u8], d: &[u8]) -> Vec<u8> {
        match self {
            Kdf::Sha256 => Hmac::<Sha256>::mac(k, d),
            Kdf::Sha384 => Hmac::<Sha384>::mac(k, d),
        }
    }
    fn extract(self, salt: &[u8], ikm: &[u8]) -> Vec<u8> {
        match self {
            Kdf::Sha256 => hkdf_extract::<Sha256>(salt, ikm),
            Kdf::Sha384 => hkdf_extract::<Sha384>(salt, ikm),
        }
    }
    fn expand_label(self, secret: &[u8], label: &str, ctx: &[u8], len: usize) -> Vec<u8> {
        let mut info = Vec::with_capacity(10 + label.len() + ctx.len());
        info.extend_from_slice(&(len as u16).to_be_bytes());
        info.push((6 + label.len()) as u8);
        info.extend_from_slice(b"tls13 ");
        info.extend_from_slice(label.as_bytes());
        info.push(ctx.len() as u8);
        info.extend_from_slice(ctx);
        match self {
            Kdf::Sha256 => hkdf_expand::<Sha256>(secret, &info, len),
            Kdf::Sha384 => hkdf_expand::<Sha384>(secret, &info, len),
        }
    }
    fn derive(self, secret: &[u8], label: &str, transcript_hash: &[u8]) -> Vec<u8> {
        self.expand_label(secret, label, transcript_hash, self.len())
    }
    /// The TLS 1.2 PRF (RFC 5246 section 5): P_hash(secret, label + seed).
    fn prf12(self, secret: &[u8], label: &str, seed: &[u8], len: usize) -> Vec<u8> {
        let mut ls = label.as_bytes().to_vec();
        ls.extend_from_slice(seed);
        let mut out = Vec::with_capacity(len + self.len());
        let mut a = self.hmac(secret, &ls);
        while out.len() < len {
            let mut m = a.clone();
            m.extend_from_slice(&ls);
            out.extend_from_slice(&self.hmac(secret, &m));
            a = self.hmac(secret, &a);
        }
        out.truncate(len);
        out
    }
}

// ---- Record protection ----------------------------------------------------

enum Aead {
    Aes(AesGcm),
    ChaCha(ChaCha20Poly1305),
}

struct Keys {
    aead: Aead,
    /// TLS 1.3 and 1.2 ChaCha20: the 12-byte IV the sequence number is
    /// XORed into. TLS 1.2 AES-GCM: the 4-byte salt, then zeros, so the same
    /// XOR gives salt || sequence number, the explicit nonce we send.
    iv: [u8; 12],
    seq: u64,
    suite: u16,
    secret: Vec<u8>,
    kdf: Kdf,
    /// TLS 1.2 record format (an explicit nonce with AES-GCM).
    tls12: bool,
}

impl Keys {
    fn new(suite: u16, kdf: Kdf, secret: Vec<u8>) -> Keys {
        let mut key = kdf.expand_label(&secret, "key", &[], key_len(suite));
        let ivv = kdf.expand_label(&secret, "iv", &[], 12);
        let aead = if is_chacha(suite) { Aead::ChaCha(ChaCha20Poly1305::new(&key)) } else { Aead::Aes(AesGcm::new(&key)) };
        wipe(&mut key);
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&ivv);
        Keys { aead, iv, seq: 0, suite, secret, kdf, tls12: false }
    }

    /// TLS 1.2 keys from the key block (RFC 5288, RFC 7905).
    fn new12(suite: u16, key: &[u8], ivv: &[u8]) -> Keys {
        let aead = if is_chacha(suite) { Aead::ChaCha(ChaCha20Poly1305::new(key)) } else { Aead::Aes(AesGcm::new(key)) };
        let mut iv = [0u8; 12];
        iv[..ivv.len()].copy_from_slice(ivv);
        Keys { aead, iv, seq: 0, suite, secret: Vec::new(), kdf: suite_kdf(suite), tls12: true }
    }

    fn explicit_nonce(&self) -> bool {
        self.tls12 && matches!(self.aead, Aead::Aes(_))
    }

    /// The TLS 1.2 additional data: seq_num || type || version || length.
    fn aad12(&self, ct: u8, len: usize) -> [u8; 13] {
        let mut a = [0u8; 13];
        a[..8].copy_from_slice(&self.seq.to_be_bytes());
        a[8] = ct;
        a[9] = 3;
        a[10] = 3;
        a[11] = (len >> 8) as u8;
        a[12] = len as u8;
        a
    }

    /// Encrypts one TLS 1.2 record body; returns what goes after the header.
    fn seal12(&mut self, ct: u8, plain: &[u8]) -> Result<Vec<u8>, TlsError> {
        let aad = self.aad12(ct, plain.len());
        let explicit = self.explicit_nonce();
        let n = self.nonce()?;
        let mut data = plain.to_vec();
        match &self.aead {
            Aead::Aes(a) => a.seal(&n, &aad, &mut data),
            Aead::ChaCha(a) => a.seal(&n, &aad, &mut data),
        }
        if explicit {
            let mut out = n[4..].to_vec();
            out.extend_from_slice(&data);
            Ok(out)
        } else {
            Ok(data)
        }
    }

    /// Decrypts one TLS 1.2 record body in place.
    fn open12(&mut self, ct: u8, body: Vec<u8>) -> Result<Vec<u8>, TlsError> {
        let explicit = self.explicit_nonce();
        let overhead = if explicit { 8 + 16 } else { 16 };
        if body.len() < overhead {
            return Err(TlsError::BadRecord);
        }
        let aad = self.aad12(ct, body.len() - overhead);
        let mut n = self.nonce()?;
        let mut data = if explicit {
            n[4..].copy_from_slice(&body[..8]);
            body[8..].to_vec()
        } else {
            body
        };
        let r = match &self.aead {
            Aead::Aes(a) => a.open(&n, &aad, &mut data),
            Aead::ChaCha(a) => a.open(&n, &aad, &mut data),
        };
        r.map_err(|_| TlsError::BadRecord)?;
        Ok(data)
    }

    fn nonce(&mut self) -> Result<[u8; 12], TlsError> {
        if self.seq == u64::MAX {
            return Err(proto("too many records"));
        }
        let mut n = self.iv;
        for (i, b) in self.seq.to_be_bytes().iter().enumerate() {
            n[4 + i] ^= b;
        }
        self.seq += 1;
        Ok(n)
    }

    fn seal(&mut self, aad: &[u8], data: &mut Vec<u8>) -> Result<(), TlsError> {
        let n = self.nonce()?;
        match &self.aead {
            Aead::Aes(a) => a.seal(&n, aad, data),
            Aead::ChaCha(a) => a.seal(&n, aad, data),
        }
        Ok(())
    }

    fn open(&mut self, aad: &[u8], data: &mut Vec<u8>) -> Result<(), TlsError> {
        let n = self.nonce()?;
        let r = match &self.aead {
            Aead::Aes(a) => a.open(&n, aad, data),
            Aead::ChaCha(a) => a.open(&n, aad, data),
        };
        r.map_err(|_| TlsError::BadRecord)
    }

    /// The next generation of these keys (KeyUpdate).
    fn updated(&self) -> Keys {
        let next = self.kdf.expand_label(&self.secret, "traffic upd", &[], self.kdf.len());
        Keys::new(self.suite, self.kdf, next)
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        wipe(&mut self.secret);
        wipe(&mut self.iv);
    }
}

// ---- Parsing helpers --------------------------------------------------------

struct Cur<'a> {
    d: &'a [u8],
}

impl<'a> Cur<'a> {
    fn new(d: &'a [u8]) -> Self {
        Cur { d }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], TlsError> {
        if self.d.len() < n {
            return Err(proto("a message was truncated"));
        }
        let (a, b) = self.d.split_at(n);
        self.d = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, TlsError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, TlsError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u24(&mut self) -> Result<usize, TlsError> {
        let b = self.take(3)?;
        Ok(((b[0] as usize) << 16) | ((b[1] as usize) << 8) | b[2] as usize)
    }
    fn vec8(&mut self) -> Result<&'a [u8], TlsError> {
        let n = self.u8()? as usize;
        self.take(n)
    }
    fn vec16(&mut self) -> Result<&'a [u8], TlsError> {
        let n = self.u16()? as usize;
        self.take(n)
    }
    fn vec24(&mut self) -> Result<&'a [u8], TlsError> {
        let n = self.u24()?;
        self.take(n)
    }
    fn done(&self) -> Result<(), TlsError> {
        if self.d.is_empty() {
            Ok(())
        } else {
            Err(proto("a message had trailing data"))
        }
    }
}

/// Splits an extensions block into (type, data), rejecting duplicates.
fn extensions(block: &[u8]) -> Result<Vec<(u16, &[u8])>, TlsError> {
    let mut c = Cur::new(block);
    let mut out: Vec<(u16, &[u8])> = Vec::new();
    while !c.d.is_empty() {
        let t = c.u16()?;
        let d = c.vec16()?;
        if out.iter().any(|(x, _)| *x == t) {
            return Err(proto("a duplicated extension"));
        }
        out.push((t, d));
    }
    Ok(out)
}

fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn put_vec16(v: &mut Vec<u8>, d: &[u8]) {
    put_u16(v, d.len() as u16);
    v.extend_from_slice(d);
}

fn ext(v: &mut Vec<u8>, t: u16, d: &[u8]) {
    put_u16(v, t);
    put_vec16(v, d);
}

fn handshake_msg(t: u8, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(4 + body.len());
    m.push(t);
    let n = body.len();
    m.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    m.extend_from_slice(body);
    m
}

// ---- The connection ---------------------------------------------------------

/// Options for one connection.
#[derive(Clone)]
pub struct TlsConfig<'a> {
    pub roots: &'a RootStore,
    /// Unix seconds, for certificate validity.
    pub now: i64,
    pub suites: &'a [u16],
    /// ALPN protocols to offer (we always want "http/1.1").
    pub alpn: &'a [&'a str],
}

/// What was negotiated.
#[derive(Clone, Debug)]
pub struct SessionInfo {
    /// 0x0304 for TLS 1.3, 0x0303 for TLS 1.2.
    pub version: u16,
    pub suite: u16,
    pub alpn: Option<String>,
    /// The signature scheme of CertificateVerify (1.3) or ServerKeyExchange (1.2).
    pub signature_scheme: u16,
    /// The key exchange group.
    pub group: u16,
}

pub fn suite_name(s: u16) -> &'static str {
    match s {
        TLS_AES_128_GCM_SHA256 => "TLS_AES_128_GCM_SHA256",
        TLS_AES_256_GCM_SHA384 => "TLS_AES_256_GCM_SHA384",
        TLS_CHACHA20_POLY1305_SHA256 => "TLS_CHACHA20_POLY1305_SHA256",
        TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256 => "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
        TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384 => "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
        TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256 => "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
        TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384 => "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
        TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256 => "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
        TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256 => "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
        _ => "unknown",
    }
}

pub struct TlsStream<S: Read + Write> {
    sock: S,
    read_keys: Option<Keys>,
    write_keys: Option<Keys>,
    /// Handshake bytes received but not yet consumed as messages.
    hs_buf: Vec<u8>,
    /// Decrypted application data not yet returned to the caller.
    app_buf: Vec<u8>,
    app_pos: usize,
    closed: bool,
    failed: bool,
    sent_close: bool,
    info: Option<SessionInfo>,
    ccs_seen: bool,
    handshake_done: bool,
    /// TLS 1.2 was negotiated.
    tls12: bool,
    /// TLS 1.2: the server's keys, switched on by its ChangeCipherSpec.
    pending_read: Option<Keys>,
}

enum Record {
    Handshake,
    AppData,
    Closed,
}

impl<S: Read + Write> TlsStream<S> {
    /// Runs the handshake over `sock` with the server `host` (used for SNI
    /// and certificate checks). On success the stream is ready for data.
    pub fn connect(sock: S, host: &str, cfg: &TlsConfig) -> Result<TlsStream<S>, TlsError> {
        let mut t = TlsStream {
            sock,
            read_keys: None,
            write_keys: None,
            hs_buf: Vec::new(),
            app_buf: Vec::new(),
            app_pos: 0,
            closed: false,
            failed: false,
            sent_close: false,
            info: None,
            ccs_seen: false,
            handshake_done: false,
            tls12: false,
            pending_read: None,
        };
        match t.handshake(host, cfg) {
            Ok(()) => Ok(t),
            Err(e) => {
                let alert = match &e {
                    TlsError::Io(_) | TlsError::Alert(_) => None,
                    TlsError::NoTls13 => Some(A_PROTOCOL_VERSION),
                    TlsError::Cert(CertError::Expired | CertError::NotYetValid) => Some(A_CERTIFICATE_EXPIRED),
                    TlsError::Cert(CertError::UnknownIssuer) => Some(A_UNKNOWN_CA),
                    TlsError::Cert(CertError::UnsupportedKey) => Some(A_UNSUPPORTED_CERTIFICATE),
                    TlsError::Cert(_) => Some(A_BAD_CERTIFICATE),
                    TlsError::BadRecord => Some(A_BAD_RECORD_MAC),
                    TlsError::BadHandshakeSignature => Some(A_DECRYPT_ERROR),
                    TlsError::Protocol(w) if w.contains("extension") => Some(A_UNSUPPORTED_EXTENSION),
                    TlsError::Protocol(w) if w.contains("truncated") || w.contains("trailing") => Some(A_DECODE_ERROR),
                    TlsError::Protocol(w) if w.contains("unexpected") => Some(A_UNEXPECTED_MESSAGE),
                    TlsError::Protocol(w) if w.contains("too large") => Some(A_RECORD_OVERFLOW),
                    TlsError::Protocol(w) if w.contains("ALPN") => Some(A_NO_APPLICATION_PROTOCOL),
                    TlsError::Protocol(w) if w.contains("no common") => Some(A_HANDSHAKE_FAILURE),
                    TlsError::Protocol(_) => Some(A_ILLEGAL_PARAMETER),
                };
                if let Some(a) = alert {
                    let _ = t.send_alert(a);
                }
                Err(e)
            }
        }
    }

    pub fn info(&self) -> Option<&SessionInfo> {
        self.info.as_ref()
    }

    pub fn get_ref(&self) -> &S {
        &self.sock
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.sock
    }

    fn handshake(&mut self, host: &str, cfg: &TlsConfig) -> Result<(), TlsError> {
        // ClientHello, with key shares for X25519 and P-256.
        let mut shares: Vec<Share> = FIRST_SHARES.iter().map(|&g| Share::new(g)).collect();
        let mut random = [0u8; 32];
        crypto::random(&mut random);
        let mut session_id = [0u8; 32];
        crypto::random(&mut session_id);
        let ch1 = client_hello(host, &random, &session_id, &shares, None, cfg);
        self.write_plain(CT_HANDSHAKE, &ch1)?;

        // ServerHello, or a HelloRetryRequest and then ServerHello.
        let sh = self.next_handshake(HS_SERVER_HELLO)?;
        let mut transcript = ch1.clone();
        let mut hello = parse_server_hello(&sh[4..], &session_id, cfg)?;
        if let ServerHello::Retry { suite, group, cookie } = hello {
            if let Some(g) = group {
                // It must ask for a group we offered and didn't already send.
                if !GROUPS.contains(&g) || shares.iter().any(|s| s.group == g) {
                    return Err(proto("the server's HelloRetryRequest asked for an unexpected key exchange"));
                }
                shares = vec![Share::new(g)];
            } else if cookie.is_none() {
                return Err(proto("a HelloRetryRequest that asks for nothing"));
            }
            // RFC 8446 section 4.4.1: ClientHello1 is replaced by its hash.
            let kdf = suite_kdf(suite);
            transcript = handshake_msg(254, &kdf.hash(&ch1));
            transcript.extend_from_slice(&sh);
            let ch2 = client_hello(host, &random, &session_id, &shares, cookie.as_deref(), cfg);
            transcript.extend_from_slice(&ch2);
            self.write_plain(CT_HANDSHAKE, &ch2)?;
            let sh2 = self.next_handshake(HS_SERVER_HELLO)?;
            hello = parse_server_hello(&sh2[4..], &session_id, cfg)?;
            match &hello {
                ServerHello::Tls13 { suite: s2, .. } if *s2 == suite => {}
                ServerHello::Tls13 { .. } => return Err(proto("the server changed its cipher suite after HelloRetryRequest")),
                _ => return Err(proto("unexpected message after HelloRetryRequest")),
            }
            transcript.extend_from_slice(&sh2);
        } else {
            transcript.extend_from_slice(&sh);
        }
        match hello {
            ServerHello::Tls13 { suite, group, share } => {
                let ours = shares
                    .iter()
                    .find(|s| s.group == group)
                    .ok_or(proto("the server chose a key exchange we sent no share for"))?;
                let mut shared = ours.agree(&share).ok_or(proto("the server's key share is invalid"))?;
                drop(shares);
                let r = self.handshake13(host, cfg, transcript, suite, group, &shared);
                wipe(&mut shared);
                r
            }
            ServerHello::Tls12 { suite, server_random, ems, alpn } => self.handshake12(
                host,
                cfg,
                transcript,
                Hello12 { suite, client_random: random, server_random, ems, alpn },
                shares,
            ),
            ServerHello::Retry { .. } => unreachable!(),
        }
    }

    /// The rest of a TLS 1.3 handshake, after ServerHello.
    fn handshake13(
        &mut self,
        host: &str,
        cfg: &TlsConfig,
        mut transcript: Vec<u8>,
        suite: u16,
        group: u16,
        shared: &[u8],
    ) -> Result<(), TlsError> {
        let kdf = suite_kdf(suite);
        let zeros = vec![0u8; kdf.len()];
        let early = kdf.extract(&[], &zeros);
        let derived = kdf.derive(&early, "derived", &kdf.hash(&[]));
        let mut hs_secret = kdf.extract(&derived, shared);
        let th = kdf.hash(&transcript);
        let c_hs = kdf.derive(&hs_secret, "c hs traffic", &th);
        let s_hs = kdf.derive(&hs_secret, "s hs traffic", &th);
        if !self.hs_buf.is_empty() {
            return Err(proto("handshake data crossed a key change"));
        }
        self.read_keys = Some(Keys::new(suite, kdf, s_hs.clone()));

        // EncryptedExtensions.
        let ee = self.next_handshake(HS_ENCRYPTED_EXTENSIONS)?;
        let alpn = parse_encrypted_extensions(&ee[4..], cfg.alpn)?;
        transcript.extend_from_slice(&ee);

        // CertificateRequest (optional), Certificate, CertificateVerify.
        let mut msg = self.next_handshake(0)?;
        let mut cert_request_ctx: Option<Vec<u8>> = None;
        if msg[0] == HS_CERTIFICATE_REQUEST {
            let mut c = Cur::new(&msg[4..]);
            cert_request_ctx = Some(c.vec8()?.to_vec());
            let _exts = extensions(c.vec16()?)?;
            c.done()?;
            transcript.extend_from_slice(&msg);
            msg = self.next_handshake(0)?;
        }
        if msg[0] != HS_CERTIFICATE {
            return Err(proto("unexpected message instead of the certificate"));
        }
        let chain = parse_certificate(&msg[4..])?;
        transcript.extend_from_slice(&msg);
        let leaf = x509::verify_chain(&chain, host, cfg.now, cfg.roots).map_err(TlsError::Cert)?;

        let cv = self.next_handshake(HS_CERTIFICATE_VERIFY)?;
        let th = kdf.hash(&transcript);
        let scheme = check_certificate_verify(&cv[4..], &leaf.key, &th)?;
        transcript.extend_from_slice(&cv);

        // Server Finished.
        let fin = self.next_handshake(HS_FINISHED)?;
        let s_fin_key = kdf.expand_label(&s_hs, "finished", &[], kdf.len());
        let expect = kdf.hmac(&s_fin_key, &kdf.hash(&transcript));
        if fin.len() != 4 + kdf.len() || !ct_eq(&fin[4..], &expect) {
            return Err(TlsError::BadHandshakeSignature);
        }
        transcript.extend_from_slice(&fin);
        if !self.hs_buf.is_empty() {
            return Err(proto("handshake data crossed a key change"));
        }

        // Application secrets (from the transcript through server Finished).
        let derived = kdf.derive(&hs_secret, "derived", &kdf.hash(&[]));
        let mut master = kdf.extract(&derived, &zeros);
        wipe(&mut hs_secret);
        let th = kdf.hash(&transcript);
        let c_ap = kdf.derive(&master, "c ap traffic", &th);
        let s_ap = kdf.derive(&master, "s ap traffic", &th);
        wipe(&mut master);

        // Our second flight: a compatibility ChangeCipherSpec (we sent a
        // session id), an empty Certificate if asked, and Finished.
        self.write_plain(CT_CCS, &[1])?;
        self.write_keys = Some(Keys::new(suite, kdf, c_hs.clone()));
        if let Some(ctx) = cert_request_ctx {
            let mut body = Vec::new();
            body.push(ctx.len() as u8);
            body.extend_from_slice(&ctx);
            body.extend_from_slice(&[0, 0, 0]); // no certificates
            let m = handshake_msg(HS_CERTIFICATE, &body);
            transcript.extend_from_slice(&m);
            self.write_record(CT_HANDSHAKE, &m)?;
        }
        let c_fin_key = kdf.expand_label(&c_hs, "finished", &[], kdf.len());
        let verify = kdf.hmac(&c_fin_key, &kdf.hash(&transcript));
        let m = handshake_msg(HS_FINISHED, &verify);
        self.write_record(CT_HANDSHAKE, &m)?;
        self.sock.flush()?;

        self.read_keys = Some(Keys::new(suite, kdf, s_ap));
        self.write_keys = Some(Keys::new(suite, kdf, c_ap));
        self.handshake_done = true;
        self.info = Some(SessionInfo { version: 0x0304, suite, alpn, signature_scheme: scheme, group });
        Ok(())
    }

    /// The rest of a TLS 1.2 handshake (RFC 5246 section 7.3, RFC 8422),
    /// after ServerHello: Certificate, ServerKeyExchange, [CertificateRequest],
    /// ServerHelloDone; then ours: [Certificate], ClientKeyExchange,
    /// ChangeCipherSpec, Finished; then the server's ChangeCipherSpec, Finished.
    fn handshake12(
        &mut self,
        host: &str,
        cfg: &TlsConfig,
        mut transcript: Vec<u8>,
        hello: Hello12,
        shares: Vec<Share>,
    ) -> Result<(), TlsError> {
        self.tls12 = true;
        let suite = hello.suite;
        let kdf = suite_kdf(suite);

        let msg = self.next_handshake(HS_CERTIFICATE)?;
        let chain = parse_certificate12(&msg[4..])?;
        transcript.extend_from_slice(&msg);
        let leaf = x509::verify_chain(&chain, host, cfg.now, cfg.roots).map_err(TlsError::Cert)?;
        let key_fits = match leaf.key {
            PublicKey::Ec(..) => tls12_suite_is_ecdsa(suite),
            PublicKey::Rsa(_) => !tls12_suite_is_ecdsa(suite),
        };
        if !key_fits {
            return Err(proto("the server's certificate doesn't fit the cipher suite it chose"));
        }

        let ske = self.next_handshake(HS_SERVER_KEY_EXCHANGE)?;
        let (group, server_point, scheme) =
            check_server_key_exchange(&ske[4..], &leaf.key, &hello.client_random, &hello.server_random)?;
        transcript.extend_from_slice(&ske);

        let mut msg = self.next_handshake(0)?;
        let mut cert_requested = false;
        if msg[0] == HS_CERTIFICATE_REQUEST {
            let mut c = Cur::new(&msg[4..]);
            let _types = c.vec8()?;
            let _sigs = c.vec16()?;
            let _cas = c.vec16()?;
            c.done()?;
            cert_requested = true;
            transcript.extend_from_slice(&msg);
            msg = self.next_handshake(0)?;
        }
        if msg[0] != HS_SERVER_HELLO_DONE || msg.len() != 4 {
            return Err(proto("unexpected message instead of ServerHelloDone"));
        }
        transcript.extend_from_slice(&msg);
        if !self.hs_buf.is_empty() {
            return Err(proto("unexpected data after ServerHelloDone"));
        }

        // A P-384 server gets a key made now (TLS 1.2 sends ours after its).
        let share = match shares.into_iter().find(|s| s.group == group) {
            Some(s) => s,
            None => Share::new(group),
        };
        let mut pms = share.agree(&server_point).ok_or(proto("the server's key share is invalid"))?;
        let our_public = &share.public[..];

        // Our flight.
        if cert_requested {
            let m = handshake_msg(HS_CERTIFICATE, &[0, 0, 0]); // no certificates
            transcript.extend_from_slice(&m);
            self.write_plain(CT_HANDSHAKE, &m)?;
        }
        let mut cke = vec![our_public.len() as u8];
        cke.extend_from_slice(our_public);
        let m = handshake_msg(HS_CLIENT_KEY_EXCHANGE, &cke);
        transcript.extend_from_slice(&m);
        self.write_plain(CT_HANDSHAKE, &m)?;

        let mut master = if hello.ems {
            kdf.prf12(&pms, "extended master secret", &kdf.hash(&transcript), 48)
        } else {
            let mut seed = hello.client_random.to_vec();
            seed.extend_from_slice(&hello.server_random);
            kdf.prf12(&pms, "master secret", &seed, 48)
        };
        wipe(&mut pms);
        let mut seed = hello.server_random.to_vec();
        seed.extend_from_slice(&hello.client_random);
        let klen = key_len(suite);
        let ivlen = if is_chacha(suite) { 12 } else { 4 };
        let mut block = kdf.prf12(&master, "key expansion", &seed, 2 * klen + 2 * ivlen);
        let client_keys = Keys::new12(suite, &block[..klen], &block[2 * klen..2 * klen + ivlen]);
        let server_keys = Keys::new12(suite, &block[klen..2 * klen], &block[2 * klen + ivlen..]);
        wipe(&mut block);

        self.write_plain(CT_CCS, &[1])?;
        self.write_keys = Some(client_keys);
        let verify = kdf.prf12(&master, "client finished", &kdf.hash(&transcript), 12);
        let m = handshake_msg(HS_FINISHED, &verify);
        transcript.extend_from_slice(&m);
        self.write_record(CT_HANDSHAKE, &m)?;
        self.sock.flush()?;

        // The server's ChangeCipherSpec (read_record switches the keys) and Finished.
        self.pending_read = Some(server_keys);
        let fin = self.next_handshake(HS_FINISHED);
        let fin = match fin {
            Ok(f) => f,
            Err(e) => {
                wipe(&mut master);
                return Err(e);
            }
        };
        let expect = kdf.prf12(&master, "server finished", &kdf.hash(&transcript), 12);
        wipe(&mut master);
        if self.read_keys.is_none() || fin.len() != 4 + 12 || !ct_eq(&fin[4..], &expect) {
            return Err(TlsError::BadHandshakeSignature);
        }
        if !self.hs_buf.is_empty() {
            return Err(proto("unexpected data after the server's Finished"));
        }
        self.handshake_done = true;
        self.info = Some(SessionInfo { version: 0x0303, suite, alpn: hello.alpn, signature_scheme: scheme, group });
        Ok(())
    }

    // ---- records ----

    fn write_plain(&mut self, ct: u8, data: &[u8]) -> Result<(), TlsError> {
        for chunk in data.chunks(MAX_PLAINTEXT) {
            let mut rec = Vec::with_capacity(5 + chunk.len());
            rec.push(ct);
            // legacy_record_version: 0x0301 for the ClientHello, as many
            // servers expect; 0x0303 otherwise.
            rec.extend_from_slice(if ct == CT_HANDSHAKE && data[0] == HS_CLIENT_HELLO { &[3, 1] } else { &[3, 3] });
            put_u16(&mut rec, chunk.len() as u16);
            rec.extend_from_slice(chunk);
            self.sock.write_all(&rec)?;
        }
        Ok(())
    }

    fn write_record(&mut self, ct: u8, data: &[u8]) -> Result<(), TlsError> {
        if self.tls12 {
            for chunk in data.chunks(MAX_PLAINTEXT) {
                let keys = self.write_keys.as_mut().ok_or(proto("no keys"))?;
                let body = keys.seal12(ct, chunk)?;
                let mut rec = Vec::with_capacity(5 + body.len());
                rec.extend_from_slice(&[ct, 3, 3]);
                put_u16(&mut rec, body.len() as u16);
                rec.extend_from_slice(&body);
                self.sock.write_all(&rec)?;
            }
            return Ok(());
        }
        for chunk in data.chunks(MAX_PLAINTEXT) {
            let keys = self.write_keys.as_mut().ok_or(proto("no keys"))?;
            let mut inner = Vec::with_capacity(chunk.len() + 17);
            inner.extend_from_slice(chunk);
            inner.push(ct);
            let len = inner.len() + 16;
            let hdr = [CT_APPDATA, 3, 3, (len >> 8) as u8, len as u8];
            keys.seal(&hdr, &mut inner)?;
            let mut rec = Vec::with_capacity(5 + inner.len());
            rec.extend_from_slice(&hdr);
            rec.extend_from_slice(&inner);
            self.sock.write_all(&rec)?;
        }
        Ok(())
    }

    fn send_alert(&mut self, desc: u8) -> Result<(), TlsError> {
        let level = if desc == A_CLOSE_NOTIFY { 1 } else { 2 };
        if self.write_keys.is_some() {
            self.write_record(CT_ALERT, &[level, desc])?;
        } else {
            self.write_plain(CT_ALERT, &[level, desc])?;
        }
        self.sock.flush()?;
        Ok(())
    }

    /// Reads one record; handshake bytes go to hs_buf, application data to app_buf.
    fn read_record(&mut self) -> Result<Record, TlsError> {
        loop {
            let mut hdr = [0u8; 5];
            match self.sock.read_exact(&mut hdr) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Err(TlsError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the connection was cut off (no TLS close_notify)",
                    )))
                }
                Err(e) => return Err(e.into()),
            }
            let ct = hdr[0];
            if hdr[1] != 3 {
                return Err(if self.read_keys.is_none() { TlsError::NoTls13 } else { proto("bad record version") });
            }
            let len = u16::from_be_bytes([hdr[3], hdr[4]]) as usize;
            if len > MAX_CIPHERTEXT {
                return Err(proto("a record was too large"));
            }
            let mut body = vec![0u8; len];
            self.sock.read_exact(&mut body)?;

            if ct == CT_CCS {
                // Allowed once, during the handshake, as exactly [1].
                if self.handshake_done || self.ccs_seen || body != [1] {
                    return Err(proto("unexpected ChangeCipherSpec"));
                }
                self.ccs_seen = true;
                if self.tls12 {
                    // TLS 1.2: it switches on the server's keys, after our
                    // Finished and before the server's, on a message boundary.
                    let Some(k) = self.pending_read.take() else {
                        return Err(proto("unexpected ChangeCipherSpec"));
                    };
                    if !self.hs_buf.is_empty() {
                        return Err(proto("handshake data crossed a key change"));
                    }
                    self.read_keys = Some(k);
                }
                continue;
            }

            let (ct, data) = match self.read_keys.as_mut() {
                None if self.pending_read.is_some() && ct == CT_HANDSHAKE => {
                    // TLS 1.2: the server's Finished must come encrypted.
                    return Err(proto("unexpected handshake message before ChangeCipherSpec"));
                }
                Some(keys) if keys.tls12 => {
                    if ct != CT_APPDATA && ct != CT_HANDSHAKE && ct != CT_ALERT {
                        return Err(proto("unexpected record type"));
                    }
                    let d = keys.open12(ct, body)?;
                    if d.len() > MAX_PLAINTEXT {
                        return Err(proto("a record was too large"));
                    }
                    (ct, d)
                }
                None => {
                    if len > MAX_PLAINTEXT {
                        return Err(proto("a record was too large"));
                    }
                    (ct, body)
                }
                Some(keys) => {
                    if ct != CT_APPDATA {
                        if ct == CT_ALERT && len == 2 {
                            // A plaintext alert after keys: the server couldn't
                            // read ours. Report it.
                            return Err(TlsError::Alert(body[1]));
                        }
                        return Err(proto("unexpected unencrypted record"));
                    }
                    let mut d = body;
                    keys.open(&hdr, &mut d)?;
                    // Strip padding; the last non-zero byte is the real type.
                    let Some(pos) = d.iter().rposition(|&b| b != 0) else {
                        return Err(proto("unexpected record without a type"));
                    };
                    let inner = d[pos];
                    d.truncate(pos);
                    if d.len() > MAX_PLAINTEXT {
                        return Err(proto("a record was too large"));
                    }
                    (inner, d)
                }
            };

            match ct {
                CT_HANDSHAKE => {
                    if data.is_empty() {
                        return Err(proto("unexpected empty handshake record"));
                    }
                    if self.hs_buf.len() + data.len() > MAX_HANDSHAKE_MSG + 4 {
                        return Err(proto("a handshake message was too large"));
                    }
                    self.hs_buf.extend_from_slice(&data);
                    return Ok(Record::Handshake);
                }
                CT_APPDATA => {
                    if !self.handshake_done {
                        return Err(proto("unexpected application data during the handshake"));
                    }
                    if !self.hs_buf.is_empty() {
                        return Err(proto("unexpected data inside a handshake message"));
                    }
                    if data.is_empty() {
                        continue;
                    }
                    self.app_buf = data;
                    self.app_pos = 0;
                    return Ok(Record::AppData);
                }
                CT_ALERT => {
                    if data.len() != 2 {
                        return Err(proto("a malformed alert"));
                    }
                    if data[1] == A_CLOSE_NOTIFY {
                        self.closed = true;
                        return Ok(Record::Closed);
                    }
                    // user_canceled is followed by close_notify; everything else is fatal.
                    if data[1] == 90 {
                        continue;
                    }
                    return Err(TlsError::Alert(data[1]));
                }
                _ => return Err(proto("unexpected record type")),
            }
        }
    }

    /// The next complete handshake message (type + length + body).
    /// `want` = 0 accepts any type.
    fn next_handshake(&mut self, want: u8) -> Result<Vec<u8>, TlsError> {
        loop {
            if self.hs_buf.len() >= 4 {
                let n = ((self.hs_buf[1] as usize) << 16) | ((self.hs_buf[2] as usize) << 8) | self.hs_buf[3] as usize;
                if n > MAX_HANDSHAKE_MSG {
                    return Err(proto("a handshake message was too large"));
                }
                if self.hs_buf.len() >= 4 + n {
                    let msg: Vec<u8> = self.hs_buf.drain(..4 + n).collect();
                    if want != 0 && msg[0] != want {
                        return Err(proto("unexpected handshake message"));
                    }
                    return Ok(msg);
                }
            }
            match self.read_record()? {
                Record::Handshake => {}
                Record::AppData => return Err(proto("unexpected application data")),
                Record::Closed => return Err(proto("unexpected close during the handshake")),
            }
        }
    }

    /// Handles NewSessionTicket and KeyUpdate after the handshake.
    fn post_handshake(&mut self) -> Result<(), TlsError> {
        while self.hs_buf.len() >= 4 {
            let n = ((self.hs_buf[1] as usize) << 16) | ((self.hs_buf[2] as usize) << 8) | self.hs_buf[3] as usize;
            if n > MAX_HANDSHAKE_MSG {
                return Err(proto("a handshake message was too large"));
            }
            if self.hs_buf.len() < 4 + n {
                return Ok(());
            }
            let msg: Vec<u8> = self.hs_buf.drain(..4 + n).collect();
            match msg[0] {
                // TLS 1.2: a request to renegotiate, which we never do; ignoring it is allowed.
                HS_HELLO_REQUEST if self.tls12 && msg.len() == 4 => {}
                HS_NEW_SESSION_TICKET if !self.tls12 => {} // no resumption: ignore
                HS_KEY_UPDATE if !self.tls12 => {
                    if msg.len() != 5 || msg[4] > 1 {
                        return Err(proto("a malformed KeyUpdate"));
                    }
                    if !self.hs_buf.is_empty() {
                        return Err(proto("handshake data crossed a key change"));
                    }
                    let next = self.read_keys.as_ref().ok_or(proto("no keys"))?.updated();
                    self.read_keys = Some(next);
                    if msg[4] == 1 {
                        // update_requested: answer with ours, then switch.
                        self.write_record(CT_HANDSHAKE, &handshake_msg(HS_KEY_UPDATE, &[0]))?;
                        let next = self.write_keys.as_ref().ok_or(proto("no keys"))?.updated();
                        self.write_keys = Some(next);
                    }
                }
                _ => return Err(proto("unexpected handshake message after the handshake")),
            }
        }
        Ok(())
    }

    /// Sends close_notify. Further writes fail.
    pub fn close(&mut self) -> io::Result<()> {
        if !self.sent_close && !self.failed {
            self.sent_close = true;
            self.send_alert(A_CLOSE_NOTIFY)?;
        }
        Ok(())
    }
}

impl<S: Read + Write> Read for TlsStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the secure connection failed earlier"));
        }
        loop {
            if self.app_pos < self.app_buf.len() {
                let n = buf.len().min(self.app_buf.len() - self.app_pos);
                buf[..n].copy_from_slice(&self.app_buf[self.app_pos..self.app_pos + n]);
                self.app_pos += n;
                return Ok(n);
            }
            if self.closed || buf.is_empty() {
                return Ok(0);
            }
            match self.read_record() {
                Ok(Record::AppData) => {}
                Ok(Record::Closed) => return Ok(0),
                Ok(Record::Handshake) => {
                    if let Err(e) = self.post_handshake() {
                        self.failed = true;
                        let _ = self.send_alert(A_UNEXPECTED_MESSAGE);
                        return Err(e.into());
                    }
                }
                Err(e) => {
                    // Timeouts are not fatal to the session; everything else is.
                    if let TlsError::Io(ioe) = &e {
                        if matches!(ioe.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) {
                            // A partial record may have been consumed: the stream
                            // can't be resynchronized, so it is failed too.
                            self.failed = true;
                            return Err(e.into());
                        }
                    }
                    self.failed = true;
                    match &e {
                        TlsError::BadRecord => {
                            let _ = self.send_alert(A_BAD_RECORD_MAC);
                        }
                        TlsError::Protocol(_) => {
                            let _ = self.send_alert(A_UNEXPECTED_MESSAGE);
                        }
                        _ => {}
                    }
                    return Err(e.into());
                }
            }
        }
    }
}

impl<S: Read + Write> Write for TlsStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.failed || self.sent_close {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the secure connection is closed"));
        }
        let n = buf.len().min(MAX_PLAINTEXT);
        if let Err(e) = self.write_record(CT_APPDATA, &buf[..n]) {
            self.failed = true;
            return Err(e.into());
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sock.flush()
    }
}

fn client_hello(
    host: &str,
    random: &[u8; 32],
    session_id: &[u8; 32],
    shares: &[Share],
    cookie: Option<&[u8]>,
    cfg: &TlsConfig,
) -> Vec<u8> {
    let mut b = Vec::with_capacity(512);
    b.extend_from_slice(&[3, 3]);
    b.extend_from_slice(random);
    b.push(32);
    b.extend_from_slice(session_id);
    let mut suites = Vec::new();
    for &s in cfg.suites {
        put_u16(&mut suites, s);
    }
    put_vec16(&mut b, &suites);
    b.extend_from_slice(&[1, 0]); // compression: null only

    let mut exts = Vec::new();
    // SNI: only for names (RFC 6066 forbids IP literals).
    if x509::parse_ip(host).is_none() {
        let mut list = vec![0u8]; // host_name
        put_vec16(&mut list, host.as_bytes());
        let mut sni = Vec::new();
        put_vec16(&mut sni, &list);
        ext(&mut exts, EXT_SERVER_NAME, &sni);
    }
    let offer12 = cfg.suites.iter().any(|&s| is_tls12_suite(s));
    if offer12 {
        ext(&mut exts, EXT_SUPPORTED_VERSIONS, &[4, 3, 4, 3, 3]);
        // TLS 1.2 extras: uncompressed points only, the extended master
        // secret, and the (empty) secure renegotiation indication.
        ext(&mut exts, EXT_EC_POINT_FORMATS, &[1, 0]);
        ext(&mut exts, EXT_EXTENDED_MASTER_SECRET, &[]);
        ext(&mut exts, EXT_RENEGOTIATION_INFO, &[0]);
    } else {
        ext(&mut exts, EXT_SUPPORTED_VERSIONS, &[2, 3, 4]);
    }
    let mut groups = Vec::new();
    for &g in GROUPS {
        put_u16(&mut groups, g);
    }
    let mut g = Vec::new();
    put_vec16(&mut g, &groups);
    ext(&mut exts, EXT_SUPPORTED_GROUPS, &g);
    let sig_list = |l: &[u16]| {
        let mut s = Vec::new();
        for &x in l {
            put_u16(&mut s, x);
        }
        let mut v = Vec::new();
        put_vec16(&mut v, &s);
        v
    };
    ext(&mut exts, EXT_SIGNATURE_ALGORITHMS, &sig_list(if offer12 { OFFERED_SIGS } else { HANDSHAKE_SIGS }));
    ext(&mut exts, EXT_SIGNATURE_ALGORITHMS_CERT, &sig_list(CERT_SIGS));
    let mut list = Vec::new();
    for s in shares {
        put_u16(&mut list, s.group);
        put_vec16(&mut list, &s.public);
    }
    let mut ks = Vec::new();
    put_vec16(&mut ks, &list);
    ext(&mut exts, EXT_KEY_SHARE, &ks);
    if let Some(c) = cookie {
        let mut v = Vec::new();
        put_vec16(&mut v, c);
        ext(&mut exts, EXT_COOKIE, &v);
    }
    if !cfg.alpn.is_empty() {
        let mut list = Vec::new();
        for p in cfg.alpn {
            list.push(p.len() as u8);
            list.extend_from_slice(p.as_bytes());
        }
        let mut a = Vec::new();
        put_vec16(&mut a, &list);
        ext(&mut exts, EXT_ALPN, &a);
    }
    put_vec16(&mut b, &exts);
    handshake_msg(HS_CLIENT_HELLO, &b)
}

enum ServerHello {
    Tls13 { suite: u16, group: u16, share: Vec<u8> },
    /// A HelloRetryRequest: the group to send a share for, and a cookie to echo.
    Retry { suite: u16, group: Option<u16>, cookie: Option<Vec<u8>> },
    Tls12 { suite: u16, server_random: [u8; 32], ems: bool, alpn: Option<String> },
}

/// What a TLS 1.2 handshake needs from the hellos.
struct Hello12 {
    suite: u16,
    client_random: [u8; 32],
    server_random: [u8; 32],
    ems: bool,
    alpn: Option<String>,
}

fn parse_server_hello(body: &[u8], session_id: &[u8; 32], cfg: &TlsConfig) -> Result<ServerHello, TlsError> {
    let mut c = Cur::new(body);
    let version = c.u16()?;
    let random = c.take(32)?;
    let sid = c.vec8()?;
    let suite = c.u16()?;
    let comp = c.u8()?;
    let exts = if c.d.is_empty() { Vec::new() } else { extensions(c.vec16()?)? };
    c.done()?;
    if !cfg.suites.contains(&suite) {
        return Err(proto("the server chose a cipher suite we didn't offer"));
    }
    if comp != 0 {
        return Err(proto("the server chose compression"));
    }
    let sv = exts.iter().find(|(t, _)| *t == EXT_SUPPORTED_VERSIONS);
    if version != 0x0303 {
        // TLS 1.1 or older (or garbage).
        return Err(TlsError::NoTls13);
    }
    let Some((_, sv)) = sv else {
        return parse_server_hello12(random, suite, &exts, cfg);
    };
    if *sv != [3, 4] {
        return Err(TlsError::NoTls13);
    }
    if sid != session_id {
        return Err(proto("the server changed the session id"));
    }
    if !is_tls13_suite(suite) {
        return Err(proto("the server chose a TLS 1.2 cipher suite for TLS 1.3"));
    }
    if random == HRR_RANDOM {
        let mut group = None;
        let mut cookie = None;
        for (t, d) in &exts {
            match *t {
                EXT_SUPPORTED_VERSIONS => {}
                EXT_KEY_SHARE => {
                    let mut k = Cur::new(d);
                    group = Some(k.u16()?);
                    k.done()?;
                }
                EXT_COOKIE => {
                    let mut k = Cur::new(d);
                    let c = k.vec16()?;
                    k.done()?;
                    if c.is_empty() {
                        return Err(proto("an empty HelloRetryRequest cookie"));
                    }
                    cookie = Some(c.to_vec());
                }
                _ => return Err(proto("the server sent an extension we didn't offer")),
            }
        }
        return Ok(ServerHello::Retry { suite, group, cookie });
    }
    let mut share = None;
    for (t, d) in &exts {
        match *t {
            EXT_SUPPORTED_VERSIONS => {}
            EXT_KEY_SHARE => {
                let mut k = Cur::new(d);
                let group = k.u16()?;
                let key = k.vec16()?;
                k.done()?;
                let Some(want) = share_len(group) else {
                    return Err(proto("the server chose a key exchange we didn't offer"));
                };
                if key.len() != want {
                    return Err(proto("the server's key share is invalid"));
                }
                share = Some((group, key.to_vec()));
            }
            _ => return Err(proto("the server sent an extension we didn't offer")),
        }
    }
    let (group, share) = share.ok_or(proto("no common key exchange (the server sent no key share)"))?;
    Ok(ServerHello::Tls13 { suite, group, share })
}

/// A TLS 1.2 ServerHello (no supported_versions extension).
fn parse_server_hello12(
    random: &[u8],
    suite: u16,
    exts: &[(u16, &[u8])],
    cfg: &TlsConfig,
) -> Result<ServerHello, TlsError> {
    // We offered TLS 1.3. A server that has it marks a 1.2 answer; seeing
    // the mark means someone in the middle stripped 1.3 from our hello.
    if random[24..] == DOWNGRADE_TLS12 || random[24..] == DOWNGRADE_TLS11 {
        return Err(proto("the server signalled that the connection was downgraded (an attack, or a broken proxy)"));
    }
    if !is_tls12_suite(suite) {
        return Err(proto("the server chose a TLS 1.3 cipher suite for TLS 1.2"));
    }
    let mut ems = false;
    let mut alpn = None;
    for (t, d) in exts {
        match *t {
            EXT_SERVER_NAME => {
                if !d.is_empty() {
                    return Err(proto("a malformed server_name extension"));
                }
            }
            EXT_RENEGOTIATION_INFO => {
                if *d != [0] {
                    return Err(proto("the server's renegotiation_info is not empty"));
                }
            }
            EXT_EC_POINT_FORMATS => {
                let mut c = Cur::new(d);
                let f = c.vec8()?;
                c.done()?;
                if !f.contains(&0) {
                    return Err(proto("the server doesn't accept uncompressed points"));
                }
            }
            EXT_EXTENDED_MASTER_SECRET => {
                if !d.is_empty() {
                    return Err(proto("a malformed extended_master_secret extension"));
                }
                ems = true;
            }
            EXT_ALPN => alpn = Some(parse_alpn(d, cfg.alpn)?),
            _ => return Err(proto("the server sent an extension we didn't offer")),
        }
    }
    let mut server_random = [0u8; 32];
    server_random.copy_from_slice(random);
    Ok(ServerHello::Tls12 { suite, server_random, ems, alpn })
}

fn parse_alpn(d: &[u8], offered_alpn: &[&str]) -> Result<String, TlsError> {
    let mut a = Cur::new(d);
    let list = a.vec16()?;
    a.done()?;
    let mut l = Cur::new(list);
    let p = l.vec8()?;
    l.done()?;
    let p = std::str::from_utf8(p).map_err(|_| proto("the server chose an ALPN protocol we didn't offer"))?;
    if !offered_alpn.contains(&p) {
        return Err(proto("the server chose an ALPN protocol we didn't offer"));
    }
    Ok(p.to_string())
}

/// A TLS 1.2 Certificate message: a list of DER certificates.
fn parse_certificate12(body: &[u8]) -> Result<Vec<Vec<u8>>, TlsError> {
    let mut c = Cur::new(body);
    let list = c.vec24()?;
    c.done()?;
    let mut l = Cur::new(list);
    let mut out = Vec::new();
    while !l.d.is_empty() {
        let cert = l.vec24()?;
        if cert.is_empty() {
            return Err(proto("an empty certificate"));
        }
        out.push(cert.to_vec());
        if out.len() > 10 {
            return Err(TlsError::Cert(CertError::NotAllowed("too many certificates")));
        }
    }
    if out.is_empty() {
        return Err(TlsError::Cert(CertError::NoCertificate));
    }
    Ok(out)
}

/// Checks a TLS 1.2 ECDHE ServerKeyExchange (RFC 8422 section 5.4): the
/// server's signature over both randoms and its key share, with the key of
/// the certificate just validated. Returns (group, server point, scheme).
fn check_server_key_exchange(
    body: &[u8],
    key: &PublicKey,
    client_random: &[u8; 32],
    server_random: &[u8; 32],
) -> Result<(u16, Vec<u8>, u16), TlsError> {
    let mut c = Cur::new(body);
    if c.u8()? != 3 {
        return Err(proto("the server's key exchange doesn't use a named curve"));
    }
    let group = c.u16()?;
    let point = c.vec8()?;
    let params_len = body.len() - c.d.len();
    let scheme = c.u16()?;
    let sig = c.vec16()?;
    c.done()?;
    let Some(want) = share_len(group) else {
        return Err(proto("the server chose a key exchange we didn't offer"));
    };
    if point.len() != want {
        return Err(proto("the server's key share is invalid"));
    }
    // In TLS 1.2, 0x0403 and 0x0503 mean "ECDSA with SHA-256/384" on any curve.
    let alg = match (scheme, key) {
        (SIG_ECDSA_P256_SHA256, PublicKey::Ec(..)) => SigAlg::Ecdsa(HashAlg::Sha256),
        (SIG_ECDSA_P384_SHA384, PublicKey::Ec(..)) => SigAlg::Ecdsa(HashAlg::Sha384),
        (SIG_RSA_PSS_RSAE_SHA256, PublicKey::Rsa(_)) => SigAlg::RsaPss(HashAlg::Sha256),
        (SIG_RSA_PSS_RSAE_SHA384, PublicKey::Rsa(_)) => SigAlg::RsaPss(HashAlg::Sha384),
        (SIG_RSA_PSS_RSAE_SHA512, PublicKey::Rsa(_)) => SigAlg::RsaPss(HashAlg::Sha512),
        (SIG_RSA_PKCS1_SHA256, PublicKey::Rsa(_)) => SigAlg::RsaPkcs1(HashAlg::Sha256),
        (SIG_RSA_PKCS1_SHA384, PublicKey::Rsa(_)) => SigAlg::RsaPkcs1(HashAlg::Sha384),
        (SIG_RSA_PKCS1_SHA512, PublicKey::Rsa(_)) => SigAlg::RsaPkcs1(HashAlg::Sha512),
        _ => return Err(proto("the server used a signature we didn't offer, or one that doesn't fit its key")),
    };
    let mut signed = Vec::with_capacity(64 + params_len);
    signed.extend_from_slice(client_random);
    signed.extend_from_slice(server_random);
    signed.extend_from_slice(&body[..params_len]);
    if !key.verify(alg, &signed, sig) {
        return Err(TlsError::BadHandshakeSignature);
    }
    Ok((group, point.to_vec(), scheme))
}

fn parse_encrypted_extensions(body: &[u8], offered_alpn: &[&str]) -> Result<Option<String>, TlsError> {
    let mut c = Cur::new(body);
    let exts = extensions(c.vec16()?)?;
    c.done()?;
    let mut alpn = None;
    for (t, d) in exts {
        match t {
            EXT_SERVER_NAME => {
                if !d.is_empty() {
                    return Err(proto("a malformed server_name extension"));
                }
            }
            EXT_SUPPORTED_GROUPS => {} // the server's preferences: informational
            EXT_ALPN => alpn = Some(parse_alpn(d, offered_alpn)?),
            _ => return Err(proto("the server sent an extension we didn't offer")),
        }
    }
    Ok(alpn)
}

fn parse_certificate(body: &[u8]) -> Result<Vec<Vec<u8>>, TlsError> {
    let mut c = Cur::new(body);
    if !c.vec8()?.is_empty() {
        return Err(proto("unexpected certificate request context"));
    }
    let list = c.vec24()?;
    c.done()?;
    let mut l = Cur::new(list);
    let mut out = Vec::new();
    while !l.d.is_empty() {
        let cert = l.vec24()?;
        let _exts = l.vec16()?; // OCSP staples, SCTs: not used
        if cert.is_empty() {
            return Err(proto("an empty certificate"));
        }
        out.push(cert.to_vec());
        if out.len() > 10 {
            return Err(TlsError::Cert(CertError::NotAllowed("too many certificates")));
        }
    }
    if out.is_empty() {
        return Err(TlsError::Cert(CertError::NoCertificate));
    }
    Ok(out)
}

/// Checks CertificateVerify against the transcript hash; returns the scheme.
fn check_certificate_verify(body: &[u8], key: &PublicKey, th: &[u8]) -> Result<u16, TlsError> {
    let mut c = Cur::new(body);
    let scheme = c.u16()?;
    let sig = c.vec16()?;
    c.done()?;
    let alg = match (scheme, key) {
        (SIG_ECDSA_P256_SHA256, PublicKey::Ec(Curve::P256, _)) => SigAlg::Ecdsa(HashAlg::Sha256),
        (SIG_ECDSA_P384_SHA384, PublicKey::Ec(Curve::P384, _)) => SigAlg::Ecdsa(HashAlg::Sha384),
        (SIG_RSA_PSS_RSAE_SHA256, PublicKey::Rsa(_)) => SigAlg::RsaPss(HashAlg::Sha256),
        (SIG_RSA_PSS_RSAE_SHA384, PublicKey::Rsa(_)) => SigAlg::RsaPss(HashAlg::Sha384),
        (SIG_RSA_PSS_RSAE_SHA512, PublicKey::Rsa(_)) => SigAlg::RsaPss(HashAlg::Sha512),
        (SIG_ECDSA_P521_SHA512, _) => return Err(proto("the server used a signature we didn't offer")),
        _ => return Err(proto("the server used a signature we didn't offer, or one that doesn't fit its key")),
    };
    let mut content = vec![0x20u8; 64];
    content.extend_from_slice(b"TLS 1.3, server CertificateVerify");
    content.push(0);
    content.extend_from_slice(th);
    if !key.verify(alg, &content, sig) {
        return Err(TlsError::BadHandshakeSignature);
    }
    Ok(scheme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    // RFC 8448 section 3 (simple 1-RTT handshake), key schedule values.
    #[test]
    fn rfc8448_key_schedule() {
        let kdf = Kdf::Sha256;
        let zeros = [0u8; 32];
        let early = kdf.extract(&[], &zeros);
        assert_eq!(early, hex("33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a"));
        let derived = kdf.derive(&early, "derived", &kdf.hash(&[]));
        assert_eq!(derived, hex("6f2615a108c702c5678f54fc9dbab69716c076189c48250cebeac3576c3611ba"));
        let shared = hex("8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d");
        let hs = kdf.extract(&derived, &shared);
        assert_eq!(hs, hex("1dc826e93606aa6fdc0aadc12f741b01046aa6b99f691ed221a9f0ca043fbeac"));
        // The transcript hash of ClientHello..ServerHello from RFC 8448.
        let th = hex("860c06edc07858ee8e78f0e7428c58edd6b43f2ca3e6e95f02ed063cf0e1cad8");
        let c_hs = kdf.derive(&hs, "c hs traffic", &th);
        assert_eq!(c_hs, hex("b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21"));
        let s_hs = kdf.derive(&hs, "s hs traffic", &th);
        assert_eq!(s_hs, hex("b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38"));
        assert_eq!(kdf.expand_label(&s_hs, "key", &[], 16), hex("3fce516009c21727d0f2e4e86ee403bc"));
        assert_eq!(kdf.expand_label(&s_hs, "iv", &[], 12), hex("5d313eb2671276ee13000b30"));
        let d2 = kdf.derive(&hs, "derived", &kdf.hash(&[]));
        let master = kdf.extract(&d2, &zeros);
        assert_eq!(master, hex("18df06843d13a08bf2a449844c5f8a478001bc4d4c627984d5a41da8d0402919"));
    }

    // TLS 1.2 PRF values from `openssl kdf ... TLS1-PRF`.
    #[test]
    fn tls12_prf() {
        let out = Kdf::Sha256.prf12(&hex("9bbe436ba940f017b17652849a71db35"), "test label", &hex("a0ba9f936cda311827a6f796ffd5198c"), 64);
        assert_eq!(
            out,
            hex("e3f229ba727be17b8d122620557cd453c2aab21d07c3d495329b52d4e61edb5a6b301791e90d35c9c9a46b4e14baf9af0fa022f7077def17abfd3797c0564bab")
        );
        let out = Kdf::Sha384.prf12(&hex("b80b733d6ceefcdc71566ea48e5567df"), "test label", &hex("cd665cf6a8447dd6ff8b27555edb7465"), 48);
        assert_eq!(
            out,
            hex("7b0c18e9ced410ed1804f2cfa34a336a1c14dffb4900bb5fd7942107e81c83cde9ca0faa60be9fe34f82b1233c9146a0")
        );
    }

    #[test]
    fn tls12_records_round_trip() {
        for suite in [TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256, TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256] {
            let key = vec![5u8; key_len(suite)];
            let iv = vec![9u8; if is_chacha(suite) { 12 } else { 4 }];
            let mut w = Keys::new12(suite, &key, &iv);
            let mut r = Keys::new12(suite, &key, &iv);
            for msg in [&b"hello"[..], b"", &[7u8; 300]] {
                let body = w.seal12(CT_APPDATA, msg).unwrap();
                assert_eq!(body.len(), msg.len() + 16 + if is_chacha(suite) { 0 } else { 8 });
                assert_eq!(r.open12(CT_APPDATA, body).unwrap(), msg);
            }
            // A record replayed (wrong sequence number) or retyped fails.
            let body = w.seal12(CT_APPDATA, b"x").unwrap();
            assert!(r.open12(CT_HANDSHAKE, body.clone()).is_err());
            let mut r2 = Keys::new12(suite, &key, &iv);
            assert!(r2.open12(CT_APPDATA, body).is_err());
        }
    }

    #[test]
    fn server_hello_checks() {
        let roots = RootStore::empty();
        let cfg = TlsConfig { roots: &roots, now: 0, suites: DEFAULT_SUITES, alpn: &["http/1.1"] };
        let only13 = TlsConfig { suites: &DEFAULT_SUITES[..3], ..cfg.clone() };
        let sid = [7u8; 32];
        let mk = |version: u16, random: [u8; 32], sid: &[u8], suite: u16, exts: &[u8]| {
            let mut b = Vec::new();
            put_u16(&mut b, version);
            b.extend_from_slice(&random);
            b.push(sid.len() as u8);
            b.extend_from_slice(sid);
            put_u16(&mut b, suite);
            b.push(0);
            put_vec16(&mut b, exts);
            b
        };
        let mut good_exts = Vec::new();
        ext(&mut good_exts, EXT_SUPPORTED_VERSIONS, &[3, 4]);
        let mut ks = Vec::new();
        put_u16(&mut ks, GROUP_X25519);
        put_vec16(&mut ks, &[9u8; 32]);
        ext(&mut good_exts, EXT_KEY_SHARE, &ks);
        let ok = mk(0x0303, [1; 32], &sid, TLS_AES_128_GCM_SHA256, &good_exts);
        assert!(matches!(parse_server_hello(&ok, &sid, &cfg), Ok(ServerHello::Tls13 { group: GROUP_X25519, .. })));
        // A P-256 share must be 65 bytes.
        let mut p_exts = Vec::new();
        ext(&mut p_exts, EXT_SUPPORTED_VERSIONS, &[3, 4]);
        let mut ks = Vec::new();
        put_u16(&mut ks, GROUP_SECP256R1);
        put_vec16(&mut ks, &[4u8; 65]);
        ext(&mut p_exts, EXT_KEY_SHARE, &ks);
        assert!(parse_server_hello(&mk(0x0303, [1; 32], &sid, TLS_AES_128_GCM_SHA256, &p_exts), &sid, &cfg).is_ok());
        // TLS 1.2 server: accepted with a 1.2 suite, refused when only 1.3 was offered.
        let tls12 = mk(0x0303, [1; 32], &sid, 0xc02f, &[]);
        assert!(matches!(parse_server_hello(&tls12, &sid, &cfg), Ok(ServerHello::Tls12 { ems: false, .. })));
        assert!(parse_server_hello(&tls12, &sid, &only13).is_err());
        let mut e12 = Vec::new();
        ext(&mut e12, EXT_RENEGOTIATION_INFO, &[0]);
        ext(&mut e12, EXT_EXTENDED_MASTER_SECRET, &[]);
        ext(&mut e12, EXT_EC_POINT_FORMATS, &[1, 0]);
        assert!(matches!(
            parse_server_hello(&mk(0x0303, [1; 32], &sid, 0xc02f, &e12), &sid, &cfg),
            Ok(ServerHello::Tls12 { ems: true, .. })
        ));
        // A TLS 1.3 suite with TLS 1.2, a 1.2 suite with 1.3.
        assert!(parse_server_hello(&mk(0x0303, [1; 32], &sid, TLS_AES_128_GCM_SHA256, &[]), &sid, &cfg).is_err());
        assert!(parse_server_hello(&mk(0x0303, [1; 32], &sid, 0xc02f, &good_exts), &sid, &cfg).is_err());
        // The downgrade sentinels.
        let mut r = [1u8; 32];
        r[24..].copy_from_slice(&DOWNGRADE_TLS12);
        assert!(parse_server_hello(&mk(0x0303, r, &sid, 0xc02f, &[]), &sid, &cfg).is_err());
        r[24..].copy_from_slice(&DOWNGRADE_TLS11);
        assert!(parse_server_hello(&mk(0x0303, r, &sid, 0xc02f, &[]), &sid, &cfg).is_err());
        // TLS 1.1.
        assert!(matches!(parse_server_hello(&mk(0x0302, [1; 32], &sid, 0xc02f, &[]), &sid, &cfg), Err(TlsError::NoTls13)));
        // A non-empty renegotiation_info.
        let mut bad = Vec::new();
        ext(&mut bad, EXT_RENEGOTIATION_INFO, &[1, 0]);
        assert!(parse_server_hello(&mk(0x0303, [1; 32], &sid, 0xc02f, &bad), &sid, &cfg).is_err());
        // Wrong session id, suite not offered, HelloRetryRequest.
        assert!(parse_server_hello(&mk(0x0303, [1; 32], &[0; 32], TLS_AES_128_GCM_SHA256, &good_exts), &sid, &cfg).is_err());
        assert!(parse_server_hello(&mk(0x0303, [1; 32], &sid, 0x1304, &good_exts), &sid, &cfg).is_err());
        assert!(parse_server_hello(&mk(0x0303, HRR_RANDOM, &sid, TLS_AES_128_GCM_SHA256, &good_exts), &sid, &cfg).is_err());
        // An extension we didn't offer.
        let mut extra = good_exts.clone();
        ext(&mut extra, 0x1234, &[]);
        assert!(parse_server_hello(&mk(0x0303, [1; 32], &sid, TLS_AES_128_GCM_SHA256, &extra), &sid, &cfg).is_err());
        // Duplicate extension.
        let mut dup = good_exts.clone();
        ext(&mut dup, EXT_SUPPORTED_VERSIONS, &[3, 4]);
        assert!(parse_server_hello(&mk(0x0303, [1; 32], &sid, TLS_AES_128_GCM_SHA256, &dup), &sid, &cfg).is_err());
    }
}
