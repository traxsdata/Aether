// SPDX-License-Identifier: Apache-2.0
//! Minimal, defensive ELF64 parser for the kernel image.
//!
//! The loader deliberately does not use an external ELF crate: the accepted
//! subset is small, and keeping the parser here makes its security properties
//! reviewable and host-testable.

use crate::{PAGE_SIZE, align_down, align_up};

/// Maximum number of program headers accepted in a kernel image.
pub const MAX_SEGMENTS: usize = 16;

const ELF_HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const PT_LOAD: u32 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;

/// A validated loadable segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Offset of the segment bytes in the ELF file.
    pub file_offset: u64,
    /// Number of bytes to copy from the file.
    pub file_size: u64,
    /// Number of bytes the segment occupies in memory (the remainder is zero).
    pub memory_size: u64,
    /// Physical destination address.
    pub physical_address: u64,
    /// ELF segment flags: read, write, and execute bits.
    pub flags: u32,
}

/// Segment is executable.
pub const PF_X: u32 = 1;
/// Segment is writable.
pub const PF_W: u32 = 2;
/// Segment is readable.
pub const PF_R: u32 = 4;

impl Segment {
    /// Exclusive physical end address, or `None` on overflow.
    #[must_use]
    pub const fn end(&self) -> Option<u64> {
        self.physical_address.checked_add(self.memory_size)
    }
}

/// A parsed kernel image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    segments: [Segment; MAX_SEGMENTS],
    segment_count: usize,
    entry: u64,
    start: u64,
    end: u64,
}

impl Image {
    /// Loadable segments in program-header order.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments[..self.segment_count]
    }

    /// Kernel entry point.
    #[must_use]
    pub const fn entry(&self) -> u64 {
        self.entry
    }

    /// First physical address covered by the image.
    #[must_use]
    pub const fn start(&self) -> u64 {
        self.start
    }

    /// Exclusive physical end address covered by the image.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.end
    }
}

/// Reasons parsing can fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// File is shorter than the ELF header.
    TooShort,
    /// ELF magic is missing.
    BadMagic,
    /// Not a 64-bit little-endian ELF.
    BadClass,
    /// Unsupported ELF version.
    BadVersion,
    /// Not an executable.
    NotExecutable,
    /// Wrong machine architecture.
    WrongMachine,
    /// Header sizes are inconsistent.
    BadHeader,
    /// Program header table is out of bounds.
    BadProgramHeaders,
    /// No loadable segments.
    NoSegments,
    /// More loadable segments than `MAX_SEGMENTS`.
    TooManySegments,
    /// A segment has invalid bounds or sizes.
    BadSegment { index: usize },
    /// Two segments overlap.
    OverlappingSegments,
    /// Entry point is not inside an executable segment.
    BadEntry,
    /// Physical range overflows or is implausibly low.
    BadImageRange,
}

