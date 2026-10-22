// SPDX-License-Identifier: Apache-2.0
//! Aether's first storage and filesystem implementation.
//!
//! The format is deliberately small and allocation-free: 256 blocks of 512
//! bytes, 32 fixed-size inodes, up to eight contiguous blocks per file, and a
//! single directory block per directory. It is a bring-up filesystem for the
//! virtual block device, not a production on-disk format.
//!
//! All path resolution is scoped: a service or application supplies a root
//! prefix, and `..` is always rejected. A path therefore cannot escape the
//! caller's root by traversal, even if the caller supplies a malformed path.

#![cfg_attr(not(test), no_std)]

/// Size of one storage block.
pub const BLOCK_SIZE: usize = 512;
/// Number of blocks in the virtual device.
pub const BLOCK_COUNT: usize = 512;
/// Total device size in bytes.
pub const DISK_BYTES: usize = BLOCK_SIZE * BLOCK_COUNT;
/// Maximum inode number.
pub const MAX_INODES: usize = 96;
/// Blocks a file may occupy.
pub const MAX_FILE_BLOCKS: usize = 8;
/// Maximum file size.
pub const MAX_FILE_BYTES: usize = MAX_FILE_BLOCKS * BLOCK_SIZE;
/// Maximum bytes in one path.
pub const MAX_PATH: usize = 128;
/// Maximum file or directory name.
pub const MAX_NAME: usize = 16;
/// Directory entry size.
pub const ENTRY_BYTES: usize = 20;
/// Entries in one directory block.
pub const MAX_ENTRIES: usize = BLOCK_SIZE / ENTRY_BYTES;
/// Open file slots.
pub const MAX_OPEN: usize = 16;

const MAGIC: u32 = u32::from_le_bytes(*b"AFS1");
const VERSION: u16 = 1;
const ROOT_INODE: u16 = 1;
const INODE_TABLE_BLOCK: u16 = 1;
const INODE_BYTES: usize = 32;
const DATA_START_BLOCK: u16 =
    INODE_TABLE_BLOCK + (MAX_INODES * INODE_BYTES).div_ceil(BLOCK_SIZE) as u16;
const FREE_BITMAP_OFFSET: usize = 16;
// The free-block bitmap shares the superblock.
const _: () = assert!(FREE_BITMAP_OFFSET + BLOCK_COUNT.div_ceil(8) <= BLOCK_SIZE);
const MODE_FREE: u8 = 0;
const MODE_FILE: u8 = 1;
const MODE_DIR: u8 = 2;

/// Filesystem errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    /// The device rejected a read or write.
    Io,
    /// The superblock is missing or has an unsupported version.
    NotFormatted,
    /// On-disk metadata is inconsistent.
    Corrupt,
    /// No free inode or data block.
    NoSpace,
    /// The path does not exist.
    NotFound,
    /// A path component is a file where a directory was required.
    NotDirectory,
    /// The final component is a directory.
    IsDirectory,
    /// The name already exists.
    Exists,
    /// A component exceeds [`MAX_NAME`].
    NameTooLong,
    /// The joined path exceeds [`MAX_PATH`].
    PathTooLong,
    /// The path contains `..`.
    Escape,
    /// The requested access is not permitted by the open handle.
    Access,
    /// The open-file handle is stale or forged.
    BadHandle,
    /// The file would exceed [`MAX_FILE_BYTES`].
    FileTooLarge,
    /// The open-file table is full.
    TooManyOpen,
}

/// Filesystem space use returned by [`AetherFs::usage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsUsage {
    /// Data blocks in use.
    pub used_blocks: usize,
    /// Data blocks available to files and directories.
    pub blocks: usize,
    /// Inodes in use, including the root directory.
    pub used_inodes: usize,
    /// Allocatable inodes.
    pub inodes: usize,
}

/// A block device used by the filesystem.
pub trait BlockDevice {
    /// Reads one block.
    fn read_block(&mut self, index: u16, out: &mut [u8; BLOCK_SIZE]) -> Result<(), FsError>;
    /// Writes one block.
    fn write_block(&mut self, index: u16, data: &[u8; BLOCK_SIZE]) -> Result<(), FsError>;
}

/// A 128 KiB RAM disk used by the QEMU bring-up platform.
pub struct RamDisk {
    data: [u8; DISK_BYTES],
}

impl RamDisk {
    /// Creates a zeroed RAM disk.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data: [0; DISK_BYTES],
        }
    }

    /// Returns the raw disk bytes (used by diagnostics only).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }
}

impl Default for RamDisk {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockDevice for RamDisk {
    fn read_block(&mut self, index: u16, out: &mut [u8; BLOCK_SIZE]) -> Result<(), FsError> {
        let start = usize::from(index) * BLOCK_SIZE;
        let source = self
            .data
            .get(start..start + BLOCK_SIZE)
            .ok_or(FsError::Io)?;
        out.copy_from_slice(source);
        Ok(())
    }

