use super::*;
use crate::memory::{Access, Image, MappingInput, MemoryLimits, Permissions};
use crate::x86::paging::{
    State,
    walk::{self, AccessClass, Request},
};

pub(super) fn fixture() -> (Image, State, Request) {
    let mut bytes = vec![0; 0x10000];
    for (address, word) in [
        (0x1000, 0x2007u64),
        (0x2000, 0x3007),
        (0x3000, 0x4007),
        (0x4008, 0xa067),
    ] {
        bytes[address..address + 8].copy_from_slice(&word.to_le_bytes());
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
            max_mappings: 1,
        },
    )
    .unwrap();
    (
        image,
        State {
            cr3: 0x1000,
            ..State::default()
        },
        Request {
            linear: 0x1234,
            access: Access::Read,
            class: AccessClass::User,
        },
    )
}

#[test]
fn retained_mapping_and_live_rights_are_distinct_from_current_pte_bytes() {
    let (mut image, mut state, request) = fixture();
    let cache = Cache::default();
    let translation = walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap();
    cache.fill(&state, request, translation);
    image.physical_write(0x4008, 8, 0xb067).unwrap();
    assert_eq!(
        cache
            .lookup(&state, request, &ProtectionKeys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xa234
    );
    state.cr4 |= 1 << 21;
    let supervisor = Request {
        class: AccessClass::Supervisor { ac: false },
        ..request
    };
    assert!(matches!(
        cache.lookup(&state, supervisor, &ProtectionKeys::ZERO),
        Err(walk::Failure::Page(walk::PageFault { error_code: 1, .. }))
    ));
    assert!(
        cache
            .lookup(&state, request, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
    let fresh = walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap();
    assert_eq!(fresh.physical, 0xb234);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_dword_miss_matches_checked_fill_and_successor_fetch() {
    let (mut image, state, request) = fixture();
    let code = Request {
        access: Access::Execute,
        ..request
    };
    let data = Request {
        linear: 0x2234,
        ..request
    };
    let old = Request {
        linear: 0x3234,
        ..request
    };
    let keys = &ProtectionKeys::ZERO;
    image.physical_write(0x4010, 8, 0xb007).unwrap();
    image.physical_write(0x4018, 8, 0xc027).unwrap();
    image.physical_write(0xb234, 4, 0xf1234567).unwrap();
    let cache = Cache::with_capacity(2);
    let reference = Cache::with_capacity(2);
    for page in [old, code] {
        let translation = walk::walk(&mut image, &state, page, keys).unwrap();
        cache.fill(&state, page, translation);
        reference.fill(&state, page, translation);
    }
    let read = |image: &Image, source| {
        cache.read_dword_then_fetch(image, &state, data, keys, || {
            if source {
                walk::peek(image, &state, data, keys).ok()
            } else {
                None
            }
        })
    };
    let before = cache.entries.borrow().clock;
    assert_eq!(read(&image, true), None);
    assert_eq!(image.physical_read(0x4010, 8).unwrap(), 0xb007);
    assert_eq!(cache.entries.borrow().clock, before);
    assert!(!cache.take_changed());
    let walked = walk::walk(&mut image, &state, data, keys).unwrap();
    assert_eq!(image.physical_read(0x4010, 8).unwrap(), 0xb027);
    assert_eq!(read(&image, false), None);
    assert!(!cache.take_changed());

    let expected = image.physical_read(walked.physical, 4).unwrap() as u32;
    reference.fill(&state, data, walked);
    reference.lookup(&state, code, keys).unwrap().unwrap();
    assert_eq!(read(&image, true), Some(expected));
    assert_eq!(cache.observe(&state, old, keys).unwrap(), None);
    assert!(cache.take_changed());
    let actual = cache.entries.borrow();
    let exact = reference.entries.borrow();
    assert_eq!(
        (actual.clock, actual.hint, actual.next_id),
        (exact.clock, exact.hint, exact.next_id)
    );
    assert_eq!(
        actual
            .slots
            .map(|entry| entry.map(|entry| (entry.translation, entry.used))),
        exact
            .slots
            .map(|entry| entry.map(|entry| (entry.translation, entry.used))),
    );
    drop(actual);
    assert_eq!(
        read(&image, false),
        Some(expected),
        "a retained hit needs no new walk"
    );
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_dword_miss_refuses_eviction_of_code_or_a_retained_denial() {
    let (mut image, mut state, request) = fixture();
    let code = Request {
        access: Access::Execute,
        ..request
    };
    let data = Request {
        linear: 0x2234,
        ..request
    };
    let keys = &ProtectionKeys::ZERO;
    let capacity_one = Cache::with_capacity(1);
    let cached = Cache::with_capacity(2);
    let execute = walk::walk(&mut image, &state, code, keys).unwrap();
    capacity_one.fill(&state, code, execute);
    cached.fill(&state, code, execute);
    image.physical_write(0x4010, 8, 0xb027).unwrap();
    assert_eq!(
        capacity_one.read_dword_then_fetch(&image, &state, data, keys, || panic!("code eviction")),
        None
    );
    assert_eq!(capacity_one.entries.borrow().clock, 1);
    assert!(!capacity_one.take_changed());

    // A stale small code entry can coexist with a new covering PDE.
    image.physical_write(0x3000, 8, 0xa7).unwrap();
    assert_eq!(
        cached.read_dword_then_fetch(&image, &state, data, keys, || {
            walk::peek(&image, &state, data, keys).ok()
        }),
        None
    );
    assert_eq!(cached.entries.borrow().clock, 1);
    assert!(!cached.take_changed());

    image.physical_write(0x3000, 8, 0x4027).unwrap();
    let translation = walk::walk(&mut image, &state, data, keys).unwrap();
    cached.fill(&state, data, translation);
    cached.lookup(&state, code, keys).unwrap().unwrap();
    state.cr4 |= 1 << 21;
    let denied = Request {
        class: AccessClass::Supervisor { ac: false },
        ..data
    };
    walk::physical_counts::take();
    assert_eq!(
        cached.read_dword_then_fetch(&image, &state, denied, keys, || panic!("retained denial")),
        None,
    );
    assert_eq!(walk::physical_counts::take(), [0; 6]);
    assert!(!cached.take_changed());
}

#[test]
fn read_span_touches_only_the_authorized_whole_operand() {
    let (mut image, mut state, request) = fixture();
    let cache = Cache::default();
    let translation = walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap();
    cache.fill(&state, request, translation);
    let clock = cache.entries.borrow().clock;
    #[cfg(feature = "parallel")]
    let hit = Some(0xa234);
    #[cfg(not(feature = "parallel"))]
    let hit = RetainedSpan::Hit(0xa234);
    assert_eq!(
        cache.touch_read_span(&state, request, &ProtectionKeys::ZERO, 16),
        hit
    );
    assert_eq!(cache.entries.borrow().clock, clock + 1);
    #[cfg(feature = "parallel")]
    let (refused, missed) = (None, None);
    #[cfg(not(feature = "parallel"))]
    let (refused, missed) = (RetainedSpan::Refused, RetainedSpan::Miss);
    for (linear, expected) in [(0x1fff, &refused), (0x2000, &missed)] {
        assert_eq!(
            &cache.touch_read_span(
                &state,
                Request { linear, ..request },
                &ProtectionKeys::ZERO,
                2
            ),
            expected
        );
        assert_eq!(cache.entries.borrow().clock, clock + 1);
        assert!(!cache.take_changed());
    }
    state.cr4 |= 1 << 21;
    let supervisor = Request {
        class: AccessClass::Supervisor { ac: false },
        ..request
    };
    assert_eq!(
        cache.touch_read_span(&state, supervisor, &ProtectionKeys::ZERO, 4),
        refused
    );
    assert_eq!(cache.entries.borrow().clock, clock + 1);
    assert!(!cache.take_changed());
    assert!(matches!(
        cache.lookup(&state, supervisor, &ProtectionKeys::ZERO),
        Err(Failure::Page(_))
    ));
    assert!(cache.take_changed());
}

#[test]
fn dirty_zero_write_intent_evicts_before_cached_denial() {
    let (mut image, state, request) = fixture();
    image.physical_write(0x4008, 8, 0xa025).unwrap();
    let cache = Cache::default();
    cache.fill(
        &state,
        request,
        walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap(),
    );
    image.physical_write(0x4008, 8, 0xb067).unwrap();
    assert!(
        cache
            .lookup(
                &state,
                Request {
                    access: Access::Write,
                    ..request
                },
                &ProtectionKeys::ZERO
            )
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .lookup(&state, request, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
}