/// Parses `bytes` as a kernel ELF image.
///
/// The parser rejects anything it does not fully understand. On success, every
/// returned segment is bounded by `bytes` and lies in page-aligned physical
/// memory.
pub fn parse(bytes: &[u8]) -> Result<Image, ParseError> {
    if bytes.len() < ELF_HEADER_SIZE {
        return Err(ParseError::TooShort);
    }
    if &bytes[0..4] != b"\x7fELF" {
        return Err(ParseError::BadMagic);
    }
    if bytes[4] != 2 || bytes[5] != 1 || bytes[6] != 1 {
        return Err(ParseError::BadClass);
    }
    if read_u16(bytes, 20) != Some(1) {
        return Err(ParseError::BadVersion);
    }
    if read_u16(bytes, 16) != Some(ET_EXEC) {
        return Err(ParseError::NotExecutable);
    }
    if read_u16(bytes, 18) != Some(EM_X86_64) {
        return Err(ParseError::WrongMachine);
    }
    if read_u16(bytes, 52) != Some(ELF_HEADER_SIZE as u16)
        || read_u16(bytes, 54) != Some(PROGRAM_HEADER_SIZE as u16)
    {
        return Err(ParseError::BadHeader);
    }

    let entry = read_u64(bytes, 24).ok_or(ParseError::BadHeader)?;
    let program_offset = read_u64(bytes, 32).ok_or(ParseError::BadHeader)?;
    let program_count = read_u16(bytes, 56).ok_or(ParseError::BadHeader)? as usize;
    if program_count == 0 {
        return Err(ParseError::NoSegments);
    }
    if program_count > MAX_SEGMENTS {
        return Err(ParseError::TooManySegments);
    }

    let table_len = (program_count as u64)
        .checked_mul(PROGRAM_HEADER_SIZE as u64)
        .ok_or(ParseError::BadProgramHeaders)?;
    let table_end = program_offset
        .checked_add(table_len)
        .ok_or(ParseError::BadProgramHeaders)?;
    if table_end > bytes.len() as u64 {
        return Err(ParseError::BadProgramHeaders);
    }

    let mut segments = [Segment {
        file_offset: 0,
        file_size: 0,
        memory_size: 0,
        physical_address: 0,
        flags: 0,
    }; MAX_SEGMENTS];
    let mut count = 0usize;
    let mut start = u64::MAX;
    let mut end = 0u64;

    for index in 0..program_count {
        let offset = table_end - table_len + (index as u64) * PROGRAM_HEADER_SIZE as u64;
        let offset = offset as usize;
        let header = &bytes[offset..offset + PROGRAM_HEADER_SIZE];
        if read_u32(header, 0) != Some(PT_LOAD) {
            continue;
        }
        if count == MAX_SEGMENTS {
            return Err(ParseError::TooManySegments);
        }

        let flags = read_u32(header, 4).ok_or(ParseError::BadSegment { index })?;
        let file_offset = read_u64(header, 8).ok_or(ParseError::BadSegment { index })?;
        let virtual_address = read_u64(header, 16).ok_or(ParseError::BadSegment { index })?;
        let physical_address = read_u64(header, 24).ok_or(ParseError::BadSegment { index })?;
        let file_size = read_u64(header, 32).ok_or(ParseError::BadSegment { index })?;
        let memory_size = read_u64(header, 40).ok_or(ParseError::BadSegment { index })?;
        let alignment = read_u64(header, 48).ok_or(ParseError::BadSegment { index })?;

        let file_end = file_offset
            .checked_add(file_size)
            .ok_or(ParseError::BadSegment { index })?;
        let memory_end = physical_address
            .checked_add(memory_size)
            .ok_or(ParseError::BadSegment { index })?;
        if file_size > memory_size
            || memory_size == 0
            || physical_address < PAGE_SIZE
            || file_end > bytes.len() as u64
            || memory_end <= physical_address
            || virtual_address != physical_address
        {
            return Err(ParseError::BadSegment { index });
        }
        if alignment > 1
            && (!alignment.is_power_of_two()
                || file_offset % alignment != physical_address % alignment)
        {
            return Err(ParseError::BadSegment { index });
        }

        let segment = Segment {
            file_offset,
            file_size,
            memory_size,
            physical_address,
            flags,
        };
        // Reject overlaps instead of letting program-header order decide which
        // bytes win.
        for previous in &segments[..count] {
            let previous_end = previous.end().ok_or(ParseError::BadSegment { index })?;
            if physical_address < previous_end && previous.physical_address < memory_end {
                return Err(ParseError::OverlappingSegments);
            }
        }

        let aligned_start =
            align_down(physical_address, PAGE_SIZE).ok_or(ParseError::BadImageRange)?;
        let aligned_end = align_up(memory_end, PAGE_SIZE).ok_or(ParseError::BadImageRange)?;
        start = start.min(aligned_start);
        end = end.max(aligned_end);
        segments[count] = segment;
        count += 1;
    }

    if count == 0 || start == u64::MAX || end <= start {
        return Err(ParseError::NoSegments);
    }

    // The entry must execute from a loaded segment.
    let entry_in_image = segments[..count].iter().any(|segment| {
        let end = segment.physical_address.saturating_add(segment.file_size);
        entry >= segment.physical_address && entry < end
    });
    if !entry_in_image {
        return Err(ParseError::BadEntry);
    }

    Ok(Image {
        segments,
        segment_count: count,
        entry,
        start,
        end,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let raw = bytes.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

#[cfg(test)]
mod tests;
