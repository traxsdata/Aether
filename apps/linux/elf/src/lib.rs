// SPDX-License-Identifier: Apache-2.0
//! Defensive Linux ELF64 parsing for the Linux ABI loader.
//!
//! The parser accepts only what the loader can map: 64-bit little-endian
//! x86-64 executables and position-independent executables with bounded
//! `PT_LOAD` segments. It records `PT_INTERP` but never executes or trusts the
//! named path; resolving the interpreter is the loader's policy.
//!
//! The structure borrows the input bytes and performs no allocation, so a
//! malformed image cannot force unbounded work in the boot path.

#![no_std]

#[cfg(test)]
extern crate std;

/// ELF magic.
pub const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
/// Maximum accepted program headers.
pub const MAX_SEGMENTS: usize = 32;
/// Maximum interpreter path bytes.
pub const MAX_INTERP: usize = 256;
/// `ET_EXEC`: fixed-address executable.
pub const ET_EXEC: u16 = 2;
/// `ET_DYN`: position-independent executable or shared object.
pub const ET_DYN: u16 = 3;
/// `PT_LOAD`: loadable segment.
pub const PT_LOAD: u32 = 1;
/// `PT_INTERP`: interpreter path.
pub const PT_INTERP: u32 = 3;
/// Segment is executable.
pub const PF_X: u32 = 1;
/// Segment is writable.
pub const PF_W: u32 = 2;
/// Segment is readable.
pub const PF_R: u32 = 4;

const ELF_HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;

/// A validated loadable segment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    /// File offset of the segment bytes.
    pub offset: u64,
    /// Virtual address before the load bias.
    pub vaddr: u64,
    /// Bytes copied from the file.
    pub file_size: u64,
    /// Bytes occupied in memory; the remainder is zero-filled.
    pub memory_size: u64,
    /// ELF segment flags.
    pub flags: u32,
    /// Segment alignment.
    pub align: u64,
}

impl Segment {
    /// Segment is executable.
    #[must_use]
    pub const fn executable(&self) -> bool {
        self.flags & PF_X != 0
    }

    /// Segment is writable.
    #[must_use]
    pub const fn writable(&self) -> bool {
        self.flags & PF_W != 0
    }
}

/// A parsed Linux ELF image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image<'a> {
    bytes: &'a [u8],
    entry: u64,
    image_type: u16,
    program_headers: u64,
    program_header_size: u16,
    program_header_count: u16,
    interpreter: Option<&'a str>,
    segments: [Segment; MAX_SEGMENTS],
    segment_count: usize,
}

impl<'a> Image<'a> {
    /// Raw image bytes.
    #[must_use]
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Entry point before the load bias.
    #[must_use]
    pub const fn entry(&self) -> u64 {
        self.entry
    }

    /// ELF image type (`ET_EXEC` or `ET_DYN`).
    #[must_use]
    pub const fn image_type(&self) -> u16 {
        self.image_type
    }

    /// Program-header file offset.
    #[must_use]
    pub const fn program_headers(&self) -> u64 {
        self.program_headers
    }

    /// Program-header entry size.
    #[must_use]
    pub const fn program_header_size(&self) -> u16 {
        self.program_header_size
    }

    /// Program-header count.
    #[must_use]
    pub const fn program_header_count(&self) -> u16 {
        self.program_header_count
    }

    /// Requested interpreter path, if any.
    #[must_use]
    pub const fn interpreter(&self) -> Option<&'a str> {
        self.interpreter
    }

    /// Loadable segments.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments[..self.segment_count]
    }

    /// Returns the virtual address of the program-header table after loading.
    ///
    /// The headers normally live inside the first `PT_LOAD` segment. A
    /// malformed table that is not covered by a loadable segment is rejected
    /// during parsing, so this calculation cannot point outside the image.
    #[must_use]
    pub fn program_header_address(&self, bias: u64) -> u64 {
        for segment in self.segments() {
            if self.program_headers >= segment.offset
                && self.program_headers < segment.offset.saturating_add(segment.file_size)
            {
                return bias
                    .saturating_add(segment.vaddr)
                    .saturating_add(self.program_headers - segment.offset);
            }
        }
        0
    }
}

/// ELF parsing failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Image is shorter than the ELF header.
    TooShort,
    /// Magic is not `\x7fELF`.
    BadMagic,
    /// Not a 64-bit little-endian image.
    BadClass,
    /// Unsupported ELF version.
    BadVersion,
    /// Unsupported image type.
    BadType,
    /// Wrong machine architecture.
    WrongMachine,
    /// Header or program-header sizes are inconsistent.
    BadHeader,
    /// Program-header table is out of bounds.
    BadProgramHeaders,
    /// No loadable segment.
    NoSegments,
    /// More loadable segments than [`MAX_SEGMENTS`].
    TooManySegments,
    /// A segment has invalid bounds.
    BadSegment,
    /// Two segments overlap in memory.
    OverlappingSegments,
    /// A writable and executable segment would violate W^X.
    WritableExecutable,
    /// The interpreter path is empty, too long, or not a normal absolute path.
    BadInterpreter,
    /// The entry point is not inside an executable segment (fixed images only).
    BadEntry,
    /// The program-header table is not covered by a loadable segment.
    UnmappedProgramHeaders,
}