    fn write_block(&mut self, index: u16, data: &[u8; BLOCK_SIZE]) -> Result<(), FsError> {
        let start = usize::from(index) * BLOCK_SIZE;
        let destination = self
            .data
            .get_mut(start..start + BLOCK_SIZE)
            .ok_or(FsError::Io)?;
        destination.copy_from_slice(data);
        Ok(())
    }
}

/// Access flags for an open file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access(u8);

impl Access {
    /// Read access.
    pub const READ: Self = Self(1);
    /// Write access.
    pub const WRITE: Self = Self(2);
    /// Create the file if it does not exist.
    pub const CREATE: Self = Self(4);

    /// Returns `true` if all flags in `other` are present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Builds an access set from raw flags.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 7)
    }

    /// Raw flags.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl core::ops::BitOr for Access {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

#[derive(Clone, Copy)]
struct Inode {
    mode: u8,
    size: u16,
    blocks: [u16; MAX_FILE_BLOCKS],
}

impl Inode {
    const EMPTY: Self = Self {
        mode: MODE_FREE,
        size: 0,
        blocks: [0; MAX_FILE_BLOCKS],
    };

    fn encode(self, out: &mut [u8; INODE_BYTES]) {
        out.fill(0);
        out[0] = self.mode;
        out[2..4].copy_from_slice(&self.size.to_le_bytes());
        for (index, block) in self.blocks.iter().enumerate() {
            let offset = 4 + index * 2;
            out[offset..offset + 2].copy_from_slice(&block.to_le_bytes());
        }
    }

    fn decode(bytes: &[u8; INODE_BYTES]) -> Self {
        let mut blocks = [0u16; MAX_FILE_BLOCKS];
        for (index, block) in blocks.iter_mut().enumerate() {
            let offset = 4 + index * 2;
            *block = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        }
        Self {
            mode: bytes[0],
            size: u16::from_le_bytes([bytes[2], bytes[3]]),
            blocks,
        }
    }

    const fn is_dir(self) -> bool {
        self.mode == MODE_DIR
    }

    const fn is_file(self) -> bool {
        self.mode == MODE_FILE
    }

    const fn block_count(self) -> usize {
        (self.size as usize).div_ceil(BLOCK_SIZE)
    }

    /// Validates on-disk metadata before it is used to index block tables.
    ///
    /// The block device is untrusted input: a corrupt or hostile image must be
    /// rejected with an error rather than panic the kernel through an
    /// out-of-bounds block number or an oversized size field.
    fn validate(self) -> Result<(), FsError> {
        match self.mode {
            MODE_FREE => {
                if self.size != 0 || self.blocks.iter().any(|block| *block != 0) {
                    return Err(FsError::Corrupt);
                }
            }
            MODE_FILE => {
                if self.size as usize > MAX_FILE_BYTES {
                    return Err(FsError::Corrupt);
                }
                let blocks = self.block_count();
                if blocks > MAX_FILE_BLOCKS {
                    return Err(FsError::Corrupt);
                }
                for (index, block) in self.blocks.iter().enumerate() {
                    if index < blocks {
                        if *block == 0 || usize::from(*block) >= BLOCK_COUNT {
                            return Err(FsError::Corrupt);
                        }
                    } else if *block != 0 {
                        return Err(FsError::Corrupt);
                    }
                }
            }
            MODE_DIR => {
                if self.size as usize > BLOCK_SIZE
                    || !(self.size as usize).is_multiple_of(ENTRY_BYTES)
                {
                    return Err(FsError::Corrupt);
                }
                if self.size != 0 && self.blocks[0] == 0 {
                    return Err(FsError::Corrupt);
                }
                if self.blocks[0] != 0 && usize::from(self.blocks[0]) >= BLOCK_COUNT {
                    return Err(FsError::Corrupt);
                }
                if self.blocks[1..].iter().any(|block| *block != 0) {
                    return Err(FsError::Corrupt);
                }
            }
            _ => return Err(FsError::Corrupt),
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct OpenSlot {
    alive: bool,
    generation: u32,
    inode: u16,
    access: Access,
    position: u16,
}

impl OpenSlot {
    const EMPTY: Self = Self {
        alive: false,
        generation: 0,
        inode: 0,
        access: Access(0),
        position: 0,
    };
}

/// An opaque open-file identifier. The generation is checked on every
/// operation, so a stale number cannot name a recycled slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenId {
    /// Slot index.
    pub index: u8,
    /// Slot generation. It is 32 bits wide so it cannot wrap back onto a
    /// live open in practice.
    pub generation: u32,
}

/// Result of a directory listing entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// Entry name, NUL padded.
    pub name: [u8; MAX_NAME],
    /// Name length.
    pub name_len: u8,
    /// Inode number.
    pub inode: u16,
    /// Directory or file.
    pub is_dir: bool,
}

/// A bounded filesystem over a block device.
pub struct AetherFs<D: BlockDevice> {
    device: D,
    open: [OpenSlot; MAX_OPEN],
    next_generation: u32,
}

impl<D: BlockDevice> AetherFs<D> {
    /// Creates a filesystem wrapper over `device`.
    #[must_use]
    pub const fn new(device: D) -> Self {
        Self {
            device,
            open: [OpenSlot::EMPTY; MAX_OPEN],
            next_generation: 1,
        }
    }

