use super::{CAPACITY, Entry};

const PAGE_BUCKETS: usize = 512;
const WIDE: usize = PAGE_BUCKETS;
const VACANT: u16 = u16::MAX;

/// Indexes candidate slots. The entries still own translation and recency.
pub(super) struct LookupIndex {
    buckets: [u64; PAGE_BUCKETS + 1],
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    occupied: u64,
    bucket_by_slot: [u16; CAPACITY],
}

impl Default for LookupIndex {
    fn default() -> Self {
        Self {
            buckets: [0; PAGE_BUCKETS + 1],
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            occupied: 0,
            bucket_by_slot: [VACANT; CAPACITY],
        }
    }
}

impl LookupIndex {
    pub(super) fn candidates(&self, address: u64) -> u64 {
        self.buckets[bucket(address)] | self.buckets[WIDE]
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(super) fn only_4k_leaves(&self) -> bool {
        self.buckets[WIDE] == 0
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(super) fn occupancy(&self) -> u64 {
        self.occupied
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(super) fn first_vacancy(&self, capacity: usize) -> Option<usize> {
        let slot = (!self.occupied).trailing_zeros() as usize;
        (slot < capacity).then_some(slot)
    }

    pub(super) fn insert(&mut self, index: usize, entry: &Entry) {
        self.remove(index);
        let translation = entry.translation;
        let bucket = if translation.span == 4096 && translation.linear_base & 4095 == 0 {
            bucket(translation.linear_base)
        } else {
            WIDE
        };
        self.buckets[bucket] |= 1u64 << index;
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        {
            self.occupied |= 1u64 << index;
        }
        self.bucket_by_slot[index] = bucket as u16;
    }

    pub(super) fn remove(&mut self, index: usize) {
        let old = self.bucket_by_slot[index];
        if old != VACANT {
            self.buckets[usize::from(old)] &= !(1u64 << index);
            self.bucket_by_slot[index] = VACANT;
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            {
                self.occupied &= !(1u64 << index);
            }
        }
    }
}

fn bucket(address: u64) -> usize {
    ((address >> 12).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - PAGE_BUCKETS.ilog2())) as usize
}
