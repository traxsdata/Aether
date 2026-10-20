// SPDX-License-Identifier: Apache-2.0
//! Firmware memory-map normalisation.
//!
//! UEFI firmware is expected to return an ascending memory map, but not every
//! implementation does. The hand-off ABI requires sorted, non-overlapping
//! regions, so the loader sorts defensively before merging adjacent regions
//! of the same kind. Invalid or overlapping descriptors are left in place and
//! rejected by [`aether_abi::BootInfo::validate_regions`].

use aether_abi::MemoryRegion;

/// Sorts `regions` by ascending physical start and merges contiguous regions
/// of the same kind.
///
/// Returns the number of regions after merging. The input is small (bounded by
/// the ABI region limit), so an allocation-free insertion sort is sufficient.
#[must_use]
pub fn normalize_regions(regions: &mut [MemoryRegion]) -> usize {
    // Stable insertion sort: equal start addresses keep firmware order, and
    // overlapping descriptors are left for ABI validation to reject.
    for index in 1..regions.len() {
        let mut position = index;
        while position > 0 && regions[position].start < regions[position - 1].start {
            regions.swap(position, position - 1);
            position -= 1;
        }
    }

    let mut write = 0;
    for read in 0..regions.len() {
        let region = regions[read];
        if write > 0 {
            let previous = &mut regions[write - 1];
            if previous.kind == region.kind
                && previous.end() == Some(region.start)
                && let Some(total) = previous.len.checked_add(region.len)
            {
                previous.len = total;
                continue;
            }
        }
        regions[write] = region;
        write += 1;
    }
    write
}

#[cfg(test)]
mod tests;
