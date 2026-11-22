use super::{CAPACITY, Entries, Entry, Request, State, family};

const WIDE: usize = CAPACITY;

/// Candidate slots for the guarded dword reader. The normal cache owns all
/// translations and recency, including hits found through this index.
pub(super) struct Candidates {
    next_id: u64,
    buckets: [u64; CAPACITY + 1],
    bucket_by_slot: [Option<u8>; CAPACITY],
}

impl Default for Candidates {
    fn default() -> Self {
        Self {
            next_id: 0,
            buckets: [0; CAPACITY + 1],
            bucket_by_slot: [None; CAPACITY],
        }
    }
}

impl Candidates {
    pub(super) fn find(
        &mut self,
        entries: &Entries,
        state: &State,
        request: Request,
    ) -> Option<usize> {
        let family = family(state);
        let pcid = state.pcid();
        let matches = |entry: &Entry| {
            entry.family == family
                && (entry.global || entry.pcid == pcid)
                && entry.translation.contains(request.linear)
        };
        if let Some(index) = entries.hint
            && let Some(entry) = entries.slots[index].as_ref()
            && !entry.global
            && matches(entry)
        {
            return Some(index);
        }
        if self.next_id != entries.next_id {
            self.refresh(entries);
        }
        let mut candidates = self.buckets[bucket(request.linear)] | self.buckets[WIDE];
        let mut global = None;
        while candidates != 0 {
            let index = candidates.trailing_zeros() as usize;
            candidates &= candidates - 1;
            // Removal can leave a stale bit. Other fills advance next_id
            // and rebuild before a candidate could be missed.
            if let Some(entry) = entries.slots[index].as_ref()
                && matches(entry)
            {
                if !entry.global {
                    return Some(index);
                }
                global.get_or_insert(index);
            }
        }
        global
    }

    #[inline(never)]
    fn refresh(&mut self, entries: &Entries) {
        self.buckets.fill(0);
        self.bucket_by_slot.fill(None);
        for (index, slot) in entries.slots[..entries.capacity].iter().enumerate() {
            if let Some(entry) = slot {
                self.update_slot(index, entry);
            }
        }
        self.next_id = entries.next_id;
    }

    pub(super) fn observe_fill(&mut self, entries: &Entries, index: usize) {
        self.update_slot(index, entries.slots[index].as_ref().unwrap());
        self.next_id = entries.next_id;
    }

    fn update_slot(&mut self, index: usize, entry: &Entry) {
        let bit = 1u64 << index;
        if let Some(old) = self.bucket_by_slot[index] {
            self.buckets[old as usize] &= !bit;
        }
        let translation = entry.translation;
        let bucket = if translation.span == 4096 && translation.linear_base & 4095 == 0 {
            bucket(translation.linear_base)
        } else {
            WIDE
        };
        self.buckets[bucket] |= bit;
        self.bucket_by_slot[index] = Some(bucket as u8);
    }
}

fn bucket(address: u64) -> usize {
    ((address >> 12).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - CAPACITY.ilog2())) as usize
}
