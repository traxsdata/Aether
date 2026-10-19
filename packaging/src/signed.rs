// SPDX-License-Identifier: Apache-2.0
//! Signed record framing and the publisher trust store.

/// A publisher key that the system trusts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustedPublisher<'a> {
    /// Display name that must match the manifest.
    pub name: &'a str,
    /// Ed25519 public key.
    pub key: [u8; 32],
}

/// Bounded list of trusted publisher keys.
#[derive(Clone, Copy)]
pub struct TrustStore<'a> {
    entries: &'a [TrustedPublisher<'a>],
}

impl<'a> TrustStore<'a> {
    /// Creates a store over a static list.
    #[must_use]
    pub const fn new(entries: &'a [TrustedPublisher<'a>]) -> Self {
        Self { entries }
    }

    /// Finds the publisher that owns `key`.
    #[must_use]
    pub fn find(&self, key: &[u8; 32]) -> Option<&'a TrustedPublisher<'a>> {
        self.entries.iter().find(|entry| entry.key == *key)
    }

    /// Number of trusted keys.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the store is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Framing errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The magic does not match the record type.
    BadMagic,
    /// The framing version is not supported.
    UnsupportedVersion,
    /// The record is shorter than its declared lengths.
    Truncated,
    /// Bytes remain after the declared signature.
    TrailingBytes,
    /// A declared length exceeds the record size.
    LengthMismatch,
}

/// A decoded signed record.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    /// Framing version.
    pub version: u16,
    /// Signed body.
    pub body: &'a [u8],
    /// Unsigned payload covered by the body's digest.
    pub payload: &'a [u8],
    /// Ed25519 signature over `body`.
    pub signature: [u8; 64],
}

/// Decodes a signed record.
pub fn decode_frame<'a>(bytes: &'a [u8], magic: &[u8; 8]) -> Result<Frame<'a>, FrameError> {
    // magic + version + body length + payload length + signature
    const HEADER: usize = 8 + 2 + 4 + 4 + 64;
    if bytes.len() < HEADER {
        return Err(FrameError::Truncated);
    }
    if &bytes[..8] != magic {
        return Err(FrameError::BadMagic);
    }
    let version = u16::from_le_bytes([bytes[8], bytes[9]]);
    let body_len = usize::try_from(u32::from_le_bytes([
        bytes[10], bytes[11], bytes[12], bytes[13],
    ]))
    .map_err(|_| FrameError::LengthMismatch)?;
    let payload_len = usize::try_from(u32::from_le_bytes([
        bytes[14], bytes[15], bytes[16], bytes[17],
    ]))
    .map_err(|_| FrameError::LengthMismatch)?;
    let expected = HEADER
        .checked_add(body_len)
        .and_then(|value| value.checked_add(payload_len))
        .ok_or(FrameError::LengthMismatch)?;
    match bytes.len().cmp(&expected) {
        core::cmp::Ordering::Less => return Err(FrameError::Truncated),
        core::cmp::Ordering::Greater => return Err(FrameError::TrailingBytes),
        core::cmp::Ordering::Equal => {}
    }
    let body_start = 18;
    let payload_start = body_start + body_len;
    let signature_start = payload_start + payload_len;
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&bytes[signature_start..signature_start + 64]);
    Ok(Frame {
        version,
        body: &bytes[body_start..payload_start],
        payload: &bytes[payload_start..signature_start],
        signature,
    })
}

/// Encodes a signed record for build tooling and tests.
///
/// Returns the number of bytes written.
#[must_use]
pub fn encode_frame(
    magic: &[u8; 8],
    version: u16,
    body: &[u8],
    payload: &[u8],
    signature: &[u8; 64],
    out: &mut [u8],
) -> Option<usize> {
    let total = 8usize
        .checked_add(2)?
        .checked_add(4)?
        .checked_add(4)?
        .checked_add(body.len())?
        .checked_add(payload.len())?
        .checked_add(64)?;
    let out = out.get_mut(..total)?;
    out[..8].copy_from_slice(magic);
    out[8..10].copy_from_slice(&version.to_le_bytes());
    out[10..14].copy_from_slice(&u32::try_from(body.len()).ok()?.to_le_bytes());
    out[14..18].copy_from_slice(&u32::try_from(payload.len()).ok()?.to_le_bytes());
    out[18..18 + body.len()].copy_from_slice(body);
    let payload_start = 18 + body.len();
    out[payload_start..payload_start + payload.len()].copy_from_slice(payload);
    let signature_start = payload_start + payload.len();
    out[signature_start..signature_start + 64].copy_from_slice(signature);
    Some(total)
}