    /// Returns the underlying device.
    #[must_use]
    pub fn device(&self) -> &D {
        &self.device
    }

    /// Formats the device and creates the root directory.
    pub fn format(&mut self) -> Result<(), FsError> {
        self.open = [OpenSlot::EMPTY; MAX_OPEN];
        self.next_generation = 1;
        let zero = [0u8; BLOCK_SIZE];
        for block in 0..BLOCK_COUNT {
            self.device.write_block(block as u16, &zero)?;
        }

        let mut superblock = [0u8; BLOCK_SIZE];
        superblock[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        superblock[4..6].copy_from_slice(&VERSION.to_le_bytes());
        superblock[6..8].copy_from_slice(&(MAX_INODES as u16).to_le_bytes());
        superblock[8..10].copy_from_slice(&ROOT_INODE.to_le_bytes());
        self.device.write_block(0, &superblock)?;

        // Reserve the superblock, inode table, and root directory block.
        for block in 0..DATA_START_BLOCK {
            self.set_block_used(block, true)?;
        }
        let root_block = self.allocate_block()?;
        let root = Inode {
            mode: MODE_DIR,
            size: 0,
            blocks: [root_block, 0, 0, 0, 0, 0, 0, 0],
        };
        self.write_inode(ROOT_INODE, root)?;
        Ok(())
    }

    /// Creates a directory below `scope`.
    pub fn mkdir(&mut self, scope: &str, path: &str) -> Result<(), FsError> {
        let resolved = resolve(scope, path)?;
        let (parent_path, name) = resolved.parent_and_name()?;
        let parent = self.lookup(&parent_path)?;
        let parent_inode = self.read_inode(parent)?;
        if !parent_inode.is_dir() {
            return Err(FsError::NotDirectory);
        }
        if self.dir_find(parent, name.as_bytes())?.is_some() {
            return Err(FsError::Exists);
        }
        let inode_number = self.allocate_inode()?;
        let block = self.allocate_block()?;
        if let Err(error) = self.write_inode(
            inode_number,
            Inode {
                mode: MODE_DIR,
                size: 0,
                blocks: [block, 0, 0, 0, 0, 0, 0, 0],
            },
        ) {
            let _ = self.free_block(block);
            return Err(error);
        }
        match self.dir_add(parent, name.as_bytes(), inode_number, true) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = self.free_inode(inode_number);
                let _ = self.free_block(block);
                Err(error)
            }
        }
    }

    /// Creates or replaces a file below `scope`.
    pub fn write_file(&mut self, scope: &str, path: &str, data: &[u8]) -> Result<(), FsError> {
        if data.len() > MAX_FILE_BYTES {
            return Err(FsError::FileTooLarge);
        }
        let resolved = resolve(scope, path)?;
        let inode_number = match self.lookup(&resolved) {
            Ok(inode) => inode,
            Err(FsError::NotFound) => {
                let (parent_path, name) = resolved.parent_and_name()?;
                let parent = self.lookup(&parent_path)?;
                if !self.read_inode(parent)?.is_dir() {
                    return Err(FsError::NotDirectory);
                }
                let inode = self.allocate_inode()?;
                if let Err(error) = self.write_inode(
                    inode,
                    Inode {
                        mode: MODE_FILE,
                        ..Inode::EMPTY
                    },
                ) {
                    let _ = self.free_inode(inode);
                    return Err(error);
                }
                if let Err(error) = self.dir_add(parent, name.as_bytes(), inode, false) {
                    let _ = self.free_inode(inode);
                    return Err(error);
                }
                inode
            }
            Err(error) => return Err(error),
        };
        let mut inode = self.read_inode(inode_number)?;
        if !inode.is_file() {
            return Err(FsError::IsDirectory);
        }
        self.write_inode_blocks(inode_number, &mut inode, data)
    }

    /// Reads a whole file below `scope`.
    pub fn read_file(&mut self, scope: &str, path: &str, out: &mut [u8]) -> Result<usize, FsError> {
        let resolved = resolve(scope, path)?;
        let inode_number = self.lookup(&resolved)?;
        let inode = self.read_inode(inode_number)?;
        if !inode.is_file() {
            return Err(FsError::IsDirectory);
        }
        let length = usize::from(inode.size).min(out.len());
        self.read_inode_data(inode, &mut out[..length])?;
        Ok(length)
    }

    /// Returns a file's size.
    pub fn stat(&mut self, scope: &str, path: &str) -> Result<usize, FsError> {
        let resolved = resolve(scope, path)?;
        let inode_number = self.lookup(&resolved)?;
        let inode = self.read_inode(inode_number)?;
        if inode.is_dir() {
            return Err(FsError::IsDirectory);
        }
        Ok(usize::from(inode.size))
    }

