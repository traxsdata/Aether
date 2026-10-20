// SPDX-License-Identifier: Apache-2.0
//! Host tests for the kernel ELF parser.

use super::*;

const BASE: u64 = 0x40_0000;

/// Builds a minimal ELF64 image with one or more loadable segments.
fn image(segments: &[(u64, &[u8], u64)]) -> Vec<u8> {
    let mut bytes = vec![0u8; ELF_HEADER_SIZE + segments.len() * PROGRAM_HEADER_SIZE];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    write_u16(&mut bytes, 16, ET_EXEC);
    write_u16(&mut bytes, 18, EM_X86_64);
    write_u32(&mut bytes, 20, 1);
    write_u64(&mut bytes, 24, BASE);
    write_u64(&mut bytes, 32, ELF_HEADER_SIZE as u64);
    write_u16(&mut bytes, 52, ELF_HEADER_SIZE as u16);
    write_u16(&mut bytes, 54, PROGRAM_HEADER_SIZE as u16);
    write_u16(&mut bytes, 56, segments.len() as u16);

    for (index, (address, contents, memory_size)) in segments.iter().enumerate() {
        // Real linkers page-align segment file offsets and addresses.
        while !bytes.len().is_multiple_of(PAGE_SIZE as usize) {
            bytes.push(0);
        }
        let file_offset = bytes.len() as u64;
        bytes.extend_from_slice(contents);
        let header = ELF_HEADER_SIZE + index * PROGRAM_HEADER_SIZE;
        write_u32(&mut bytes, header, PT_LOAD);
        write_u32(&mut bytes, header + 4, PF_R | PF_W | PF_X);
        write_u64(&mut bytes, header + 8, file_offset);
        write_u64(&mut bytes, header + 16, *address);
        write_u64(&mut bytes, header + 24, *address);
        write_u64(&mut bytes, header + 32, contents.len() as u64);
        write_u64(&mut bytes, header + 40, *memory_size);
        write_u64(&mut bytes, header + 48, PAGE_SIZE);
    }
    bytes
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn parses_a_single_loadable_segment() {
    let bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    let parsed = parse(&bytes).expect("valid image");
    assert_eq!(parsed.entry(), BASE);
    assert_eq!(parsed.start(), BASE);
    assert_eq!(parsed.end(), BASE + PAGE_SIZE);
    assert_eq!(parsed.segments().len(), 1);
    assert_eq!(parsed.segments()[0].file_size, 16);
}

#[test]
fn ignores_non_load_segments() {
    let mut bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    write_u32(&mut bytes, ELF_HEADER_SIZE, 4); // PT_NOTE
    assert_eq!(parse(&bytes), Err(ParseError::NoSegments));
}

#[test]
fn rejects_a_truncated_header() {
    let bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    assert_eq!(parse(&bytes[..32]), Err(ParseError::TooShort));
}

#[test]
fn rejects_bad_magic_and_class() {
    let mut bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    bytes[0] = b'X';
    assert_eq!(parse(&bytes), Err(ParseError::BadMagic));

    let mut bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    bytes[4] = 1;
    assert_eq!(parse(&bytes), Err(ParseError::BadClass));
}

#[test]
fn rejects_wrong_machine() {
    let mut bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    write_u16(&mut bytes, 18, 3);
    assert_eq!(parse(&bytes), Err(ParseError::WrongMachine));
}

#[test]
fn rejects_out_of_bounds_file_data() {
    let mut bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    write_u64(&mut bytes, ELF_HEADER_SIZE + 32, u64::MAX);
    assert_eq!(parse(&bytes), Err(ParseError::BadSegment { index: 0 }));
}

#[test]
fn rejects_file_size_larger_than_memory_size() {
    let mut bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    write_u64(&mut bytes, ELF_HEADER_SIZE + 40, 8);
    assert_eq!(parse(&bytes), Err(ParseError::BadSegment { index: 0 }));
}

#[test]
fn rejects_low_physical_addresses() {
    let bytes = image(&[(0, &[0x90; 16], PAGE_SIZE)]);
    assert_eq!(parse(&bytes), Err(ParseError::BadSegment { index: 0 }));
}

#[test]
fn rejects_overlapping_segments() {
    let bytes = image(&[
        (BASE, &[0x90; 16], PAGE_SIZE * 2),
        (BASE + PAGE_SIZE, &[0x90; 16], PAGE_SIZE),
    ]);
    assert_eq!(parse(&bytes), Err(ParseError::OverlappingSegments));
}

#[test]
fn rejects_entry_in_a_non_executable_segment() {
    let mut bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    write_u32(&mut bytes, ELF_HEADER_SIZE + 4, PF_R | PF_W);
    assert_eq!(parse(&bytes), Err(ParseError::BadEntry));
}

#[test]
fn rejects_entry_outside_image() {
    let mut bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    write_u64(&mut bytes, 24, BASE + PAGE_SIZE * 4);
    assert_eq!(parse(&bytes), Err(ParseError::BadEntry));
}

#[test]
fn limits_segment_count() {
    let segments: Vec<_> = (0..=MAX_SEGMENTS)
        .map(|index| {
            (
                BASE + index as u64 * PAGE_SIZE * 2,
                &[0x90u8; 8][..],
                PAGE_SIZE,
            )
        })
        .collect();
    let bytes = image(&segments);
    assert_eq!(parse(&bytes), Err(ParseError::TooManySegments));
}

#[test]
fn rejects_misaligned_file_offsets() {
    let mut bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE)]);
    let offset = ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE;
    bytes.insert(offset, 0);
    write_u64(&mut bytes, ELF_HEADER_SIZE + 8, offset as u64 + 1);
    assert_eq!(parse(&bytes), Err(ParseError::BadSegment { index: 0 }));
}

#[test]
fn accepts_bss_after_file_data() {
    let bytes = image(&[(BASE, &[0x90; 16], PAGE_SIZE * 2)]);
    let parsed = parse(&bytes).expect("valid image");
    assert_eq!(parsed.end(), BASE + PAGE_SIZE * 2);
}
