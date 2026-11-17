use super::*;
use crate::memory::{MappingInput, MemoryLimits, Permissions};
use crate::x86::paging::walk::{Failure, Request};

fn fixture(first: u64, second: u64) -> (Image, State, AccessState, Cache) {
    let mut bytes = vec![0; 0x10000];
    for (address, word) in [
        (0x1000, 0x2027u64),
        (0x2000, 0x3027),
        (0x3000, 0x4027),
        (0x4008, first),
        (0x4010, second),
    ] {
        bytes[address..address + 8].copy_from_slice(&word.to_le_bytes());
    }
    for (address, value) in [(0x9000, 0xa1), (0xa000, 0xb2), (0xb000, 0xc3)] {
        bytes[address] = value;
    }
    let image = Image::physical_mapped(
        vec![("ram".into(), bytes)],
        vec![MappingInput {
            base: 0,
            backing: "ram".into(),
            offset: 0,
            length: 0x10000,
            permissions: Permissions {
                read: true,
                write: true,
                execute: true,
            },
        }],
        MemoryLimits {
            max_backing_bytes: 0x10000,
            max_backings: 1,
            max_mappings: 3,
        },
    )
    .unwrap();
    (
        image,
        State {
            cr3: 0x1000,
            ..State::default()
        },
        AccessState::default(),
        Cache::default(),
    )
}

fn request(linear: u64) -> Request {
    Request {
        linear,
        access: Access::Read,
        class: AccessClass::User,
    }
}

#[test]
fn warm_read_uses_retained_mapping_without_walking_or_reasserting_accessed() {
    let (mut image, state, mut work, cache) = fixture(0x9027, 0);
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work)
            .with_cache(&cache)
            .read(0x1000, 1)
            .unwrap(),
        0xa1
    );
    image.physical_write(0x4008, 8, 0xa007).unwrap();
    walk::physical_counts::take();
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work)
            .with_cache(&cache)
            .read(0x1000, 1)
            .unwrap(),
        0xa1
    );
    assert_eq!(walk::physical_counts::take(), [0; 6]);
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0xa007);
    cache.invalidate_page(0, 0x1000, true);
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work)
            .with_cache(&cache)
            .read(0x1000, 1)
            .unwrap(),
        0xb2
    );
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0xa027);
}

#[test]
fn dirty_zero_rmw_reads_new_mapping_before_capture_and_commit_keeps_that_plan() {
    let (mut image, state, mut work, cache) = fixture(0x9027, 0);
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .read(0x1000, 1)
        .unwrap();
    image.physical_write(0x4008, 8, 0xa027).unwrap();
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work)
            .with_cache(&cache)
            .read_for_write(0x1000, 1)
            .unwrap(),
        0xb2
    );
    image.physical_write(0x4008, 8, 0xb027).unwrap();
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .write(0x1000, 1, 0x44)
        .unwrap();
    assert_eq!(image.physical_read(0x9000, 1).unwrap(), 0xa1);
    assert_eq!(image.physical_read(0xa000, 1).unwrap(), 0x44);
    assert_eq!(image.physical_read(0xb000, 1).unwrap(), 0xc3);
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0xb067);
    assert_eq!(
        cache
            .observe(&state, request(0x1000))
            .unwrap()
            .unwrap()
            .physical,
        0xa000
    );
}

#[test]
fn dirty_ready_write_keeps_old_mapping_without_touching_retargeted_leaf() {
    let (mut image, state, mut work, cache) = fixture(0x9067, 0);
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .read(0x1000, 1)
        .unwrap();
    image.physical_write(0x4008, 8, 0xa027).unwrap();
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .write(0x1000, 1, 0x55)
        .unwrap();
    assert_eq!(image.physical_read(0x9000, 1).unwrap(), 0x55);
    assert_eq!(image.physical_read(0xa000, 1).unwrap(), 0xb2);
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0xa027);
}

#[test]
fn failed_dirty_refresh_does_not_resurrect_the_old_mapping() {
    let (mut image, state, mut work, cache) = fixture(0x9027, 0);
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .read(0x1000, 1)
        .unwrap();
    image.physical_write(0x4008, 8, 0xa025).unwrap();
    assert!(matches!(
        Memory::new(&mut image, &state, 3, 2, &mut work)
            .with_cache(&cache)
            .write(0x1000, 1, 0),
        Err(AccessFailure::Paging(Failure::Page(_)))
    ));
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work)
            .with_cache(&cache)
            .read(0x1000, 1)
            .unwrap(),
        0xb2
    );
}

