#[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
mod image;
#[cfg(test)]
mod tests;

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
pub(crate) use image::SharedDword;
#[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
pub(crate) use image::{SharedBacking, SharedExclusive, SharedHeld, SharedLayout};

// A generated store and its checked fallback can each spend one pass.
pub(crate) const CAS_ATTEMPTS: usize = 8;

trait Lanes {
    fn load(&self, lane: u32) -> u64;
    fn store(&self, lane: u32, value: u64);
    fn compare_exchange(&self, lane: u32, current: u64, next: u64) -> u64;
}

const fn lane(byte: u32) -> u32 {
    byte & !7
}

const fn shift(byte: u32) -> u32 {
    (byte & 7) * 8
}

fn each_lane(offsets: &[u32]) -> impl Iterator<Item = u32> {
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

fn gather(ram: &impl Lanes, offsets: &[u32], output: &mut [u8]) {
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

fn word_patch(base: u32, offsets: &[u32], values: &[u8]) -> (u64, u64) {
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

fn scatter(ram: &impl Lanes, offsets: &[u32], bytes: &[u8], guarded: bool) -> bool {
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
