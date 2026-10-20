// SPDX-License-Identifier: Apache-2.0
//! Bounded SquashFS 4 reader for the Linux AppImage import.
//!
//! The reader validates every offset and length before reading, decompresses
//! only the metadata and data blocks it needs, and rejects unsupported inode
//! types instead of guessing. It supports zstd (the GridCraft AppImage), gzip,
//! lz4, lzo, and xz/lzma (host `std` feature). No_std xz/lzma decoders remain
//! tracked in the roadmap.
//!
//! The API is deliberately small: parse a raw SquashFS image or an AppImage,
//! walk the tree, read regular files, and extract into memory with limits.
//! Filesystem mounting, writable overlays, and signature policy are separate
//! layers.

#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use ruzstd::io::Read;

/// Maximum decompressed metadata block size defined by SquashFS 4.
pub const METADATA_BLOCK_BYTES: usize = 8192;
/// Maximum path length accepted during extraction.
pub const MAX_PATH_BYTES: usize = 4096;
/// Default maximum bytes extracted from one image.
pub const DEFAULT_MAX_BYTES: usize = 512 * 1024 * 1024;
/// Default maximum entries extracted from one image.
pub const DEFAULT_MAX_ENTRIES: usize = 65_536;
/// Default maximum directory depth.
pub const DEFAULT_MAX_DEPTH: usize = 64;

/// SquashFS reader failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A field, table, block, or file extends past the image.
    Truncated,
    /// The SquashFS magic or version is not 4.0.
    BadSuperblock,
    /// The AppImage container itself is invalid.
    Container(aether_appimage::Error),
    /// The compression method is not supported yet.
    UnsupportedCompression,
    /// The inode type is not supported by the extractor.
    UnsupportedInode(u16),
    /// A decompressor rejected or overflowed a block.
    Decompress,
    /// A directory or inode field is inconsistent.
    InvalidInode,
    /// A name is empty, oversized, or contains `/`, NUL, `.` or `..`.
    InvalidName,
    /// A symlink target is absolute or escapes the root.
    UnsafeSymlink,
    /// Two entries resolve to the same path.
    DuplicatePath,
    /// A configured extraction limit was exceeded.
    Limit,
}

/// One inode's relevant data.
#[derive(Clone, Debug)]
pub enum Kind {
    /// Directory with a metadata location and byte length.
    Directory {
        /// Offset from the directory table start.
        start_block: u32,
        /// Directory data length, including the three-byte size bias.
        file_size: u32,
        /// Offset within the first metadata block.
        offset: u16,
    },
    /// Regular file.
    File {
        /// Absolute offset of the first data block.
        blocks_start: u64,
        /// File size in bytes.
        file_size: u64,
        /// Fragment index, or `u32::MAX` when the tail is not fragmented.
        fragment: u32,
        /// Offset inside the fragment block.
        fragment_offset: u32,
        /// On-disk block sizes, including the uncompressed bit.
        block_sizes: Vec<u32>,
    },
    /// Symbolic link target bytes.
    Symlink(Vec<u8>),
}

/// Parsed inode.
#[derive(Clone, Debug)]
pub struct Inode {
    /// File type and location.
    pub kind: Kind,
    /// Unix mode bits.
    pub mode: u16,
    /// Inode number.
    pub number: u32,
    /// Extended-attribute index, or `u32::MAX` when absent.
    pub xattr: u32,
}

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Entry name, without a slash.
    pub name: String,
    /// Inode reference: `(metadata_block << 16) | offset`.
    pub inode: u64,
    /// SquashFS inode type.
    pub kind: u16,
}

/// One extracted regular file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedFile {
    /// Path below the root, without a leading slash.
    pub path: String,
    /// File contents.
    pub data: Vec<u8>,
    /// Unix mode bits.
    pub mode: u16,
}

/// One entry in an imported tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeKind {
    /// Directory.
    Directory,
    /// Regular file contents.
    File(Vec<u8>),
    /// Symlink target.
    Symlink(Vec<u8>),
}

