// SPDX-License-Identifier: Apache-2.0
//! Bounded payload file archive.

use crate::{MAX_FILE_BYTES, MAX_FILE_NAME, MAX_FILES};

/// One file in a payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PayloadFile<'a> {
    /// File name, relative to the application root.
    pub name: &'a str,
    /// File contents.
    pub data: &'a [u8],
}

/// Payload parsing errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadError {
    /// The archive ends before its declared lengths.
    Truncated,
    /// More files than [`MAX_FILES`].
    TooManyFiles,
    /// A file name is unsafe or malformed.
    InvalidName,
    /// Two files share a name.
    DuplicateName,
    /// A file is larger than [`MAX_FILE_BYTES`].
    FileTooLarge,
    /// Bytes remain after the declared file count.
    TrailingBytes,
}

/// Returns `true` for a safe, bounded payload file name.
#[must_use]
pub fn is_valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME
        && name != "."
        && name != ".."
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || byte == b'.'
                || byte == b'-'
                || byte == b'_'
        })
}

/// A parsed payload archive.
#[derive(Debug)]
pub struct Payload<'a> {
    files: [Option<PayloadFile<'a>>; MAX_FILES],
    len: usize,
}

impl<'a> Payload<'a> {
    /// Parses a payload archive.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, PayloadError> {
        let mut reader = Reader::new(bytes);
        let count = usize::from(reader.u16()?);
        if count > MAX_FILES {
            return Err(PayloadError::TooManyFiles);
        }
        let mut payload = Self {
            files: [None; MAX_FILES],
            len: 0,
        };
        for _ in 0..count {
            let name_length = usize::from(reader.u16()?);
            if name_length == 0 || name_length > MAX_FILE_NAME {
                return Err(PayloadError::InvalidName);
            }
            let name_bytes = reader.bytes(name_length)?;
            let name = core::str::from_utf8(name_bytes).map_err(|_| PayloadError::InvalidName)?;
            if !is_valid_file_name(name) {
                return Err(PayloadError::InvalidName);
            }
            if payload.get(name).is_some() {
                return Err(PayloadError::DuplicateName);
            }
            let data_length =
                usize::try_from(reader.u32()?).map_err(|_| PayloadError::FileTooLarge)?;
            if data_length > MAX_FILE_BYTES {
                return Err(PayloadError::FileTooLarge);
            }
            let data = reader.bytes(data_length)?;
            let slot = payload
                .files
                .iter_mut()
                .find(|slot| slot.is_none())
                .ok_or(PayloadError::TooManyFiles)?;
            *slot = Some(PayloadFile { name, data });
            payload.len += 1;
        }
        if !reader.is_empty() {
            return Err(PayloadError::TrailingBytes);
        }
        Ok(payload)
    }

    /// Returns a file by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PayloadFile<'a>> {
        self.files.iter().flatten().find(|file| file.name == name)
    }

    /// Iterates over files.
    pub fn files(&self) -> impl Iterator<Item = &PayloadFile<'a>> {
        self.files.iter().flatten()
    }

    /// Number of files.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the payload has no files.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Encodes a payload archive for build tooling and tests.
///
/// Returns the number of bytes written, or `None` for an invalid list or a
/// destination that is too small.
#[must_use]
pub fn encode_payload(files: &[(&str, &[u8])], out: &mut [u8]) -> Option<usize> {
    if files.len() > MAX_FILES {
        return None;
    }
    let mut written = 0usize;
    put_u16(out, &mut written, files.len() as u16)?;
    for (name, data) in files {
        if !is_valid_file_name(name) || data.len() > MAX_FILE_BYTES {
            return None;
        }
        // Reject duplicates exactly as the parser does.
        if files.iter().filter(|(other, _)| other == name).count() != 1 {
            return None;
        }
        put_u16(out, &mut written, name.len() as u16)?;
        put_bytes(out, &mut written, name.as_bytes())?;
        put_u32(out, &mut written, data.len() as u32)?;
        put_bytes(out, &mut written, data)?;
    }
    Some(written)
}

fn put_bytes(out: &mut [u8], written: &mut usize, bytes: &[u8]) -> Option<()> {
    let end = written.checked_add(bytes.len())?;
    out.get_mut(*written..end)?.copy_from_slice(bytes);
    *written = end;
    Some(())
}

fn put_u16(out: &mut [u8], written: &mut usize, value: u16) -> Option<()> {
    put_bytes(out, written, &value.to_le_bytes())
}

fn put_u32(out: &mut [u8], written: &mut usize, value: u32) -> Option<()> {
    put_bytes(out, written, &value.to_le_bytes())
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], PayloadError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(PayloadError::Truncated)?;
        let slice = self
            .data
            .get(self.offset..end)
            .ok_or(PayloadError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, PayloadError> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, PayloadError> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn is_empty(&self) -> bool {
        self.offset == self.data.len()
    }
}