    /// Lists a directory into `out` as `name<TAB>kind<TAB>size\n` lines.
    pub fn list(&mut self, scope: &str, path: &str, out: &mut [u8]) -> Result<usize, FsError> {
        let resolved = resolve(scope, path)?;
        let inode_number = self.lookup(&resolved)?;
        let inode = self.read_inode(inode_number)?;
        if !inode.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let mut written = 0usize;
        for entry in self.dir_entries(inode)?.into_iter().flatten() {
            let name = entry_text(&entry);
            let kind = if entry.is_dir { "d" } else { "f" };
            let size = if entry.is_dir {
                0
            } else {
                usize::from(self.read_inode(entry.inode)?.size)
            };
            written += format_entry(&mut out[written..], name, kind, size)?;
        }
        Ok(written)
    }

    /// Opens a file. `CREATE` creates an empty file when it is missing.
    pub fn open(&mut self, scope: &str, path: &str, access: Access) -> Result<OpenId, FsError> {
        let resolved = resolve(scope, path)?;
        let inode_number = match self.lookup(&resolved) {
            Ok(inode) => inode,
            Err(FsError::NotFound) if access.contains(Access::CREATE) => {
                let (parent_path, name) = resolved.parent_and_name()?;
                let parent = self.lookup(&parent_path)?;
                if !self.read_inode(parent)?.is_dir() {
                    return Err(FsError::NotDirectory);
                }
                let inode = self.allocate_inode()?;
                if let Err(error) = self.write_inode(
                    inode,
                    Inode {
                        mode: MODE_FILE,
                        ..Inode::EMPTY
                    },
                ) {
                    let _ = self.free_inode(inode);
                    return Err(error);
                }
                if let Err(error) = self.dir_add(parent, name.as_bytes(), inode, false) {
                    let _ = self.free_inode(inode);
                    return Err(error);
                }
                inode
            }
            Err(error) => return Err(error),
        };
        let inode = self.read_inode(inode_number)?;
        if inode.is_dir() {
            return Err(FsError::IsDirectory);
        }
        let slot = self
            .open
            .iter()
            .position(|slot| !slot.alive)
            .ok_or(FsError::TooManyOpen)?;
        let generation = self.next_generation.max(1);
        self.next_generation = generation.wrapping_add(1).max(1);
        self.open[slot] = OpenSlot {
            alive: true,
            generation,
            inode: inode_number,
            access,
            position: 0,
        };
        Ok(OpenId {
            index: slot as u8,
            generation,
        })
    }

    /// Reads from an open file at its current position.
    pub fn read(&mut self, id: OpenId, out: &mut [u8]) -> Result<usize, FsError> {
        let slot = *self.slot(id)?;
        if !slot.access.contains(Access::READ) {
            return Err(FsError::Access);
        }
        let inode = self.read_inode(slot.inode)?;
        let position = usize::from(slot.position).min(usize::from(inode.size));
        let length = (usize::from(inode.size) - position).min(out.len());
        if length == 0 {
            return Ok(0);
        }
        let mut data = [0u8; MAX_FILE_BYTES];
        self.read_inode_data(inode, &mut data[..usize::from(inode.size)])?;
        out[..length].copy_from_slice(&data[position..position + length]);
        let slot = self.slot_mut(id)?;
        slot.position = (position + length) as u16;
        Ok(length)
    }

    /// Writes at the current position, extending or replacing file contents.
    pub fn write(&mut self, id: OpenId, data: &[u8]) -> Result<usize, FsError> {
        let slot = *self.slot(id)?;
        if !slot.access.contains(Access::WRITE) {
            return Err(FsError::Access);
        }
        let mut inode = self.read_inode(slot.inode)?;
        let position = usize::from(slot.position);
        let end = position
            .checked_add(data.len())
            .ok_or(FsError::FileTooLarge)?;
        if end > MAX_FILE_BYTES {
            return Err(FsError::FileTooLarge);
        }
        let mut buffer = [0u8; MAX_FILE_BYTES];
        self.read_inode_data(inode, &mut buffer[..usize::from(inode.size)])?;
        buffer[position..end].copy_from_slice(data);
        let size = end.max(usize::from(inode.size));
        inode.size = size as u16;
        self.write_inode_blocks(slot.inode, &mut inode, &buffer[..size])?;
        let slot = self.slot_mut(id)?;
        slot.position = end as u16;
        Ok(data.len())
    }

    /// Closes an open file.
    pub fn close(&mut self, id: OpenId) -> Result<(), FsError> {
        let slot = self.slot_mut(id)?;
        slot.alive = false;
        Ok(())
    }

    /// Deletes a file below `scope`.
    pub fn unlink(&mut self, scope: &str, path: &str) -> Result<(), FsError> {
        let resolved = resolve(scope, path)?;
        let (parent_path, name) = resolved.parent_and_name()?;
        let parent = self.lookup(&parent_path)?;
        let (inode_number, is_dir) = self
            .dir_find(parent, name.as_bytes())?
            .ok_or(FsError::NotFound)?;
        if is_dir {
            return Err(FsError::IsDirectory);
        }
        // An open handle must not outlive the inode or point at blocks that a
        // later create can reuse. Refuse deletion until the owner closes it.
        if self
            .open
            .iter()
            .any(|slot| slot.alive && slot.inode == inode_number)
        {
            return Err(FsError::Access);
        }
        self.dir_remove(parent, name.as_bytes())?;
        let mut inode = self.read_inode(inode_number)?;
        self.free_inode_blocks(&mut inode)?;
        self.free_inode(inode_number)
    }