/// One validated entry from an AppImage/SquashFS root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// Relative path below the root, without a leading slash.
    pub path: String,
    /// Entry type and payload.
    pub kind: TreeKind,
    /// Permission bits with setuid/setgid/sticky removed.
    pub mode: u16,
    /// Inode number; repeated numbers are hardlinks.
    pub inode: u32,
    /// Whether this entry shares its inode with an earlier path.
    pub hardlink: bool,
}

/// Result of importing a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedTree {
    /// Validated entries in traversal order.
    pub entries: Vec<TreeEntry>,
    /// Number of regular files.
    pub files: usize,
    /// Number of directories below the root.
    pub directories: usize,
    /// Number of symlinks.
    pub symlinks: usize,
    /// Number of repeated inode numbers.
    pub hardlinks: usize,
    /// Number of inodes that carried xattr metadata (metadata is dropped).
    pub xattrs: usize,
}

/// Extraction limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum total file bytes.
    pub max_bytes: usize,
    /// Maximum number of entries.
    pub max_entries: usize,
    /// Maximum directory depth.
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

/// Parsed SquashFS image backed by the original bytes.
pub struct Image<'a> {
    bytes: &'a [u8],
    offset: usize,
    inode_table: usize,
    directory_table: usize,
    fragment_table: usize,
    block_size: usize,
    compression: u16,
    fragments: u32,
    root_inode: u64,
}

impl<'a> Image<'a> {
    /// Parses a raw SquashFS 4 image.
    pub fn from_squashfs(bytes: &'a [u8]) -> Result<Self, Error> {
        Self::parse_at(bytes, 0)
    }

    /// Parses the SquashFS filesystem inside an AppImage type 2 container.
    pub fn from_appimage(bytes: &'a [u8]) -> Result<Self, Error> {
        let info = aether_appimage::parse(bytes).map_err(Error::Container)?;
        Self::parse_at(bytes, info.squashfs_offset)
    }

    fn parse_at(bytes: &'a [u8], offset: usize) -> Result<Self, Error> {
        let superblock = bytes.get(offset..offset + 96).ok_or(Error::Truncated)?;
        if superblock[..4] != *b"hsqs" {
            return Err(Error::BadSuperblock);
        }
        let inodes = read_u32(superblock, 4).ok_or(Error::BadSuperblock)?;
        let block_size = read_u32(superblock, 12).ok_or(Error::BadSuperblock)?;
        let fragments = read_u32(superblock, 16).ok_or(Error::BadSuperblock)?;
        let compression = read_u16(superblock, 20).ok_or(Error::BadSuperblock)?;
        let major = read_u16(superblock, 28).ok_or(Error::BadSuperblock)?;
        let minor = read_u16(superblock, 30).ok_or(Error::BadSuperblock)?;
        let root_inode = read_u64(superblock, 32).ok_or(Error::BadSuperblock)?;
        let bytes_used = read_u64(superblock, 40).ok_or(Error::BadSuperblock)?;
        let inode_table = read_u64(superblock, 64).ok_or(Error::BadSuperblock)?;
        let directory_table = read_u64(superblock, 72).ok_or(Error::BadSuperblock)?;
        let fragment_table = read_u64(superblock, 80).ok_or(Error::BadSuperblock)?;
        if major != 4
            || minor != 0
            || inodes == 0
            || !(4096..=1_048_576).contains(&block_size)
            || !block_size.is_power_of_two()
            || bytes_used > (bytes.len() - offset) as u64
        {
            return Err(Error::BadSuperblock);
        }
        let supported = matches!(compression, 1 | 3 | 5 | 6)
            || (cfg!(feature = "std") && matches!(compression, 2 | 4));
        if !supported {
            return Err(Error::UnsupportedCompression);
        }
        let base = offset as u64;
        let image_end = base + bytes_used;
        let image = Self {
            bytes,
            offset,
            inode_table: usize::try_from(base + inode_table).map_err(|_| Error::Truncated)?,
            directory_table: usize::try_from(base + directory_table)
                .map_err(|_| Error::Truncated)?,
            fragment_table: usize::try_from(base + fragment_table).map_err(|_| Error::Truncated)?,
            block_size: block_size as usize,
            compression,
            fragments,
            root_inode,
        };
        if image.inode_table as u64 >= image_end
            || image.directory_table as u64 >= image_end
            || (fragments != 0 && image.fragment_table as u64 >= image_end)
        {
            return Err(Error::BadSuperblock);
        }
        Ok(image)
    }

