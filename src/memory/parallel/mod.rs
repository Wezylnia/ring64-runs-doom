#[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
mod image;
#[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
mod ram;
#[cfg(test)]
mod tests;

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
pub(crate) use image::SharedDword;
#[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
pub(crate) use ram::{SharedBacking, SharedExclusive, SharedHeld, SharedLayout};
pub(crate) type SharedOffset = super::image::BackingOffset;

#[cfg(feature = "parallel")]
pub(crate) const SHARED_RAM_MAX_PAGES: u64 = if cfg!(parallel_memory64) {
    262_144
} else {
    65_536
};

#[cfg(feature = "parallel")]
fn direct_offset(offset: SharedOffset) -> Option<u32> {
    #[cfg(parallel_memory64)]
    {
        u32::try_from(offset).ok()
    }
    #[cfg(not(parallel_memory64))]
    {
        Some(offset)
    }
}

// A generated store and its checked fallback can each spend one pass.
pub(crate) const CAS_ATTEMPTS: usize = 8;

trait Lanes {
    fn load(&self, lane: SharedOffset) -> u64;
    fn store(&self, lane: SharedOffset, value: u64);
    fn compare_exchange(&self, lane: SharedOffset, current: u64, next: u64) -> u64;
}

const fn lane(byte: SharedOffset) -> SharedOffset {
    byte & !7
}

const fn shift(byte: SharedOffset) -> u32 {
    #[cfg(parallel_memory64)]
    let byte = byte as u32;
    (byte & 7) * 8
}

fn each_lane(offsets: &[SharedOffset]) -> impl Iterator<Item = SharedOffset> {
    offsets
        .iter()
        .copied()
        .map(lane)
        .enumerate()
        .filter(|(index, base)| {
            !offsets[..*index]
                .iter()
                .any(|offset| lane(*offset) == *base)
        })
        .map(|(_, base)| base)
}

fn gather(ram: &impl Lanes, offsets: &[SharedOffset], output: &mut [u8]) {
    assert_eq!(offsets.len(), output.len());
    for base in each_lane(offsets) {
        let value = ram.load(base);
        for (&offset, byte) in offsets.iter().zip(output.iter_mut()) {
            if lane(offset) == base {
                *byte = (value >> shift(offset)) as u8;
            }
        }
    }
}

fn word_patch(base: SharedOffset, offsets: &[SharedOffset], values: &[u8]) -> (u64, u64) {
    let (mut mask, mut value) = (0, 0);
    for (&offset, &byte) in offsets.iter().zip(values) {
        if lane(offset) == base {
            let shift = shift(offset);
            let field = 0xff << shift;
            mask |= field;
            value = (value & !field) | (u64::from(byte) << shift);
        }
    }
    (mask, value)
}

fn scatter(ram: &impl Lanes, offsets: &[SharedOffset], bytes: &[u8], guarded: bool) -> bool {
    assert_eq!(offsets.len(), bytes.len());
    debug_assert!(guarded || each_lane(offsets).take(2).count() <= 1);
    for base in each_lane(offsets) {
        let (mask, value) = word_patch(base, offsets, bytes);
        if mask == u64::MAX {
            ram.store(base, value);
            continue;
        }
        let mut current = ram.load(base);
        if guarded {
            ram.store(base, current & !mask | value);
            continue;
        }
        let mut published = false;
        for _ in 0..CAS_ATTEMPTS {
            let next = current & !mask | value;
            let observed = ram.compare_exchange(base, current, next);
            if observed == current {
                published = true;
                break;
            }
            current = observed;
        }
        if !published {
            return false;
        }
    }
    true
}