    /// Renames or moves a file or directory below `scope`.
    ///
    /// The operation changes only the parent directory entry; the inode and
    /// open handles remain valid. Existing destinations are refused, and a
    /// directory cannot be moved inside itself.
    pub fn rename(&mut self, scope: &str, from: &str, to: &str) -> Result<(), FsError> {
        let source = resolve(scope, from)?;
        let destination = resolve(scope, to)?;
        if source.as_str() == destination.as_str() {
            return Ok(());
        }
        let (source_parent_path, source_name) = source.parent_and_name()?;
        let (destination_parent_path, destination_name) = destination.parent_and_name()?;
        let source_parent = self.lookup(&source_parent_path)?;
        let (inode_number, is_dir) = self
            .dir_find(source_parent, source_name.as_bytes())?
            .ok_or(FsError::NotFound)?;
        if is_dir {
            let prefix = source.as_str();
            let target = destination.as_str();
            if target
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
            {
                return Err(FsError::Access);
            }
        }
        let destination_parent = self.lookup(&destination_parent_path)?;
        if !self.read_inode(destination_parent)?.is_dir() {
            return Err(FsError::NotDirectory);
        }
        if self
            .dir_find(destination_parent, destination_name.as_bytes())?
            .is_some()
        {
            return Err(FsError::Exists);
        }
        self.dir_add(
            destination_parent,
            destination_name.as_bytes(),
            inode_number,
            is_dir,
        )?;
        if let Err(error) = self.dir_remove(source_parent, source_name.as_bytes()) {
            // Roll back the destination entry rather than leaving two names for
            // one inode after a partial rename.
            let _ = self.dir_remove(destination_parent, destination_name.as_bytes());
            return Err(error);
        }
        Ok(())
    }

    /// Copies a file below `scope`, preserving no metadata beyond its bytes.
    ///
    /// The destination must not exist. Directories and open destination files
    /// are refused by the underlying open/write path.
    pub fn copy_file(&mut self, scope: &str, from: &str, to: &str) -> Result<(), FsError> {
        let mut buffer = [0u8; MAX_FILE_BYTES];
        let length = self.read_file(scope, from, &mut buffer)?;
        let resolved = resolve(scope, to)?;
        if self.lookup(&resolved).is_ok() {
            return Err(FsError::Exists);
        }
        self.write_file(scope, to, &buffer[..length])
    }

    /// Deletes an empty directory below `scope`.
    pub fn rmdir(&mut self, scope: &str, path: &str) -> Result<(), FsError> {
        let resolved = resolve(scope, path)?;
        let (parent_path, name) = resolved.parent_and_name()?;
        let parent = self.lookup(&parent_path)?;
        let (inode_number, is_dir) = self
            .dir_find(parent, name.as_bytes())?
            .ok_or(FsError::NotFound)?;
        if !is_dir {
            return Err(FsError::NotDirectory);
        }
        let inode = self.read_inode(inode_number)?;
        if inode.size != 0 {
            return Err(FsError::Exists);
        }
        self.dir_remove(parent, name.as_bytes())?;
        let mut inode = self.read_inode(inode_number)?;
        self.free_inode_blocks(&mut inode)?;
        self.free_inode(inode_number)
    }

    /// Deletes a file or empty directory below `scope`.
    pub fn remove(&mut self, scope: &str, path: &str) -> Result<(), FsError> {
        let resolved = resolve(scope, path)?;
        let (parent_path, name) = resolved.parent_and_name()?;
        let parent = self.lookup(&parent_path)?;
        let (_, is_dir) = self
            .dir_find(parent, name.as_bytes())?
            .ok_or(FsError::NotFound)?;
        if is_dir {
            self.rmdir(scope, path)
        } else {
            self.unlink(scope, path)
        }
    }

    /// Reports space use: `(used data blocks, data blocks, used inodes,
    /// inodes)`. Inode 0 is reserved and counted as neither.
    pub fn usage(&mut self) -> Result<FsUsage, FsError> {
        let mut used_blocks = 0usize;
        for block in DATA_START_BLOCK..BLOCK_COUNT as u16 {
            if self.block_used(block)? {
                used_blocks += 1;
            }
        }
        let mut used_inodes = 0usize;
        for index in 1..MAX_INODES as u16 {
            if self.read_inode(index)?.mode != MODE_FREE {
                used_inodes += 1;
            }
        }
        Ok(FsUsage {
            used_blocks,
            blocks: BLOCK_COUNT - usize::from(DATA_START_BLOCK),
            used_inodes,
            inodes: MAX_INODES - 1,
        })
    }

    fn slot(&self, id: OpenId) -> Result<&OpenSlot, FsError> {
        let slot = self
            .open
            .get(usize::from(id.index))
            .ok_or(FsError::BadHandle)?;
        if !slot.alive || slot.generation != id.generation {
            return Err(FsError::BadHandle);
        }
        Ok(slot)
    }