    /// Returns the root inode.
    pub fn root(&self) -> Result<Inode, Error> {
        self.inode(self.root_inode)
    }

    /// Resolves a relative slash-separated path from the root.
    pub fn lookup(&self, path: &str) -> Result<Inode, Error> {
        let mut inode = self.root()?;
        if path.is_empty() || path == "/" {
            return Ok(inode);
        }
        for component in path.trim_start_matches('/').split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(Error::InvalidName);
            }
            let entry = self
                .list(&inode)?
                .into_iter()
                .find(|entry| entry.name == component)
                .ok_or(Error::InvalidInode)?;
            inode = self.inode(entry.inode)?;
        }
        Ok(inode)
    }

    /// Lists a directory inode.
    pub fn list(&self, inode: &Inode) -> Result<Vec<Entry>, Error> {
        let Kind::Directory {
            start_block,
            file_size,
            offset,
        } = inode.kind
        else {
            return Err(Error::InvalidInode);
        };
        if file_size < 3 {
            return Err(Error::InvalidInode);
        }
        let length = (file_size - 3) as usize;
        if length == 0 {
            return Ok(Vec::new());
        }
        let data = self.metadata_stream(
            self.directory_table,
            start_block as usize,
            offset as usize,
            length,
        )?;
        parse_directory(&data)
    }

    /// Reads a regular file inode into memory.
    pub fn read_file(&self, inode: &Inode) -> Result<Vec<u8>, Error> {
        let Kind::File {
            blocks_start,
            file_size,
            fragment,
            fragment_offset,
            ref block_sizes,
        } = inode.kind
        else {
            return Err(Error::InvalidInode);
        };
        let size = usize::try_from(file_size).map_err(|_| Error::Limit)?;
        if size > DEFAULT_MAX_BYTES {
            return Err(Error::Limit);
        }
        let mut out = Vec::with_capacity(size);
        let mut cursor = self
            .offset
            .checked_add(usize::try_from(blocks_start).map_err(|_| Error::Truncated)?)
            .ok_or(Error::Truncated)?;
        let chunks = if fragment == u32::MAX {
            size.div_ceil(self.block_size)
        } else {
            size / self.block_size
        };
        if block_sizes.len() != chunks {
            return Err(Error::InvalidInode);
        }
        for (index, encoded) in block_sizes.iter().enumerate() {
            let expected = (size - index * self.block_size).min(self.block_size);
            let block = self.data_block(cursor, *encoded, expected)?;
            out.extend_from_slice(&block);
            cursor = cursor
                .checked_add((*encoded & 0x00ff_ffff) as usize)
                .ok_or(Error::Truncated)?;
        }
        if fragment != u32::MAX && out.len() < size {
            let tail = size - out.len();
            let block = self.fragment_block(fragment as usize, tail, fragment_offset as usize)?;
            out.extend_from_slice(&block);
        }
        if out.len() != size {
            return Err(Error::InvalidInode);
        }
        Ok(out)
    }

    /// Walks the tree and returns every regular file with its relative path.
    pub fn extract(&self, limits: Limits) -> Result<Vec<ExtractedFile>, Error> {
        Ok(self
            .import(limits)?
            .entries
            .into_iter()
            .filter_map(|entry| match entry.kind {
                TreeKind::File(data) => Some(ExtractedFile {
                    path: entry.path,
                    data,
                    mode: entry.mode,
                }),
                TreeKind::Directory | TreeKind::Symlink(_) => None,
            })
            .collect())
    }

    /// Imports the complete tree with path, symlink, hardlink, and xattr policy.
    ///
    /// Symlinks must be relative and stay below the root. Duplicate paths are
    /// rejected. Repeated inode numbers are marked as hardlinks and their data
    /// is copied per path, so a host extraction never creates a link that could
    /// cross the root. Setuid, setgid, and sticky bits are stripped. Xattr
    /// metadata is counted and dropped; file capabilities never reach the
    /// runtime root.
    pub fn import(&self, limits: Limits) -> Result<ImportedTree, Error> {
        let root = self.root()?;
        let mut state = WalkState::default();
        self.walk(&root, "", 0, limits, &mut state)?;
        Ok(ImportedTree {
            entries: state.tree,
            files: state.files,
            directories: state.directories,
            symlinks: state.symlinks,
            hardlinks: state.hardlinks,
            xattrs: state.xattrs,
        })
    }

    fn walk(
        &self,
        inode: &Inode,
        prefix: &str,
        depth: usize,
        limits: Limits,
        state: &mut WalkState,
    ) -> Result<(), Error> {
        if depth > limits.max_depth {
            return Err(Error::Limit);
        }
        match &inode.kind {
            Kind::Directory { .. } => {
                for entry in self.list(inode)? {
                    state.entries += 1;
                    if state.entries > limits.max_entries {
                        return Err(Error::Limit);
                    }
                    if entry.name == "." || entry.name == ".." || entry.name.contains('/') {
                        return Err(Error::InvalidName);
                    }
                    let child = self.inode(entry.inode)?;
                    let path = if prefix.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{prefix}/{}", entry.name)
                    };
                    if path.len() > MAX_PATH_BYTES {
                        return Err(Error::Limit);
                    }
                    if state.paths.insert(path.clone(), ()).is_some() {
                        return Err(Error::DuplicatePath);
                    }
                    if child.xattr != u32::MAX {
                        state.xattrs += 1;
                    }
                    match child.kind {
                        Kind::Directory { .. } => {
                            state.directories += 1;
                            state.tree.push(TreeEntry {
                                path: path.clone(),
                                kind: TreeKind::Directory,
                                mode: child.mode & 0o777,
                                inode: child.number,
                                hardlink: false,
                            });
                            self.walk(&child, &path, depth + 1, limits, state)?;
                        }
                        Kind::File { .. } => {
                            let data = self.read_file(&child)?;
                            state.bytes =
                                state.bytes.checked_add(data.len()).ok_or(Error::Limit)?;
                            if state.bytes > limits.max_bytes {
                                return Err(Error::Limit);
                            }
                            let hardlink = state.inodes.insert(child.number, ()).is_some();
                            if hardlink {
                                state.hardlinks += 1;
                            }
                            state.files += 1;
                            state.tree.push(TreeEntry {
                                path,
                                kind: TreeKind::File(data),
                                mode: child.mode & 0o777,
                                inode: child.number,
                                hardlink,
                            });
                        }
                        Kind::Symlink(target) => {
                            if !safe_symlink(&target) {
                                return Err(Error::UnsafeSymlink);
                            }
                            state.symlinks += 1;
                            state.tree.push(TreeEntry {
                                path,
                                kind: TreeKind::Symlink(target),
                                mode: child.mode & 0o777,
                                inode: child.number,
                                hardlink: false,
                            });
                        }
                    }
                }
            }
            Kind::File { .. } => {
                let data = self.read_file(inode)?;
                state.bytes = state.bytes.checked_add(data.len()).ok_or(Error::Limit)?;
                if state.bytes > limits.max_bytes {
                    return Err(Error::Limit);
                }
                state.files += 1;
                state.tree.push(TreeEntry {
                    path: prefix.to_string(),
                    kind: TreeKind::File(data),
                    mode: inode.mode & 0o777,
                    inode: inode.number,
                    hardlink: false,
                });
            }
            Kind::Symlink(target) => {
                if !safe_symlink(target) {
                    return Err(Error::UnsafeSymlink);
                }
                state.symlinks += 1;
                state.tree.push(TreeEntry {
                    path: prefix.to_string(),
                    kind: TreeKind::Symlink(target.clone()),
                    mode: inode.mode & 0o777,
                    inode: inode.number,
                    hardlink: false,
                });
            }
        }
        Ok(())
    }

    fn inode(&self, reference: u64) -> Result<Inode, Error> {
        let block = (reference >> 16) as usize;
        let offset = (reference & 0xffff) as usize;
        let (data, _) = self.metadata_block(self.inode_table, block)?;
        let start = data.get(offset..).ok_or(Error::InvalidInode)?;
        if start.len() < 16 {
            return Err(Error::InvalidInode);
        }
        let kind = read_u16(start, 0).ok_or(Error::InvalidInode)?;
        let mode = read_u16(start, 2).ok_or(Error::InvalidInode)?;
        let number = read_u32(start, 12).ok_or(Error::InvalidInode)?;
        let mut xattr = u32::MAX;
        let inode = match kind {
            1 => {
                let start_block = read_u32(start, 16).ok_or(Error::InvalidInode)?;
                let file_size = u32::from(read_u16(start, 24).ok_or(Error::InvalidInode)?);
                let offset = read_u16(start, 26).ok_or(Error::InvalidInode)?;
                Kind::Directory {
                    start_block,
                    file_size,
                    offset,
                }
            }
            8 => {
                let file_size = read_u32(start, 20).ok_or(Error::InvalidInode)?;
                let start_block = read_u32(start, 24).ok_or(Error::InvalidInode)?;
                let offset = read_u16(start, 34).ok_or(Error::InvalidInode)?;
                xattr = read_u32(start, 36).ok_or(Error::InvalidInode)?;
                Kind::Directory {
                    start_block,
                    file_size,
                    offset,
                }
            }
            2 => {
                let blocks_start = u64::from(read_u32(start, 16).ok_or(Error::InvalidInode)?);
                let fragment = read_u32(start, 20).ok_or(Error::InvalidInode)?;
                let fragment_offset = read_u32(start, 24).ok_or(Error::InvalidInode)?;
                let file_size = u64::from(read_u32(start, 28).ok_or(Error::InvalidInode)?);
                let block_sizes =
                    parse_block_sizes(start, 32, file_size, fragment, self.block_size)?;
                Kind::File {
                    blocks_start,
                    file_size,
                    fragment,
                    fragment_offset,
                    block_sizes,
                }
            }
            9 => {
                let blocks_start = read_u64(start, 16).ok_or(Error::InvalidInode)?;
                let file_size = read_u64(start, 24).ok_or(Error::InvalidInode)?;
                let fragment = read_u32(start, 44).ok_or(Error::InvalidInode)?;
                let fragment_offset = read_u32(start, 48).ok_or(Error::InvalidInode)?;
                let block_sizes =
                    parse_block_sizes(start, 56, file_size, fragment, self.block_size)?;
                Kind::File {
                    blocks_start,
                    file_size,
                    fragment,
                    fragment_offset,
                    block_sizes,
                }
            }
            3 | 10 => {
                let length = read_u32(start, 20).ok_or(Error::InvalidInode)? as usize;
                let target = start
                    .get(24..24usize.checked_add(length).ok_or(Error::Truncated)?)
                    .ok_or(Error::InvalidInode)?;
                let target = target.strip_suffix(&[0]).unwrap_or(target);
                if kind == 10 {
                    let end = 24usize.checked_add(length).ok_or(Error::Truncated)?;
                    xattr = read_u32(start, end).ok_or(Error::InvalidInode)?;
                }
                Kind::Symlink(target.to_vec())
            }
            other => return Err(Error::UnsupportedInode(other)),
        };
        Ok(Inode {
            kind: inode,
            mode,
            number,
            xattr,
        })
    }

    fn metadata_block(&self, table: usize, block: usize) -> Result<(Vec<u8>, usize), Error> {
        let start = table.checked_add(block).ok_or(Error::Truncated)?;
        let header = read_u16(self.bytes, start).ok_or(Error::Truncated)?;
        let size = usize::from(header & 0x7fff);
        let on_disk = 2usize.checked_add(size).ok_or(Error::Truncated)?;
        let payload = self
            .bytes
            .get(start + 2..start + on_disk)
            .ok_or(Error::Truncated)?;
        let data = if header & 0x8000 != 0 {
            if size > METADATA_BLOCK_BYTES {
                return Err(Error::InvalidInode);
            }
            payload.to_vec()
        } else {
            self.decompress(payload, METADATA_BLOCK_BYTES)?
        };
        Ok((data, on_disk))
    }

    fn metadata_stream(
        &self,
        table: usize,
        mut block: usize,
        mut offset: usize,
        length: usize,
    ) -> Result<Vec<u8>, Error> {
        let mut out = Vec::with_capacity(length);
        while out.len() < length {
            let (data, on_disk) = self.metadata_block(table, block)?;
            if offset > data.len() {
                return Err(Error::InvalidInode);
            }
            let take = (length - out.len()).min(data.len() - offset);
            out.extend_from_slice(&data[offset..offset + take]);
            offset = 0;
            if out.len() < length {
                block = block.checked_add(on_disk).ok_or(Error::Truncated)?;
            }
        }
        Ok(out)
    }

    fn data_block(&self, cursor: usize, encoded: u32, expected: usize) -> Result<Vec<u8>, Error> {
        let data = self.data_block_any(cursor, encoded, expected)?;
        if data.len() != expected {
            return Err(Error::InvalidInode);
        }
        Ok(data)
    }

    fn data_block_any(
        &self,
        cursor: usize,
        encoded: u32,
        maximum: usize,
    ) -> Result<Vec<u8>, Error> {
        if encoded == 0 {
            return Ok(vec![0; maximum]);
        }
        let size = (encoded & 0x00ff_ffff) as usize;
        if size == 0 || size > maximum {
            return Err(Error::InvalidInode);
        }
        let payload = self
            .bytes
            .get(cursor..cursor.checked_add(size).ok_or(Error::Truncated)?)
            .ok_or(Error::Truncated)?;
        if encoded & 0x0100_0000 != 0 {
            Ok(payload.to_vec())
        } else {
            self.decompress(payload, maximum)
        }
    }

    fn fragment_block(
        &self,
        fragment: usize,
        tail: usize,
        offset: usize,
    ) -> Result<Vec<u8>, Error> {
        if fragment >= self.fragments as usize {
            return Err(Error::InvalidInode);
        }
        let index = fragment / 512;
        let entry = fragment % 512;
        let index_offset = self
            .fragment_table
            .checked_add(index.checked_mul(8).ok_or(Error::Truncated)?)
            .ok_or(Error::Truncated)?;
        let block_position = read_u64(self.bytes, index_offset).ok_or(Error::Truncated)?;
        let block_position = self
            .offset
            .checked_add(usize::try_from(block_position).map_err(|_| Error::Truncated)?)
            .ok_or(Error::Truncated)?;
        let (data, _) = self.metadata_block(0, block_position)?;
        let entry_offset = entry.checked_mul(16).ok_or(Error::Truncated)?;
        let entry = data
            .get(entry_offset..entry_offset + 16)
            .ok_or(Error::InvalidInode)?;
        let start = read_u64(entry, 0).ok_or(Error::InvalidInode)?;
        let encoded = read_u32(entry, 8).ok_or(Error::InvalidInode)?;
        let start = self
            .offset
            .checked_add(usize::try_from(start).map_err(|_| Error::Truncated)?)
            .ok_or(Error::Truncated)?;
        let block = self.data_block_any(start, encoded, self.block_size)?;
        let end = offset.checked_add(tail).ok_or(Error::Truncated)?;
        block
            .get(offset..end)
            .map(|slice| slice.to_vec())
            .ok_or(Error::InvalidInode)
    }

    fn decompress(&self, input: &[u8], maximum: usize) -> Result<Vec<u8>, Error> {
        let out = match self.compression {
            1 => miniz_oxide::inflate::decompress_to_vec_zlib(input)
                .map_err(|_| Error::Decompress)?,
            2 => {
                #[cfg(feature = "std")]
                {
                    let mut out = Vec::new();
                    lzma_rs::lzma_decompress(&mut &input[..], &mut out)
                        .map_err(|_| Error::Decompress)?;
                    out
                }
                #[cfg(not(feature = "std"))]
                {
                    return Err(Error::UnsupportedCompression);
                }
            }
            3 => {
                let mut out = vec![0u8; maximum];
                let length = lzokay::decompress::decompress(input, &mut out)
                    .map_err(|_| Error::Decompress)?;
                out.truncate(length);
                out
            }
            4 => {
                #[cfg(feature = "std")]
                {
                    let mut out = Vec::new();
                    lzma_rs::xz_decompress(&mut &input[..], &mut out)
                        .map_err(|_| Error::Decompress)?;
                    out
                }
                #[cfg(not(feature = "std"))]
                {
                    return Err(Error::UnsupportedCompression);
                }
            }
            5 => lz4_flex::block::decompress(input, maximum).map_err(|_| Error::Decompress)?,
            6 => {
                let mut decoder = ruzstd::decoding::StreamingDecoder::new(input)
                    .map_err(|_| Error::Decompress)?;
                let mut out = Vec::new();
                decoder
                    .read_to_end(&mut out)
                    .map_err(|_| Error::Decompress)?;
                out
            }
            _ => return Err(Error::UnsupportedCompression),
        };
        if out.len() > maximum {
            return Err(Error::Decompress);
        }
        Ok(out)
    }

    /// Offset of the filesystem in the source bytes.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }
}

