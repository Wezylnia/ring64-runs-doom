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

#[cfg(not(feature = "parallel"))]
#[test]
fn retained_dword_read_checks_the_live_occupant_after_invalidation() {
    let (mut image, state, first) = fixture();
    let code = Request {
        access: Access::Execute,
        ..first
    };
    let data = Request {
        linear: 0x2000,
        ..first
    };
    let replacement = Request {
        linear: 0x3000,
        ..first
    };
    image.physical_write(0x4010, 8, 0xb067).unwrap();
    image.physical_write(0x4018, 8, 0xc067).unwrap();
    let cache = Cache::with_capacity(2);
    let mut fill = |request| {
        let translation = walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap();
        cache.fill(&state, request, translation);
        translation.physical
    };
    let code_physical = fill(code);
    assert_eq!(fill(data), 0xb000);
    assert!(cache.touch_current_fetch(&state, code, code_physical));
    assert_eq!(
        cache.read_local_dword_then_fetch(&state, data, &ProtectionKeys::ZERO, Some),
        Some(0xb000)
    );

    cache.invalidate_page(state.pcid(), data.linear, false);
    assert_eq!(fill(replacement), 0xc000);
    assert!(cache.touch_current_fetch(&state, code, code_physical));
    assert_eq!(
        cache.read_local_dword_then_fetch(&state, data, &ProtectionKeys::ZERO, Some),
        None
    );
    assert_eq!(
        cache.read_local_dword_then_fetch(&state, replacement, &ProtectionKeys::ZERO, Some),
        Some(0xc000)
    );
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
fn paired_touch_matches_sequential_ages_at_clock_rollover() {
    let (mut image, state, data) = fixture();
    image.physical_write(0x4010, 8, 0xb027).unwrap();
    let fetch = Request {
        linear: data.linear + 4096,
        access: Access::Execute,
        ..data
    };
    let keys = &ProtectionKeys::ZERO;
    let walked = [data, fetch].map(|request| {
        let translation = walk::walk(&mut image, &state, request, keys).unwrap();
        (request, translation)
    });
    let make_cache = |clock| {
        let cache = Cache::with_capacity(2);
        for (request, translation) in walked {
            cache.fill(&state, request, translation);
        }
        {
            let mut entries = cache.entries.borrow_mut();
            entries.clock = clock;
            entries.slots[0].as_mut().unwrap().used = clock - 1;
            entries.slots[1].as_mut().unwrap().used = clock;
        }
        cache
    };
    for clock in [u64::MAX - 2, u64::MAX - 1, u64::MAX] {
        let exact = make_cache(clock);
        let paired = make_cache(clock);
        let mut exact = exact.entries.borrow_mut();
        exact.touch(0);
        exact.touch(1);
        let mut paired = paired.entries.borrow_mut();
        paired.touch_data_and_fetch(0, 1);
        assert_eq!((paired.clock, paired.hint), (exact.clock, exact.hint));
        assert_eq!(
            paired.slots.map(|entry| entry.map(|entry| entry.used)),
            exact.slots.map(|entry| entry.map(|entry| entry.used)),
        );
    }
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
fn guarded_dword_miss_uses_the_first_vacancy_before_an_older_live_slot() {
    let (mut image, state, code) = fixture();
    let code = Request {
        access: Access::Execute,
        ..code
    };
    let data = Request {
        linear: 0x2234,
        access: Access::Read,
        ..code
    };
    let old = Request {
        linear: 0x3234,
        ..data
    };
    let keys = &ProtectionKeys::ZERO;
    image.physical_write(0x4010, 8, 0xb027).unwrap();
    image.physical_write(0x4018, 8, 0xc027).unwrap();
    let cache = Cache::with_capacity(3);
    for page in [data, old, code] {
        cache.fill(
            &state,
            page,
            walk::walk(&mut image, &state, page, keys).unwrap(),
        );
    }
    cache.invalidate_page(state.pcid(), data.linear, true);
    assert!(cache.take_changed());
    assert_eq!(cache.entries.borrow().hint, Some(2));
    assert_eq!(
        cache.read_dword_then_fetch(&image, &state, data, keys, || {
            walk::peek(&image, &state, data, keys).ok()
        }),
        Some(0)
    );
    let entries = cache.entries.borrow();
    assert_eq!(entries.find(&state, data), Some(0));
    assert_eq!(entries.find(&state, old), Some(1));
    assert_eq!(entries.hint, Some(2));
    assert!(!cache.take_changed());
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