    fn slot_mut(&mut self, id: OpenId) -> Result<&mut OpenSlot, FsError> {
        let slot = self
            .open
            .get_mut(usize::from(id.index))
            .ok_or(FsError::BadHandle)?;
        if !slot.alive || slot.generation != id.generation {
            return Err(FsError::BadHandle);
        }
        Ok(slot)
    }

    fn lookup(&mut self, path: &FixedPath) -> Result<u16, FsError> {
        let mut current = ROOT_INODE;
        let mut components = path.components().peekable();
        while let Some(component) = components.next() {
            let (next, is_dir) = self
                .dir_find(current, component.as_bytes())?
                .ok_or(FsError::NotFound)?;
            if !is_dir && components.peek().is_some() {
                return Err(FsError::NotDirectory);
            }
            current = next;
        }
        Ok(current)
    }

    fn read_superblock(&mut self) -> Result<[u8; BLOCK_SIZE], FsError> {
        let mut data = [0u8; BLOCK_SIZE];
        self.device.read_block(0, &mut data)?;
        if data[0..4] != MAGIC.to_le_bytes() {
            return Err(FsError::NotFormatted);
        }
        let version = u16::from_le_bytes([data[4], data[5]]);
        let inode_count = u16::from_le_bytes([data[6], data[7]]);
        let root = u16::from_le_bytes([data[8], data[9]]);
        if version != VERSION || inode_count != MAX_INODES as u16 || root != ROOT_INODE {
            return Err(FsError::Corrupt);
        }
        Ok(data)
    }

    fn read_inode(&mut self, index: u16) -> Result<Inode, FsError> {
        if index == 0 || usize::from(index) >= MAX_INODES {
            return Err(FsError::Corrupt);
        }
        self.read_superblock()?;
        let offset = usize::from(index) * INODE_BYTES;
        let block = INODE_TABLE_BLOCK + (offset / BLOCK_SIZE) as u16;
        let start = offset % BLOCK_SIZE;
        let mut data = [0u8; BLOCK_SIZE];
        self.device.read_block(block, &mut data)?;
        let mut bytes = [0u8; INODE_BYTES];
        bytes.copy_from_slice(&data[start..start + INODE_BYTES]);
        let inode = Inode::decode(&bytes);
        inode.validate()?;
        Ok(inode)
    }

    fn write_inode(&mut self, index: u16, inode: Inode) -> Result<(), FsError> {
        if index == 0 || usize::from(index) >= MAX_INODES {
            return Err(FsError::Corrupt);
        }
        self.read_superblock()?;
        let offset = usize::from(index) * INODE_BYTES;
        let block = INODE_TABLE_BLOCK + (offset / BLOCK_SIZE) as u16;
        let start = offset % BLOCK_SIZE;
        let mut data = [0u8; BLOCK_SIZE];
        self.device.read_block(block, &mut data)?;
        let mut bytes = [0u8; INODE_BYTES];
        inode.encode(&mut bytes);
        data[start..start + INODE_BYTES].copy_from_slice(&bytes);
        self.device.write_block(block, &data)
    }

    fn allocate_inode(&mut self) -> Result<u16, FsError> {
        for index in 1..MAX_INODES as u16 {
            if self.read_inode(index)?.mode == MODE_FREE {
                return Ok(index);
            }
        }
        Err(FsError::NoSpace)
    }

    fn free_inode(&mut self, index: u16) -> Result<(), FsError> {
        self.write_inode(index, Inode::EMPTY)
    }

    fn allocate_block(&mut self) -> Result<u16, FsError> {
        for block in DATA_START_BLOCK..BLOCK_COUNT as u16 {
            if !self.block_used(block)? {
                self.set_block_used(block, true)?;
                let zero = [0u8; BLOCK_SIZE];
                self.device.write_block(block, &zero)?;
                return Ok(block);
            }
        }
        Err(FsError::NoSpace)
    }

    fn free_block(&mut self, block: u16) -> Result<(), FsError> {
        self.set_block_used(block, false)
    }

    fn block_used(&mut self, block: u16) -> Result<bool, FsError> {
        if usize::from(block) >= BLOCK_COUNT {
            return Err(FsError::Corrupt);
        }
        let data = self.read_superblock()?;
        let byte = FREE_BITMAP_OFFSET + usize::from(block) / 8;
        let bit = 1u8 << (block % 8);
        Ok(data[byte] & bit != 0)
    }

    fn set_block_used(&mut self, block: u16, used: bool) -> Result<(), FsError> {
        if usize::from(block) >= BLOCK_COUNT {
            return Err(FsError::Corrupt);
        }
        let mut data = self.read_superblock()?;
        let byte = FREE_BITMAP_OFFSET + usize::from(block) / 8;
        let bit = 1u8 << (block % 8);
        if used {
            data[byte] |= bit;
        } else {
            data[byte] &= !bit;
        }
        self.device.write_block(0, &data)
    }

