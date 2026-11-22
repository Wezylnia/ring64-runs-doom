use super::{CAPACITY, Entries};

const NONE: u8 = CAPACITY as u8;

#[derive(Clone, Copy)]
struct Neighbors {
    older: u8,
    newer: u8,
}

const UNLINKED: Neighbors = Neighbors {
    older: NONE,
    newer: NONE,
};

pub(super) struct Recency {
    neighbors: [Neighbors; CAPACITY],
    oldest: u8,
    newest: u8,
}

impl Default for Recency {
    fn default() -> Self {
        Self {
            neighbors: [UNLINKED; CAPACITY],
            oldest: NONE,
            newest: NONE,
        }
    }
}

impl Recency {
    pub(super) fn oldest(&self) -> Option<usize> {
        (self.oldest != NONE).then_some(usize::from(self.oldest))
    }

    pub(super) fn touch(&mut self, index: usize) {
        let slot = index as u8;
        if slot == self.newest {
            return;
        }
        self.remove(index);
        if self.newest == NONE {
            self.oldest = slot;
        } else {
            self.neighbors[usize::from(self.newest)].newer = slot;
        }
        self.neighbors[index].older = self.newest;
        self.newest = slot;
    }

    fn touch_before_newest(&mut self, index: usize, newest: usize) {
        debug_assert_eq!(self.newest, newest as u8);
        debug_assert_ne!(index, newest);
        if self.neighbors[newest].older == index as u8 {
            return;
        }
        self.remove(index);
        let older = self.neighbors[newest].older;
        if older == NONE {
            self.oldest = index as u8;
        } else {
            self.neighbors[usize::from(older)].newer = index as u8;
        }
        self.neighbors[index] = Neighbors {
            older,
            newer: newest as u8,
        };
        self.neighbors[newest].older = index as u8;
    }

    pub(super) fn remove(&mut self, index: usize) {
        let Neighbors { older, newer } = self.neighbors[index];
        if older == NONE {
            if self.oldest == index as u8 {
                self.oldest = newer;
            }
        } else {
            self.neighbors[usize::from(older)].newer = newer;
        }
        if newer == NONE {
            if self.newest == index as u8 {
                self.newest = older;
            }
        } else {
            self.neighbors[usize::from(newer)].older = older;
        }
        self.neighbors[index] = UNLINKED;
    }
}

/// A lease for the guarded dword reader, whose accesses are a subset of all
/// possible cache operations. Start it only after the first capacity miss.
pub(super) struct GuardedOrder {
    pub(super) order: Recency,
    clock: u64,
    next_id: u64,
    occupied: u64,
}

impl GuardedOrder {
    pub(super) fn from_entries(entries: &Entries) -> Self {
        let mut order = Recency::default();
        let mut occupied = entries.lookup_index.occupancy();
        let mut indices = [0; CAPACITY];
        let mut count = 0;
        while occupied != 0 {
            let index = occupied.trailing_zeros() as usize;
            occupied &= occupied - 1;
            indices[count] = index;
            count += 1;
        }
        indices[..count].sort_unstable_by_key(|&index| entries.slots[index].as_ref().unwrap().used);
        for index in indices[..count].iter().copied() {
            order.touch(index);
        }
        let mut retained = Self {
            order,
            clock: 0,
            next_id: 0,
            occupied: 0,
        };
        retained.record(entries);
        retained
    }

    pub(super) fn matches(&self, entries: &Entries) -> bool {
        self.clock == entries.clock
            && self.next_id == entries.next_id
            && self.occupied == entries.lookup_index.occupancy()
    }

    pub(super) fn record(&mut self, entries: &Entries) {
        self.clock = entries.clock;
        self.next_id = entries.next_id;
        self.occupied = entries.lookup_index.occupancy();
    }

    pub(super) fn touch(&mut self, entries: &Entries, operand: usize, code: usize) {
        self.order.touch_before_newest(operand, code);
        self.record(entries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_order_agrees_with_repeated_slot_removal_and_reuse() {
        let mut recency = Recency::default();
        let mut clock = [None; CAPACITY];
        let mut random = 0x1348_108du32;
        for tick in 1..=2000 {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            let index = (random >> 12) as usize % CAPACITY;
            if (random & 7) < 3 {
                recency.remove(index);
                clock[index] = None;
            } else {
                recency.touch(index);
                clock[index] = Some(tick);
            }
            let reference = clock
                .iter()
                .enumerate()
                .filter_map(|(index, age)| age.map(|age| (age, index)))
                .min()
                .map(|(_, index)| index);
            assert_eq!(recency.oldest(), reference);
        }
    }
}
