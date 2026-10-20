// SPDX-License-Identifier: Apache-2.0
//! Bounded inspection of AppImage type 2 containers.
//!
//! An AppImage is a runtime ELF with a SquashFS filesystem appended. The
//! runtime normally mounts that filesystem through FUSE; Aether's Linux app
//! runtime will instead import the image into a private read-only root. Before
//! any of that can happen the container must be validated: the import path
//! reads an untrusted file, and a forged header must not be able to point the
//! runtime outside the image or select an unsupported decompressor.
//!
//! This crate only inspects the container. It does not execute the runtime,
//! mount the filesystem, decompress a block, or load an application. Those are
//! later stages of the runtime described in
//! `docs/specifications/linux-compatibility.md`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// Size of an ELF64 header.
pub const ELF_HEADER_BYTES: usize = 64;
/// Size of a SquashFS 4 superblock.
pub const SQUASHFS_SUPERBLOCK_BYTES: usize = 96;

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const APPIMAGE_MAGIC: [u8; 3] = [b'A', b'I', 0x02];
const SQUASHFS_MAGIC: [u8; 4] = *b"hsqs";
const SQUASHFS_MAJOR: u16 = 4;
const SQUASHFS_MINOR: u16 = 0;
const NO_TABLE: u64 = u64::MAX;

/// AppImage container inspection failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The file is smaller than an ELF header or its declared structures.
    Truncated,
    /// The file is not an ELF image.
    NotElf,
    /// The ELF is not a 64-bit little-endian x86-64 executable.
    UnsupportedElf,
    /// The ELF is valid but is not AppImage type 2.
    NotAppImage,
    /// No structurally valid SquashFS 4 superblock was found.
    NoSquashfs,
    /// The SquashFS superblock is malformed or inconsistent.
    InvalidSquashfs,
    /// The SquashFS compression method is not recognised.
    UnsupportedCompression,
    /// The container digest does not match the pinned expected value.
    DigestMismatch,
}

/// A SHA-256 digest pinned by the runtime profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Computes the digest of an untrusted container.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(aether_crypto::sha256(bytes))
    }

    /// Parses a 64-character hexadecimal digest.
    #[must_use]
    pub fn from_hex(text: &[u8]) -> Option<Self> {
        let mut digest = [0u8; 32];
        let length = aether_crypto::from_hex(text, &mut digest)?;
        (length == 32).then_some(Self(digest))
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compares this pinned digest with the bytes.
    #[must_use]
    pub fn matches(&self, bytes: &[u8]) -> bool {
        aether_crypto::sha256(bytes) == self.0
    }
}

/// Verifies a pinned digest before parsing the container.
///
/// The digest check is not a substitute for signature verification: it pins
/// one exact artifact. The publisher signature and trust store remain the
/// runtime manager's decision.
pub fn parse_verified(bytes: &[u8], expected: Digest) -> Result<Info, Error> {
    if !expected.matches(bytes) {
        return Err(Error::DigestMismatch);
    }
    parse(bytes)
}

/// SquashFS compression method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// gzip/deflate.
    Gzip,
    /// LZMA.
    Lzma,
    /// LZO.
    Lzo,
    /// XZ.
    Xz,
    /// LZ4.
    Lz4,
    /// Zstandard.
    Zstd,
}

impl Compression {
    /// Human-readable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Lzma => "lzma",
            Self::Lzo => "lzo",
            Self::Xz => "xz",
            Self::Lz4 => "lz4",
            Self::Zstd => "zstd",
        }
    }

    fn from_raw(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::Gzip),
            2 => Some(Self::Lzma),
            3 => Some(Self::Lzo),
            4 => Some(Self::Xz),
            5 => Some(Self::Lz4),
            6 => Some(Self::Zstd),
            _ => None,
        }
    }
}

/// The ELF program type of the AppImage runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeElf {
    /// `ET_EXEC`, a fixed-address executable.
    Executable,
    /// `ET_DYN`, a position-independent runtime.
    PositionIndependent,
}

/// Validated metadata needed by the import stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    /// Offset of the SquashFS superblock in the AppImage.
    pub squashfs_offset: usize,
    /// Number of filesystem bytes starting at [`Self::squashfs_offset`].
    pub squashfs_bytes: u64,
    /// SquashFS data block size.
    pub block_size: u32,
    /// SquashFS compression method.
    pub compression: Compression,
    /// Number of filesystem inodes.
    pub inodes: u32,
    /// Number of fragments.
    pub fragments: u32,
    /// SquashFS flags word.
    pub flags: u16,
    /// Runtime ELF type.
    pub runtime: RuntimeElf,
}