/// Parses a Linux ELF64 image.
pub fn parse(bytes: &[u8]) -> Result<Image<'_>, ParseError> {
    if bytes.len() < ELF_HEADER_SIZE {
        return Err(ParseError::TooShort);
    }
    if bytes[..4] != ELF_MAGIC {
        return Err(ParseError::BadMagic);
    }
    if bytes[4] != 2 || bytes[5] != 1 || bytes[6] != 1 {
        return Err(ParseError::BadClass);
    }
    let image_type = read_u16(bytes, 16)?;
    if image_type != ET_EXEC && image_type != ET_DYN {
        return Err(ParseError::BadType);
    }
    if read_u16(bytes, 18)? != 62 {
        return Err(ParseError::WrongMachine);
    }
    let entry = read_u64(bytes, 24)?;
    let program_headers = read_u64(bytes, 32)?;
    let header_size = read_u16(bytes, 54)?;
    let header_count = read_u16(bytes, 56)?;
    if header_size as usize != PROGRAM_HEADER_SIZE || header_count == 0 {
        return Err(ParseError::BadHeader);
    }

    let table_size = (header_count as u64)
        .checked_mul(header_size as u64)
        .ok_or(ParseError::BadProgramHeaders)?;
    let table_end = program_headers
        .checked_add(table_size)
        .ok_or(ParseError::BadProgramHeaders)?;
    if table_end > bytes.len() as u64 {
        return Err(ParseError::BadProgramHeaders);
    }

    let mut segments = [Segment::default(); MAX_SEGMENTS];
    let mut segment_count = 0usize;
    let mut interpreter = None;
    for index in 0..header_count as u64 {
        let offset = program_headers + index * header_size as u64;
        let offset = usize::try_from(offset).map_err(|_| ParseError::BadProgramHeaders)?;
        let kind = read_u32(bytes, offset)?;
        match kind {
            PT_LOAD => {
                if segment_count == MAX_SEGMENTS {
                    return Err(ParseError::TooManySegments);
                }
                let file_offset = read_u64(bytes, offset + 8)?;
                let vaddr = read_u64(bytes, offset + 16)?;
                let file_size = read_u64(bytes, offset + 32)?;
                let memory_size = read_u64(bytes, offset + 40)?;
                let flags = read_u32(bytes, offset + 4)?;
                let align = read_u64(bytes, offset + 48)?;
                let file_end = file_offset
                    .checked_add(file_size)
                    .ok_or(ParseError::BadSegment)?;
                if file_size > memory_size
                    || file_end > bytes.len() as u64
                    || memory_size == 0
                    || align > 0 && !align.is_power_of_two()
                {
                    return Err(ParseError::BadSegment);
                }
                if flags & (PF_W | PF_X) == (PF_W | PF_X) {
                    return Err(ParseError::WritableExecutable);
                }
                segments[segment_count] = Segment {
                    offset: file_offset,
                    vaddr,
                    file_size,
                    memory_size,
                    flags,
                    align,
                };
                segment_count += 1;
            }
            PT_INTERP => {
                let file_offset = read_u64(bytes, offset + 8)?;
                let file_size = read_u64(bytes, offset + 32)?;
                if file_size == 0 || file_size as usize > MAX_INTERP {
                    return Err(ParseError::BadInterpreter);
                }
                let start = usize::try_from(file_offset).map_err(|_| ParseError::BadInterpreter)?;
                let end = start
                    .checked_add(file_size as usize)
                    .ok_or(ParseError::BadInterpreter)?;
                let raw = bytes.get(start..end).ok_or(ParseError::BadInterpreter)?;
                let path = raw.strip_suffix(&[0]).ok_or(ParseError::BadInterpreter)?;
                let path = core::str::from_utf8(path).map_err(|_| ParseError::BadInterpreter)?;
                if !path.starts_with('/')
                    || path.contains('\0')
                    || path.split('/').any(|part| part == "." || part == "..")
                {
                    return Err(ParseError::BadInterpreter);
                }
                interpreter = Some(path);
            }
            _ => {}
        }
    }
    if segment_count == 0 {
        return Err(ParseError::NoSegments);
    }

    for (index, segment) in segments[..segment_count].iter().enumerate() {
        for other in segments[..index].iter() {
            if segment.vaddr < other.vaddr.saturating_add(other.memory_size)
                && other.vaddr < segment.vaddr.saturating_add(segment.memory_size)
            {
                return Err(ParseError::OverlappingSegments);
            }
        }
    }

    if image_type == ET_EXEC
        && !segments[..segment_count].iter().any(|segment| {
            segment.executable()
                && entry >= segment.vaddr
                && entry < segment.vaddr.saturating_add(segment.memory_size)
        })
    {
        return Err(ParseError::BadEntry);
    }
    if !segments[..segment_count].iter().any(|segment| {
        program_headers >= segment.offset
            && program_headers < segment.offset.saturating_add(segment.file_size)
    }) {
        return Err(ParseError::UnmappedProgramHeaders);
    }

    Ok(Image {
        bytes,
        entry,
        image_type,
        program_headers,
        program_header_size: header_size,
        program_header_count: header_count,
        interpreter,
        segments,
        segment_count,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ParseError> {
    let raw = bytes.get(offset..offset + 2).ok_or(ParseError::BadHeader)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ParseError> {
    let raw = bytes.get(offset..offset + 4).ok_or(ParseError::BadHeader)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ParseError> {
    let raw = bytes.get(offset..offset + 8).ok_or(ParseError::BadHeader)?;
    Ok(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_image(image_type: u16, interp: Option<&str>) -> std::vec::Vec<u8> {
        let mut bytes = std::vec![0u8; 0x1000];
        bytes[..4].copy_from_slice(&ELF_MAGIC);
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 1;
        bytes[16..18].copy_from_slice(&image_type.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x100u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[54..56].copy_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes());
        bytes[56..58].copy_from_slice(&2u16.to_le_bytes());
        // PT_LOAD covering the headers.
        let load = 64;
        bytes[load..load + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        bytes[load + 4..load + 8].copy_from_slice(&(PF_R | PF_X).to_le_bytes());
        bytes[load + 16..load + 24].copy_from_slice(&0u64.to_le_bytes());
        bytes[load + 32..load + 40].copy_from_slice(&0x400u64.to_le_bytes());
        bytes[load + 40..load + 48].copy_from_slice(&0x400u64.to_le_bytes());
        bytes[load + 48..load + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        if let Some(path) = interp {
            let header = 64 + PROGRAM_HEADER_SIZE;
            let path_offset = 0x800u64;
            bytes[header..header + 4].copy_from_slice(&PT_INTERP.to_le_bytes());
            bytes[header + 8..header + 16].copy_from_slice(&path_offset.to_le_bytes());
            bytes[header + 32..header + 40]
                .copy_from_slice(&((path.len() + 1) as u64).to_le_bytes());
            bytes[path_offset as usize..path_offset as usize + path.len()]
                .copy_from_slice(path.as_bytes());
        } else {
            // Remove the second header by marking it as PT_NULL.
            bytes[64 + PROGRAM_HEADER_SIZE..64 + PROGRAM_HEADER_SIZE + 4]
                .copy_from_slice(&0u32.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn parses_fixed_and_pie_images() {
        for image_type in [ET_EXEC, ET_DYN] {
            let bytes = build_image(image_type, Some("/lib/ld-aether.so"));
            let image = parse(&bytes).unwrap();
            assert_eq!(image.image_type(), image_type);
            assert_eq!(image.entry(), 0x100);
            assert_eq!(image.interpreter(), Some("/lib/ld-aether.so"));
            assert_eq!(image.segments().len(), 1);
            assert_eq!(image.program_header_address(0), 64);
        }
    }

    #[test]
    fn rejects_writable_executable_and_bad_interpreter() {
        let mut bytes = build_image(ET_DYN, Some("/lib/ld-aether.so"));
        bytes[68..72].copy_from_slice(&(PF_R | PF_W | PF_X).to_le_bytes());
        assert_eq!(parse(&bytes), Err(ParseError::WritableExecutable));

        let bytes = build_image(ET_DYN, Some("lib/ld-aether.so"));
        assert_eq!(parse(&bytes), Err(ParseError::BadInterpreter));
    }

    #[test]
    fn rejects_unmapped_program_headers_and_overlap() {
        let mut bytes = build_image(ET_DYN, None);
        bytes[32..40].copy_from_slice(&0x2000u64.to_le_bytes());
        assert_eq!(parse(&bytes), Err(ParseError::BadProgramHeaders));

        let mut bytes = build_image(ET_DYN, None);
        bytes[56..58].copy_from_slice(&2u16.to_le_bytes());
        let second = 64 + PROGRAM_HEADER_SIZE;
        bytes[second..second + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        bytes[second + 4..second + 8].copy_from_slice(&PF_R.to_le_bytes());
        bytes[second + 16..second + 24].copy_from_slice(&0x200u64.to_le_bytes());
        bytes[second + 32..second + 40].copy_from_slice(&0x200u64.to_le_bytes());
        bytes[second + 40..second + 48].copy_from_slice(&0x200u64.to_le_bytes());
        assert_eq!(parse(&bytes), Err(ParseError::OverlappingSegments));
    }
}
