use super::*;
use std::cell::Cell;

#[derive(Default)]
struct Memory {
    lane: Cell<u64>,
    loads: Cell<usize>,
    stores: Cell<usize>,
    compare_exchanges: Cell<usize>,
    pending_neighbor: Cell<bool>,
    reject_every_cas: Cell<bool>,
}

impl Lanes for Memory {
    fn load(&self, lane: u32) -> u64 {
        assert_eq!(lane, 0);
        self.loads.set(self.loads.get() + 1);
        self.lane.get()
    }

    fn store(&self, lane: u32, value: u64) {
        assert_eq!(lane, 0);
        self.stores.set(self.stores.get() + 1);
        self.lane.set(value);
    }

    fn compare_exchange(&self, lane: u32, current: u64, next: u64) -> u64 {
        assert_eq!(lane, 0);
        self.compare_exchanges.set(self.compare_exchanges.get() + 1);
        if self.pending_neighbor.replace(false) {
            self.lane.set(self.lane.get() | 0xdf00);
        }
        if self.reject_every_cas.get() {
            self.lane.set(self.lane.get() ^ 0xff00);
        }
        let found = self.lane.get();
        if found == current {
            self.lane.set(next);
        }
        found
    }
}

#[test]
fn shared_alias_reads_one_lane_and_later_guest_byte_wins() {
    let memory = Memory::default();
    memory.lane.set(0xaabb_ccdd_eeff_9876);
    let offsets = [6, 6, 1];
    let mut bytes = [0; 3];
    gather(&memory, &offsets, &mut bytes);
    assert_eq!(bytes, [0xbb, 0xbb, 0x98]);
    assert_eq!(memory.loads.get(), 1);
    assert!(scatter(&memory, &offsets, &[0x34, 0x12, 0xa7], false));
    assert_eq!(memory.lane.get(), 0xaa12_ccdd_eeff_a776);
    assert_eq!(memory.compare_exchanges.get(), 1);
}

#[test]
fn shared_partial_store_remerges_the_latest_neighbor_on_failed_cas() {
    let memory = Memory::default();
    memory.pending_neighbor.set(true);
    assert!(scatter(&memory, &[0], &[0x34], false));
    assert_eq!(memory.lane.get(), 0xdf34);
    assert_eq!(memory.compare_exchanges.get(), 2);
    assert_eq!(memory.stores.get(), 0);
}

#[test]
fn shared_contiguous_full_lane_store_uses_only_one_atomic_store() {
    let memory = Memory::default();
    assert!(scatter(
        &memory,
        &[0, 1, 2, 3, 4, 5, 6, 7],
        &[1, 2, 3, 4, 5, 6, 7, 8],
        false
    ));
    assert_eq!(memory.lane.get(), 0x0807_0605_0403_0201);
    assert_eq!(memory.loads.get(), 0);
    assert_eq!(memory.stores.get(), 1);
}

#[cfg(feature = "parallel")]
#[test]
fn shared_layout_keeps_logical_lengths_and_projects_aliased_dependencies() {
    use crate::memory::{Image, MappingInput, MemoryLimits, Permissions};

    let limits = MemoryLimits {
        max_backing_bytes: 8,
        max_backings: 3,
        max_mappings: 4,
    };
    let mapping = |base, offset, length| MappingInput {
        base,
        offset,
        length,
        backing: "image".into(),
        permissions: Permissions {
            read: true,
            write: true,
            execute: true,
        },
    };
    let layout = [0x1000, 0x2000, 0x3000, 0x4000];
    let backing = || vec![("image".into(), 8, 0x1000, 1)];
    let mut image = Image::mapped_shared(
        backing(),
        vec![
            mapping(0x4000, 2, 2),
            mapping(0x4002, 4, 3),
            mapping(0x8000, 3, 2),
        ],
        limits,
        false,
        layout,
    )
    .unwrap();
    assert!(
        Image::mapped_shared(
            backing(),
            vec![mapping(0x4000, 7, 2)],
            limits,
            false,
            layout
        )
        .is_err()
    );
    let source = image
        .register_dependencies([(0x4001, 3)], [(0x8000, 2)])
        .unwrap();
    assert_eq!(
        image.parallel_proof(source),
        Some(vec![(0x1003, 1), (0x1004, 2), (0x1003, 2)])
    );
    image.parallel_invalidate_range(0x1007, 1);
    assert!(image.owner_valid(source));
    image.parallel_invalidate_range(0x1005, 1);
    assert!(!image.owner_valid(source));
    assert_eq!(image.parallel_proof(source), None);
}

#[test]
fn shared_cas_limit_yields_before_the_operand_becomes_visible() {
    let memory = Memory::default();
    memory.reject_every_cas.set(true);
    assert!(!scatter(&memory, &[0], &[0xac], false));
    assert_eq!(memory.compare_exchanges.get(), CAS_ATTEMPTS);
    assert_eq!(memory.lane.get() & 0xff, 0);
    assert!(scatter(&memory, &[0], &[0xac], true));
    assert_eq!(memory.lane.get() & 0xff, 0xac);
}