#[test]
fn later_page_private_or_alignment_refusal_does_not_publish_first_candidate() {
    for second in [0, 0x10027, 0xa027] {
        let (mut image, mut state, mut work, cache) = fixture(0x9027, second);
        state.cr0 |= 1 << 18;
        let failure = Memory::new(&mut image, &state, 3, 0x40002, &mut work)
            .with_cache(&cache)
            .read_aligned(0x1fff, 4, Intent::Natural(4))
            .unwrap_err();
        match second {
            0 => assert!(matches!(failure, AccessFailure::Paging(Failure::Page(_)))),
            0x10027 => assert!(matches!(
                failure,
                AccessFailure::Paging(Failure::Physical(_))
            )),
            _ => assert!(matches!(failure, AccessFailure::AlignmentCheck(_))),
        }
        assert!(cache.observe(&state, request(0x1000)).unwrap().is_none());
        image.physical_write(0x4008, 8, 0xa027).unwrap();
        assert_eq!(
            Memory::new(&mut image, &state, 3, 0x40002, &mut work)
                .with_cache(&cache)
                .read(0x1000, 1)
                .unwrap(),
            0xb2
        );
        assert!(work.pending.is_empty());
    }
}

#[test]
fn write_only_probe_refreshes_without_dirty_or_payload_and_next_store_refreshes_again() {
    let (mut image, state, mut work, cache) = fixture(0x9027, 0);
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .read(0x1000, 1)
        .unwrap();
    image.physical_write(0x4008, 8, 0xa027).unwrap();
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .probe_write(0x1000)
        .unwrap();
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0xa027);
    assert_eq!(image.physical_read(0xa000, 1).unwrap(), 0xb2);
    assert!(work.pending.is_empty());
    image.physical_write(0x4008, 8, 0xb027).unwrap();
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .write(0x1000, 1, 0x66)
        .unwrap();
    assert_eq!(image.physical_read(0xa000, 1).unwrap(), 0xb2);
    assert_eq!(image.physical_read(0xb000, 1).unwrap(), 0x66);
}

#[test]
fn retained_hit_rechecks_current_private_permissions_without_fault_eviction() {
    for access in [Access::Read, Access::Write] {
        let (mut image, state, mut work, cache) = fixture(0x9067, 0);
        Memory::new(&mut image, &state, 3, 2, &mut work)
            .with_cache(&cache)
            .read(0x1000, 1)
            .unwrap();
        image
            .set_mappings(vec![MappingInput {
                base: 0,
                backing: "ram".into(),
                offset: 0,
                length: 0x10000,
                permissions: Permissions {
                    read: access != Access::Read,
                    write: access != Access::Write,
                    execute: true,
                },
            }])
            .unwrap();
        let mut memory = Memory::new(&mut image, &state, 3, 2, &mut work).with_cache(&cache);
        let failure = if access == Access::Read {
            memory.read(0x1000, 1).unwrap_err()
        } else {
            memory.write(0x1000, 1, 0).unwrap_err()
        };
        assert!(matches!(
            failure,
            AccessFailure::Paging(Failure::Physical(walk::PhysicalFault {
                phase: PhysicalPhase::FinalAccess,
                ..
            }))
        ));
        assert!(cache.observe(&state, request(0x1000)).unwrap().is_some());
    }
}

#[test]
fn ordered_captured_dirty_feedback_cannot_mark_recycled_unrelated_mapping() {
    let (mut image, state, mut work, _) = fixture(0x9027, 0xa027);
    let cache = Cache::with_capacity(1);
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .check_write(0x1000, 1)
        .unwrap();
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .read(0x2000, 1)
        .unwrap();
    assert!(
        cache
            .observe(&state, request(0x2000))
            .unwrap()
            .unwrap()
            .dirty
    );
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .prepare_publication()
        .unwrap();
    assert!(
        cache
            .observe(&state, request(0x2000))
            .unwrap()
            .unwrap()
            .dirty
    );
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0x9067);
    assert_eq!(image.physical_read(0x4010, 8).unwrap(), 0xa027);
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .write(0x1000, 1, 0x77)
        .unwrap();
    assert_eq!(image.physical_read(0x9000, 1).unwrap(), 0x77);
    assert_eq!(image.physical_read(0xa000, 1).unwrap(), 0xb2);
}