    fn read_inode_data(&mut self, inode: Inode, out: &mut [u8]) -> Result<(), FsError> {
        let mut block = [0u8; BLOCK_SIZE];
        let mut copied = 0usize;
        for number in inode.blocks.iter().take(inode.block_count()) {
            if copied == out.len() {
                break;
            }
            if *number == 0 || usize::from(*number) >= BLOCK_COUNT {
                return Err(FsError::Corrupt);
            }
            self.device.read_block(*number, &mut block)?;
            let length = (out.len() - copied).min(BLOCK_SIZE);
            out[copied..copied + length].copy_from_slice(&block[..length]);
            copied += length;
        }
        Ok(())
    }

    /// Replaces a file's contents and commits the inode before releasing the
    /// old blocks.
    ///
    /// New blocks are allocated and filled while the old mapping is still
    /// valid. If anything fails, the old blocks and the on-disk inode are left
    /// untouched; a failed write must not make an existing file point at
    /// freed or partially overwritten storage.
    fn write_inode_blocks(
        &mut self,
        inode_number: u16,
        inode: &mut Inode,
        data: &[u8],
    ) -> Result<(), FsError> {
        if data.len() > MAX_FILE_BYTES {
            return Err(FsError::FileTooLarge);
        }
        let needed = data.len().div_ceil(BLOCK_SIZE);
        let mut new_blocks = [0u16; MAX_FILE_BLOCKS];
        for slot in new_blocks.iter_mut().take(needed) {
            match self.allocate_block() {
                Ok(block) => *slot = block,
                Err(error) => {
                    let _ = self.release_blocks(new_blocks);
                    return Err(error);
                }
            }
        }
        for (index, block) in new_blocks.iter().take(needed).enumerate() {
            let start = index * BLOCK_SIZE;
            let end = (start + BLOCK_SIZE).min(data.len());
            let mut buffer = [0u8; BLOCK_SIZE];
            buffer[..end - start].copy_from_slice(&data[start..end]);
            if let Err(error) = self.device.write_block(*block, &buffer) {
                let _ = self.release_blocks(new_blocks);
                return Err(error);
            }
        }

        let old_blocks = inode.blocks;
        let old_size = inode.size;
        inode.blocks = new_blocks;
        inode.size = data.len() as u16;
        if let Err(error) = self.write_inode(inode_number, *inode) {
            inode.blocks = old_blocks;
            inode.size = old_size;
            let _ = self.release_blocks(new_blocks);
            return Err(error);
        }
        // The inode now names only the new blocks. Releasing the old blocks
        // after the commit means a failure here leaks space but cannot corrupt
        // the file.
        self.release_blocks(old_blocks)
    }

    fn release_blocks(&mut self, blocks: [u16; MAX_FILE_BLOCKS]) -> Result<(), FsError> {
        for block in blocks {
            if block != 0 {
                self.free_block(block)?;
            }
        }
        Ok(())
    }

    fn free_inode_blocks(&mut self, inode: &mut Inode) -> Result<(), FsError> {
        for block in inode.blocks {
            if block != 0 {
                self.free_block(block)?;
            }
        }
        inode.blocks = [0; MAX_FILE_BLOCKS];
        inode.size = 0;
        Ok(())
    }

    fn dir_entries(&mut self, inode: Inode) -> Result<[Option<DirEntry>; MAX_ENTRIES], FsError> {
        let mut entries = [None; MAX_ENTRIES];
        if !inode.is_dir() {
            return Err(FsError::NotDirectory);
        }
        if inode.blocks[0] != 0 {
            let mut data = [0u8; BLOCK_SIZE];
            self.device.read_block(inode.blocks[0], &mut data)?;
            let count = usize::from(inode.size) / ENTRY_BYTES;
            if count > MAX_ENTRIES {
                return Err(FsError::Corrupt);
            }
            for (index, entry) in entries.iter_mut().enumerate() {
                if index >= count {
                    break;
                }
                let start = index * ENTRY_BYTES;
                let mut name = [0u8; MAX_NAME];
                name.copy_from_slice(&data[start..start + MAX_NAME]);
                let name_len = name.iter().position(|byte| *byte == 0).unwrap_or(MAX_NAME);
                let inode_number = u16::from_le_bytes([data[start + 16], data[start + 17]]);
                *entry = Some(DirEntry {
                    name,
                    name_len: name_len as u8,
                    inode: inode_number,
                    is_dir: data[start + 18] == 2,
                });
            }
        }
        Ok(entries)
    }

    fn dir_find(&mut self, directory: u16, name: &[u8]) -> Result<Option<(u16, bool)>, FsError> {
        let inode = self.read_inode(directory)?;
        for entry in self.dir_entries(inode)?.into_iter().flatten() {
            if &entry.name[..usize::from(entry.name_len)] == name {
                return Ok(Some((entry.inode, entry.is_dir)));
            }
        }
        Ok(None)
    }

