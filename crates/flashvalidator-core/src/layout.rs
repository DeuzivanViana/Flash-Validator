//! Where the test regions are, and in what order they are visited.

use crate::rng::Xorshift128;

/// Default number of regions.
pub const DEFAULT_REGIONS: usize = 576;

/// Default transfer size per region, in bytes.
pub const DEFAULT_BLOCK_SIZE: usize = 4096;

/// Offsets are always aligned down to this boundary.
pub const OFFSET_ALIGN: u64 = 4096;

/// Byte offset of `region` (`0..regions`) on a device of `device_size` bytes,
/// for transfers of `block_size` bytes.
///
/// Regions are spread evenly over the declared capacity, using exact
/// integer arithmetic:
///
/// ```text
/// span   = device_sectors - block_size / 512
/// offset = round(region * span / (regions - 1)) * 512, aligned down to 4 KiB
/// ```
///
/// Region 0 starts at byte 0. The last region ends at the declared end of
/// the device, or less than 4 KiB before it.
///
/// # Panics
///
/// Panics if `regions < 2`, `region >= regions`, or the device is smaller
/// than one block.
pub fn region_offset(region: usize, regions: usize, device_size: u64, block_size: usize) -> u64 {
    assert!(regions >= 2, "need at least two regions");
    assert!(region < regions, "region index out of range");
    let sectors = device_size / 512;
    let block_sectors = (block_size as u64).div_ceil(512);
    assert!(sectors >= block_sectors, "device smaller than one block");
    let span = u128::from(sectors - block_sectors);
    let den = (regions - 1) as u128;
    // round-half-up of region * span / den
    let off_sectors = (region as u128 * span * 2 + den) / (2 * den);
    let bytes = (off_sectors as u64) * 512;
    bytes & !(OFFSET_ALIGN - 1)
}

/// All region offsets for a device.
pub fn region_offsets(regions: usize, device_size: u64, block_size: usize) -> Vec<u64> {
    (0..regions).map(|r| region_offset(r, regions, device_size, block_size)).collect()
}

/// Order in which regions are tested.
///
/// The last region comes first (the end of the claimed capacity, where
/// counterfeit drives fail), then region 0. The rest follow in uniformly
/// random order, so the drive cannot predict which address comes next.
pub fn test_order(regions: usize, rng: &mut Xorshift128) -> Vec<usize> {
    let mut order = Vec::with_capacity(regions);
    if regions == 0 {
        return order;
    }
    order.push(regions - 1);
    if regions == 1 {
        return order;
    }
    order.push(0);
    let mut rest: Vec<usize> = (1..regions - 1).collect();
    // Fisher-Yates shuffle.
    for i in (1..rest.len()).rev() {
        let j = rng.below(i + 1);
        rest.swap(i, j);
    }
    order.extend(rest);
    order
}

/// Whether the device is large enough for `regions` non-overlapping blocks.
pub fn fits(device_size: u64, regions: usize, block_size: usize) -> bool {
    device_size / block_size as u64 >= regions as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    #[test]
    fn first_and_last_region() {
        let size = 32 * GB;
        assert_eq!(region_offset(0, 576, size, 4096), 0);
        let last = region_offset(575, 576, size, 4096);
        assert!(last + 4096 <= size);
        assert!(size - (last + 4096) < 4096 + 512, "last region should touch the end");
    }

    #[test]
    fn offsets_are_aligned_monotonic_and_distinct() {
        for size in [64 * 1024 * 1024, 7 * GB + 12345 * 512, 2_000 * GB] {
            let offs = region_offsets(576, size, 4096);
            assert!(offs.iter().all(|o| o % 4096 == 0));
            assert!(offs.windows(2).all(|w| w[0] < w[1]));
            assert!(*offs.last().unwrap() + 4096 <= size);
        }
    }

    #[test]
    fn matches_floating_point_reference() {
        // The exact integer formula must agree with a straightforward f64 version.
        let size = 15_931_539_456u64; // a typical "16 GB" stick
        let sectors = size / 512;
        let span = (sectors - 8) as f64;
        for r in [1usize, 17, 288, 400, 574] {
            let expect = (((r as f64 / 575.0) * span).round() as u64 * 512) & !0xFFF;
            assert_eq!(region_offset(r, 576, size, 4096), expect, "region {r}");
        }
    }

    #[test]
    fn order_is_a_permutation_starting_with_last_then_first() {
        let mut rng = Xorshift128::from_seed(1);
        let order = test_order(576, &mut rng);
        assert_eq!(order[0], 575);
        assert_eq!(order[1], 0);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..576).collect::<Vec<_>>());
    }

    #[test]
    fn fits_checks_capacity() {
        assert!(fits(576 * 4096, 576, 4096));
        assert!(!fits(575 * 4096, 576, 4096));
    }
}