#[test]
fn refused_dirty_metadata_after_refill_keeps_new_facts_unready_until_real_publication() {
    let (mut image, state, mut work, cache) = fixture(0x9027, 0);
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .read(0x1000, 1)
        .unwrap();
    image.physical_write(0x4008, 8, 0xa027).unwrap();
    let mapping = |base, length, write| MappingInput {
        base,
        backing: "ram".into(),
        offset: base as u32,
        length,
        permissions: Permissions {
            read: true,
            write,
            execute: true,
        },
    };
    image
        .set_mappings(vec![
            mapping(0, 0x9000, false),
            mapping(0x9000, 0x7000, true),
        ])
        .unwrap();
    let failure = Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .write(0x1000, 1, 0x99)
        .unwrap_err();
    assert!(matches!(
        failure,
        AccessFailure::Paging(Failure::Physical(walk::PhysicalFault {
            phase: PhysicalPhase::DirtyWrite,
            ..
        }))
    ));
    let retained = cache.observe(&state, request(0x1000)).unwrap().unwrap();
    assert_eq!((retained.physical, retained.dirty), (0xa000, true));
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0xa027);
    assert_eq!(image.physical_read(0xa000, 1).unwrap(), 0xb2);
    image.set_mappings(vec![mapping(0, 0x10000, true)]).unwrap();
    Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .write(0x1000, 1, 0x99)
        .unwrap();
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0xa067);
    assert_eq!(image.physical_read(0xa000, 1).unwrap(), 0x99);
    assert!(
        !cache
            .observe(&state, request(0x1000))
            .unwrap()
            .unwrap()
            .dirty
    );
}

#[test]
fn warmed_translation_rechecks_implicit_and_prospective_classes_then_keeps_captured_frame() {
    let (mut image, mut state, mut work, cache) = fixture(0x9067, 0);
    state.cr0 |= 1 << 18;
    state.cr4 |= 1 << 21;
    let flags = 0x40002;
    Memory::new(&mut image, &state, 3, flags, &mut work)
        .with_cache(&cache)
        .read(0x1000, 1)
        .unwrap();
    let implicit = Memory::new(&mut image, &state, 3, flags, &mut work)
        .with_cache(&cache)
        .read_system(0x1001, 8)
        .unwrap_err();
    assert!(matches!(
        implicit,
        AccessFailure::Paging(Failure::Page(walk::PageFault { error_code: 1, .. }))
    ));
    assert!(cache.observe(&state, request(0x1000)).unwrap().is_none());
    Memory::new(&mut image, &state, 3, flags, &mut work)
        .with_cache(&cache)
        .read(0x1000, 1)
        .unwrap();
    let denied = Memory::new(&mut image, &state, 3, 2, &mut work)
        .with_cache(&cache)
        .check_frame_write_aligned(0x1001, 8, 0, Intent::Natural(8))
        .unwrap_err();
    assert!(matches!(
        denied,
        AccessFailure::Paging(Failure::Page(walk::PageFault { error_code: 3, .. }))
    ));
    Memory::new(&mut image, &state, 3, flags, &mut work)
        .with_cache(&cache)
        .read(0x1000, 1)
        .unwrap();
    let user = Memory::new(&mut image, &state, 0, flags, &mut work)
        .with_cache(&cache)
        .check_frame_write_aligned(0x1001, 8, 3, Intent::Natural(8))
        .unwrap_err();
    assert!(matches!(user, AccessFailure::AlignmentCheck(_)));
    Memory::new(&mut image, &state, 3, flags, &mut work)
        .with_cache(&cache)
        .check_frame_write_aligned(0x1001, 8, 0, Intent::Natural(8))
        .unwrap();
    image.physical_write(0x4008, 8, 0xa027).unwrap();
    cache.invalidate_page(0, 0x1000, true);
    {
        let mut memory = Memory::new(&mut image, &state, 3, flags, &mut work).with_cache(&cache);
        memory.prepare_publication().unwrap();
        memory
            .write_aligned(0x1001, 8, 0x1122334455667788, Intent::Natural(8))
            .unwrap();
    }
    assert_eq!(image.physical_read(0x9001, 8).unwrap(), 0x1122334455667788);
    assert_eq!(image.physical_read(0xa001, 8).unwrap(), 0);
    assert!(cache.observe(&state, request(0x1000)).unwrap().is_none());
}