#[derive(Default)]
struct WalkState {
    entries: usize,
    bytes: usize,
    files: usize,
    directories: usize,
    symlinks: usize,
    hardlinks: usize,
    xattrs: usize,
    paths: BTreeMap<String, ()>,
    inodes: BTreeMap<u32, ()>,
    tree: Vec<TreeEntry>,
}

fn safe_symlink(target: &[u8]) -> bool {
    if target.is_empty() || target.contains(&0) {
        return false;
    }
    let Ok(target) = core::str::from_utf8(target) else {
        return false;
    };
    !target.starts_with('/')
        && target
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn parse_directory(data: &[u8]) -> Result<Vec<Entry>, Error> {
    let count = read_u32(data, 0).ok_or(Error::InvalidInode)? as usize + 1;
    let mut position = 12usize;
    let mut entries = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let offset = read_u16(data, position).ok_or(Error::InvalidInode)?;
        let _inode_offset = read_u16(data, position + 2).ok_or(Error::InvalidInode)?;
        let kind = read_u16(data, position + 4).ok_or(Error::InvalidInode)?;
        let name_size = read_u16(data, position + 6).ok_or(Error::InvalidInode)? as usize + 1;
        let name_start = position + 8;
        let name = data
            .get(name_start..name_start.checked_add(name_size).ok_or(Error::Truncated)?)
            .ok_or(Error::InvalidInode)?;
        if name.is_empty()
            || name.len() > 256
            || name.contains(&b'/')
            || name.contains(&0)
            || name == b"."
            || name == b".."
        {
            return Err(Error::InvalidName);
        }
        let name = core::str::from_utf8(name).map_err(|_| Error::InvalidName)?;
        entries.push(Entry {
            name: name.to_string(),
            inode: ((read_u32(data, 4).ok_or(Error::InvalidInode)? as u64) << 16)
                | u64::from(offset),
            kind,
        });
        position = name_start.checked_add(name_size).ok_or(Error::Truncated)?;
    }
    Ok(entries)
}

fn parse_block_sizes(
    data: &[u8],
    offset: usize,
    file_size: u64,
    fragment: u32,
    block_size: usize,
) -> Result<Vec<u32>, Error> {
    let size = usize::try_from(file_size).map_err(|_| Error::Limit)?;
    let count = if fragment == u32::MAX {
        size.div_ceil(block_size)
    } else {
        size / block_size
    };
    let start = offset;
    let end = start
        .checked_add(count.checked_mul(4).ok_or(Error::Truncated)?)
        .ok_or(Error::Truncated)?;
    let bytes = data.get(start..end).ok_or(Error::InvalidInode)?;
    let mut sizes = Vec::with_capacity(count);
    for chunk in bytes.as_chunks::<4>().0 {
        sizes.push(u32::from_le_bytes(*chunk));
    }
    Ok(sizes)
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

#[cfg(test)]
mod tests;