/// Parses and validates an AppImage type 2 container.
///
/// The scan is bounded by `bytes` and only accepts a SquashFS 4 superblock
/// whose size, block geometry, compression id, and table offsets are
/// internally consistent. A magic byte sequence in the runtime's code is not
/// enough.
pub fn parse(bytes: &[u8]) -> Result<Info, Error> {
    if bytes.len() < ELF_HEADER_BYTES {
        return Err(Error::Truncated);
    }
    if bytes[..4] != ELF_MAGIC {
        return Err(Error::NotElf);
    }
    if bytes[4] != 2 || bytes[5] != 1 || bytes[6] != 1 {
        return Err(Error::UnsupportedElf);
    }
    if read_u16(bytes, 18) != Some(62) {
        return Err(Error::UnsupportedElf);
    }
    let runtime = match read_u16(bytes, 16) {
        Some(2) => RuntimeElf::Executable,
        Some(3) => RuntimeElf::PositionIndependent,
        _ => return Err(Error::UnsupportedElf),
    };
    if bytes.get(8..11) != Some(APPIMAGE_MAGIC.as_slice()) {
        return Err(Error::NotAppImage);
    }

    let mut offset = ELF_HEADER_BYTES;
    let mut unsupported_compression = false;
    while offset + SQUASHFS_SUPERBLOCK_BYTES <= bytes.len() {
        if bytes[offset..offset + 4] == SQUASHFS_MAGIC {
            match parse_superblock(bytes, offset) {
                Ok(mut info) => {
                    info.runtime = runtime;
                    return Ok(info);
                }
                Err(Error::UnsupportedCompression) => unsupported_compression = true,
                Err(_) => {}
            }
        }
        offset += 1;
    }
    if unsupported_compression {
        Err(Error::UnsupportedCompression)
    } else {
        Err(Error::NoSquashfs)
    }
}

fn parse_superblock(bytes: &[u8], offset: usize) -> Result<Info, Error> {
    let block = bytes
        .get(offset..offset + SQUASHFS_SUPERBLOCK_BYTES)
        .ok_or(Error::Truncated)?;
    let inodes = read_u32(block, 4).ok_or(Error::InvalidSquashfs)?;
    let block_size = read_u32(block, 12).ok_or(Error::InvalidSquashfs)?;
    let fragments = read_u32(block, 16).ok_or(Error::InvalidSquashfs)?;
    let compression_raw = read_u16(block, 20).ok_or(Error::InvalidSquashfs)?;
    let block_log = read_u16(block, 22).ok_or(Error::InvalidSquashfs)?;
    let flags = read_u16(block, 24).ok_or(Error::InvalidSquashfs)?;
    let major = read_u16(block, 28).ok_or(Error::InvalidSquashfs)?;
    let minor = read_u16(block, 30).ok_or(Error::InvalidSquashfs)?;
    let bytes_used = read_u64(block, 40).ok_or(Error::InvalidSquashfs)?;

    if major != SQUASHFS_MAJOR || minor != SQUASHFS_MINOR {
        return Err(Error::InvalidSquashfs);
    }
    if inodes == 0
        || !(4096..=1_048_576).contains(&block_size)
        || !block_size.is_power_of_two()
        || u32::from(block_log) != block_size.trailing_zeros()
    {
        return Err(Error::InvalidSquashfs);
    }
    let compression =
        Compression::from_raw(compression_raw).ok_or(Error::UnsupportedCompression)?;
    if bytes_used < SQUASHFS_SUPERBLOCK_BYTES as u64 || bytes_used > (bytes.len() - offset) as u64 {
        return Err(Error::InvalidSquashfs);
    }

    for position in [48usize, 56, 64, 72, 80, 88] {
        let table = read_u64(block, position).ok_or(Error::InvalidSquashfs)?;
        if table != NO_TABLE && (table < SQUASHFS_SUPERBLOCK_BYTES as u64 || table >= bytes_used) {
            return Err(Error::InvalidSquashfs);
        }
    }

    Ok(Info {
        squashfs_offset: offset,
        squashfs_bytes: bytes_used,
        block_size,
        compression,
        inodes,
        fragments,
        flags,
        runtime: RuntimeElf::Executable,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let value = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let value = bytes.get(offset..offset + 8)?;
    Some(u64::from_le_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

pub mod manifest;

#[cfg(test)]
mod tests;