    fn dir_add(
        &mut self,
        directory: u16,
        name: &[u8],
        inode_number: u16,
        is_dir: bool,
    ) -> Result<(), FsError> {
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(FsError::NameTooLong);
        }
        let mut inode = self.read_inode(directory)?;
        if !inode.is_dir() {
            return Err(FsError::NotDirectory);
        }
        if usize::from(inode.size) + ENTRY_BYTES > BLOCK_SIZE {
            return Err(FsError::NoSpace);
        }
        let block = if inode.blocks[0] == 0 {
            let block = self.allocate_block()?;
            inode.blocks[0] = block;
            block
        } else {
            inode.blocks[0]
        };
        let mut data = [0u8; BLOCK_SIZE];
        self.device.read_block(block, &mut data)?;
        let start = usize::from(inode.size);
        data[start..start + MAX_NAME].fill(0);
        data[start..start + name.len()].copy_from_slice(name);
        data[start + 16..start + 18].copy_from_slice(&inode_number.to_le_bytes());
        data[start + 18] = if is_dir { 2 } else { 1 };
        self.device.write_block(block, &data)?;
        inode.size += ENTRY_BYTES as u16;
        self.write_inode(directory, inode)
    }

    fn dir_remove(&mut self, directory: u16, name: &[u8]) -> Result<(), FsError> {
        let mut inode = self.read_inode(directory)?;
        if inode.blocks[0] == 0 {
            return Err(FsError::NotFound);
        }
        let mut data = [0u8; BLOCK_SIZE];
        self.device.read_block(inode.blocks[0], &mut data)?;
        let count = usize::from(inode.size) / ENTRY_BYTES;
        for index in 0..count {
            let start = index * ENTRY_BYTES;
            let length = data[start..start + MAX_NAME]
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(MAX_NAME);
            if &data[start..start + length] == name {
                let last = (count - 1) * ENTRY_BYTES;
                data.copy_within(last..last + ENTRY_BYTES, start);
                inode.size -= ENTRY_BYTES as u16;
                self.device.write_block(inode.blocks[0], &data)?;
                return self.write_inode(directory, inode);
            }
        }
        Err(FsError::NotFound)
    }
}

fn entry_text(entry: &DirEntry) -> &str {
    core::str::from_utf8(&entry.name[..usize::from(entry.name_len)]).unwrap_or("?")
}

fn format_entry(out: &mut [u8], name: &str, kind: &str, size: usize) -> Result<usize, FsError> {
    let mut written = 0usize;
    for part in [name, "\t", kind, "\t"] {
        let bytes = part.as_bytes();
        if written + bytes.len() > out.len() {
            return Err(FsError::PathTooLong);
        }
        out[written..written + bytes.len()].copy_from_slice(bytes);
        written += bytes.len();
    }
    let mut digits = [0u8; 20];
    let mut value = size;
    let mut index = digits.len();
    if value == 0 {
        index -= 1;
        digits[index] = b'0';
    }
    while value > 0 {
        index -= 1;
        digits[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    for byte in &digits[index..] {
        if written == out.len() {
            return Err(FsError::PathTooLong);
        }
        out[written] = *byte;
        written += 1;
    }
    if written == out.len() {
        return Err(FsError::PathTooLong);
    }
    out[written] = b'\n';
    Ok(written + 1)
}

/// A normalized path relative to a service root.
#[derive(Clone, Copy)]
struct FixedPath {
    bytes: [u8; MAX_PATH],
    len: usize,
}

impl FixedPath {
    const fn new() -> Self {
        Self {
            bytes: [0; MAX_PATH],
            len: 0,
        }
    }

    fn push(&mut self, component: &str) -> Result<(), FsError> {
        if component.len() > MAX_NAME {
            return Err(FsError::NameTooLong);
        }
        let needed = self.len + component.len() + usize::from(self.len != 0);
        if needed > MAX_PATH {
            return Err(FsError::PathTooLong);
        }
        if self.len != 0 {
            self.bytes[self.len] = b'/';
            self.len += 1;
        }
        self.bytes[self.len..self.len + component.len()].copy_from_slice(component.as_bytes());
        self.len += component.len();
        Ok(())
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    fn components(&self) -> impl Iterator<Item = &str> {
        self.as_str().split('/').filter(|part| !part.is_empty())
    }

    fn last_component(&self) -> &str {
        self.components().last().unwrap_or("")
    }

    fn parent_and_name(&self) -> Result<(Self, &str), FsError> {
        let count = self.components().count();
        if count == 0 {
            return Err(FsError::IsDirectory);
        }
        let name = self.last_component();
        let mut parent = Self::new();
        for (index, component) in self.components().enumerate() {
            if index + 1 == count {
                break;
            }
            parent.push(component)?;
        }
        Ok((parent, name))
    }
}

/// Joins a trusted root scope and an untrusted relative path.
///
/// `..` is rejected rather than resolved. This is stricter than a POSIX
/// resolver and makes root escape impossible by construction.
fn resolve(scope: &str, path: &str) -> Result<FixedPath, FsError> {
    let mut resolved = FixedPath::new();
    for component in scope.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            return Err(FsError::Escape);
        }
        resolved.push(component)?;
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            return Err(FsError::Escape);
        }
        if component.len() > MAX_NAME {
            return Err(FsError::NameTooLong);
        }
        resolved.push(component)?;
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests;
