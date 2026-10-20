// SPDX-License-Identifier: Apache-2.0
//! Host tests for firmware memory-map normalisation.

use super::*;
use aether_abi::MemoryKind;

fn region(start: u64, pages: u64, kind: MemoryKind) -> MemoryRegion {
    MemoryRegion::new(start, pages * 4096, kind)
}

#[test]
fn merges_unsorted_contiguous_regions() {
    let mut regions = [
        region(0x5000, 2, MemoryKind::Usable),
        region(0x1000, 2, MemoryKind::Usable),
        region(0x3000, 2, MemoryKind::Usable),
    ];

    let count = normalize_regions(&mut regions);

    assert_eq!(count, 1);
    assert_eq!(regions[0], region(0x1000, 6, MemoryKind::Usable));
}

#[test]
fn sorts_without_merging_different_kinds() {
    let mut regions = [
        region(0x3000, 1, MemoryKind::BootServices),
        region(0x1000, 1, MemoryKind::Usable),
        region(0x2000, 1, MemoryKind::Reserved),
    ];

    let count = normalize_regions(&mut regions);

    assert_eq!(count, 3);
    assert_eq!(regions[0], region(0x1000, 1, MemoryKind::Usable));
    assert_eq!(regions[1], region(0x2000, 1, MemoryKind::Reserved));
    assert_eq!(regions[2], region(0x3000, 1, MemoryKind::BootServices));
}

#[test]
fn keeps_non_contiguous_regions_separate() {
    let mut regions = [
        region(0x5000, 1, MemoryKind::Usable),
        region(0x1000, 1, MemoryKind::Usable),
    ];

    let count = normalize_regions(&mut regions);

    assert_eq!(count, 2);
    assert_eq!(regions[0], region(0x1000, 1, MemoryKind::Usable));
    assert_eq!(regions[1], region(0x5000, 1, MemoryKind::Usable));
}

#[test]
fn leaves_overlaps_for_abi_validation() {
    let mut regions = [
        region(0x2000, 2, MemoryKind::Usable),
        region(0x1000, 2, MemoryKind::Usable),
    ];

    let count = normalize_regions(&mut regions);

    assert_eq!(count, 2);
    assert_eq!(regions[0], region(0x1000, 2, MemoryKind::Usable));
    assert_eq!(regions[1], region(0x2000, 2, MemoryKind::Usable));
}

#[test]
fn empty_input_stays_empty() {
    let mut regions: [MemoryRegion; 0] = [];
    assert_eq!(normalize_regions(&mut regions), 0);
}
