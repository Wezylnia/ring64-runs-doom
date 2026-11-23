use super::*;
#[cfg(not(feature = "parallel"))]
use crate::memory::DirectSpan;
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
fn grouped_operand_touches_match_separate_touches_with_aliases_and_clock_rebasing() {
    let grouped = Cache::new(4);
    let reference = Cache::new(4);
    let mut grouped = grouped.entries.borrow_mut();
    let mut reference = reference.entries.borrow_mut();
    for start in [0, u64::MAX - 2] {
        for index in 0..4 {
            for table in 0..4 {
                for store in 0..4 {
                    for entries in [&mut grouped, &mut reference] {
                        entries.recency_mut().clock = start;
                        entries.recency_mut().ages.fill(0);
                        entries.set_hint(Some(3));
                    }
                    let slots = [index, table, store, 3];
                    grouped.touch_indexed_group(slots);
                    for slot in slots {
                        if reference.hint() != Some(slot) {
                            reference.touch(slot);
                        }
                    }
                    assert_eq!(grouped.hint(), reference.hint());
                    assert_eq!(grouped.recency().clock, reference.recency().clock);
                    assert_eq!(grouped.recency().ages, reference.recency().ages);
                }
            }
        }
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn indexed_operand_span_checks_width_and_slot_replacement() {
    let (mut image, state, read) = fixture();
    let cache = Cache::new(1);
    let fill = |image: &mut Image| {
        let translation = walk::walk(image, &state, read, &ProtectionKeys::ZERO).unwrap();
        cache.fill(&state, read, translation);
    };
    let mut slot = None;
    let mut resolve = |linear, width| {
        cache.entries.borrow().indexed_write_span::<false>(
            &mut slot,
            &state,
            Request { linear, ..read },
            &ProtectionKeys::ZERO,
            width,
            &mut false,
        )
    };

    fill(&mut image);
    assert_eq!(resolve(0x1234, 1), Some((0, 0xa234)));
    assert_eq!(resolve(0x1ffc, 4), Some((0, 0xaffc)));
    assert_eq!(resolve(0x1ffd, 4), None);
    cache.invalidate_page(0, 0x1234, true);
    assert_eq!(resolve(0x1234, 1), None);
    image.physical_write(0x4008, 8, 0xb067).unwrap();
    fill(&mut image);
    assert_eq!(resolve(0x1234, 1), Some((0, 0xb234)));
}

#[cfg(not(feature = "parallel"))]
#[test]
fn current_fetch_requires_the_right_physical_span_and_live_execute_rights() {
    use crate::x86::paging::{CR4_SMAP, CR4_SMEP};

    let (mut image, mut state, read) = fixture();
    state.efer |= 1 << 11;
    let cache = Cache::default();
    let fetch = Request {
        access: Access::Execute,
        ..read
    };
    let supervisor = Request {
        class: AccessClass::Supervisor { ac: false },
        ..fetch
    };
    let fill = |image: &mut Image| {
        let translation = walk::walk(image, &state, read, &ProtectionKeys::ZERO).unwrap();
        cache.fill(&state, read, translation);
        translation.physical
    };
    let physical = fill(&mut image);
    assert!(cache.touch_current_fetch(&state, fetch, physical));
    let smap = State {
        cr4: state.cr4 | CR4_SMAP,
        ..state
    };
    assert!(cache.touch_current_fetch(&smap, supervisor, physical));
    let smep = State {
        cr4: state.cr4 | CR4_SMEP,
        ..state
    };
    let clock = cache.entries.borrow().recency().clock;
    assert!(!cache.touch_current_fetch(&smep, supervisor, physical));
    assert!(!cache.touch_current_fetch(&state, fetch, physical + 1));
    assert_eq!(cache.entries.borrow().recency().clock, clock);
    assert!(cache.touch_current_fetch(&smep, fetch, physical));

    cache.invalidate_page(state.pcid(), read.linear, false);
    assert!(!cache.touch_current_fetch(&state, fetch, physical));
    image.physical_write(0x4008, 8, 0xa067 | (1 << 63)).unwrap();
    assert_eq!(fill(&mut image), physical);
    let clock = cache.entries.borrow().recency().clock;
    assert!(!cache.touch_current_fetch(&state, fetch, physical));
    assert_eq!(cache.entries.borrow().recency().clock, clock);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn retained_fetch_token_keeps_recency_and_refuses_a_recycled_slot() {
    let (mut image, state, code) = fixture();
    let code = Request {
        access: Access::Execute,
        ..code
    };
    let data = Request {
        linear: 0x2000,
        access: Access::Read,
        ..code
    };
    let third = Request {
        linear: 0x3000,
        ..data
    };
    for (address, word) in [(0x4010, 0xb067), (0x4018, 0xc067)] {
        image.physical_write(address, 8, word).unwrap();
    }
    let cache = Cache::with_capacity(2);
    let fill = |image: &mut Image, request| {
        let translation = walk::walk(image, &state, request, &ProtectionKeys::ZERO).unwrap();
        cache.fill(&state, request, translation)
    };
    let physical = fill(&mut image, code).physical;
    assert!(cache.touch_current_fetch(&state, code, physical));
    let token = cache.entries.borrow().admitted_fetch.unwrap();
    let clock = cache.entries.borrow().recency().clock;
    let nearby = Request {
        linear: code.linear + 7,
        ..code
    };
    assert!(cache.touch_current_fetch(&state, nearby, physical + 7));
    assert_eq!(cache.entries.borrow().recency().clock, clock);

    {
        let mut entries = cache.entries.borrow_mut();
        entries.recency_mut().clock = u64::MAX;
        entries.recency_mut().ages[token.index] = u64::MAX;
    }
    assert!(cache.touch_current_fetch(&state, code, physical));
    assert_eq!(cache.entries.borrow().recency().clock, 2);

    fill(&mut image, data);
    let clock = cache.entries.borrow().recency().clock;
    assert!(cache.touch_current_fetch(&state, code, physical));
    assert_eq!(cache.entries.borrow().recency().clock, clock + 1);
    fill(&mut image, third);
    assert!(
        cache
            .observe(&state, code, &ProtectionKeys::ZERO)
            .unwrap()
            .is_some()
    );
    assert!(
        cache
            .observe(&state, data, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );

    cache.invalidate_page(state.pcid(), code.linear, false);
    assert!(!cache.touch_current_fetch(&state, code, physical));
    image.physical_write(0x4008, 8, 0xd067).unwrap();
    let replacement = fill(&mut image, code);
    assert_eq!(cache.entries.borrow().hint(), Some(token.index));
    assert_ne!(replacement.cache_id, Some(token.id));
    assert!(!cache.touch_current_fetch(&state, code, physical));
    assert!(cache.touch_current_fetch(&state, code, replacement.physical));
}

#[cfg(not(feature = "parallel"))]
#[test]
fn retained_fetch_token_follows_subpages_of_a_large_local_leaf() {
    let (mut image, state, read) = fixture();
    let code = Request {
        access: Access::Execute,
        ..read
    };
    let adjacent = Request {
        linear: code.linear + 4096,
        ..code
    };
    let cache = Cache::with_capacity(2);
    let fill = |image: &mut Image, request| {
        let translation = walk::walk(image, &state, request, &ProtectionKeys::ZERO).unwrap();
        cache.fill(&state, request, translation)
    };
    image.physical_write(0x3000, 8, 0x2000e7).unwrap();
    let large = fill(&mut image, code);
    assert_eq!((large.span, large.physical), (2 * 1024 * 1024, 0x201234));
    assert!(cache.touch_current_fetch(&state, code, large.physical));
    let clock = cache.entries.borrow().recency().clock;
    assert!(cache.touch_current_fetch(
        &state,
        Request {
            linear: code.linear + 8,
            ..code
        },
        large.physical + 8
    ));
    assert!(!cache.touch_current_fetch(&state, adjacent, large.physical));
    assert_eq!(cache.entries.borrow().recency().clock, clock);
    assert!(cache.touch_current_fetch(&state, adjacent, 0x202234));
    let adjacent_token = cache.entries.borrow().admitted_fetch.unwrap();
    assert_eq!(adjacent_token.linear_base, 0x2000);
    assert_eq!(Some(adjacent_token.id), large.cache_id);

    image.physical_write(0x3000, 8, 0x4007).unwrap();
    let small = fill(&mut image, code);
    assert_eq!((small.span, small.physical), (4096, 0xa234));
    assert_eq!(cache.entries.borrow().hint(), Some(adjacent_token.index));
    assert_ne!(small.cache_id, large.cache_id);
    assert!(!cache.touch_current_fetch(&state, adjacent, 0x202234));
    assert!(!cache.touch_current_fetch(&state, code, large.physical));
    assert!(cache.touch_current_fetch(&state, code, small.physical));
}

#[cfg(not(feature = "parallel"))]
#[test]
fn retained_fetch_token_excludes_global_translations_that_can_be_shadowed() {
    let (mut image, mut state, code) = fixture();
    state.cr4 |= 1 << 7; // PGE
    let code = Request {
        access: Access::Execute,
        ..code
    };
    let cache = Cache::with_capacity(2);
    let fill = |image: &mut Image| {
        let translation = walk::walk(image, &state, code, &ProtectionKeys::ZERO).unwrap();
        cache.fill(&state, code, translation)
    };
    image.physical_write(0x4008, 8, 0xa167).unwrap();
    let global = fill(&mut image);
    assert!(cache.touch_current_fetch(&state, code, global.physical));
    assert!(cache.entries.borrow().admitted_fetch.is_none());

    image.physical_write(0x4008, 8, 0xb067).unwrap();
    let local = fill(&mut image);
    assert!(!cache.touch_current_fetch(&state, code, global.physical));
    assert!(cache.touch_current_fetch(&state, code, local.physical));
    assert!(cache.entries.borrow().admitted_fetch.is_some());
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
    image.physical_write(0x4010, 8, 0xb067).unwrap();
    let cache = Cache::with_capacity(2);
    let fill = |image: &mut Image, request| {
        let translation = walk::walk(image, &state, request, &ProtectionKeys::ZERO).unwrap();
        cache.fill(&state, request, translation);
        translation.physical
    };
    let code_physical = fill(&mut image, code);
    assert_eq!(fill(&mut image, data), 0xb000);
    assert!(cache.touch_current_fetch(&state, code, code_physical));
    let admit = |base| {
        Some(DirectSpan {
            base,
            length: 16,
            host: base as u32,
        })
    };
    assert_eq!(
        cache.read_local_dword_then_fetch(&state, data, &ProtectionKeys::ZERO, admit),
        Some(0xb000)
    );
    let nearby = Request {
        linear: data.linear + 12,
        ..data
    };
    let refuse_recheck = |_| panic!("covered data should reuse its backing");
    let clock = cache.entries.borrow().recency().clock;
    assert_eq!(
        cache.read_local_dword_then_fetch(&state, nearby, &ProtectionKeys::ZERO, refuse_recheck),
        Some(0xb00c)
    );
    assert_eq!(cache.entries.borrow().recency().clock, clock);
    let outside = Request {
        linear: nearby.linear + 1,
        ..nearby
    };
    assert_eq!(
        cache.read_local_dword_then_fetch(&state, outside, &ProtectionKeys::ZERO, |address| {
            assert_eq!(address, 0xb00d);
            None
        }),
        None
    );
    let denied = Request {
        class: AccessClass::Supervisor { ac: false },
        ..nearby
    };
    let smap = State {
        cr4: state.cr4 | (1 << 21),
        ..state
    };
    assert_eq!(
        cache.read_local_dword_then_fetch(&smap, denied, &ProtectionKeys::ZERO, refuse_recheck),
        None
    );
    assert_eq!(cache.entries.borrow().recency().clock, clock);

    cache.invalidate_page(state.pcid(), data.linear, false);
    assert_eq!(
        cache.read_local_dword_then_fetch(&state, data, &ProtectionKeys::ZERO, refuse_recheck),
        None
    );
    image.physical_write(0x4010, 8, 0xc067).unwrap();
    assert_eq!(fill(&mut image, data), 0xc000);
    assert!(cache.touch_current_fetch(&state, code, code_physical));
    assert_eq!(
        cache.read_local_dword_then_fetch(&state, data, &ProtectionKeys::ZERO, admit),
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
            entries.recency_mut().clock = clock;
            entries.recency_mut().ages[0] = clock - 1;
            entries.recency_mut().ages[1] = clock;
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
        assert_eq!(
            (paired.recency().clock, paired.hint()),
            (exact.recency().clock, exact.hint())
        );
        assert_eq!(paired.recency().ages, exact.recency().ages);
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_dword_reports_checked_mapping_on_miss_and_resident_hit() {
    let (mut image, state, request) = fixture();
    let keys = &ProtectionKeys::ZERO;
    let code = Request {
        access: Access::Execute,
        ..request
    };
    let cache = Cache::with_capacity(2);
    let fetch = cache.fill(
        &state,
        code,
        walk::walk(&mut image, &state, code, keys).unwrap(),
    );
    image.physical_write(0x4010, 8, 0xb027).unwrap();
    image.physical_write(0xb020, 4, 0x11223344).unwrap();
    image.physical_write(0xb024, 4, 0xaabbccdd).unwrap();
    let data = Request {
        linear: 0x2020,
        ..request
    };
    let (value, translation) = cache
        .read_guarded_dword_then_fetch(&image, &state, data, keys, || {
            walk::peek(&image, &state, data, keys).ok()
        })
        .unwrap();
    assert_eq!((value, translation.physical), (0x11223344, 0xb020));
    assert_eq!(
        (
            translation.linear_base,
            translation.physical_base,
            translation.span
        ),
        (0x2000, 0xb000, 4096)
    );

    // A guest PTE write alone does not retire the resident mapping.
    image.physical_write(0x4010, 8, 0xc027).unwrap();
    let next = Request {
        linear: 0x2024,
        ..request
    };
    let (value, translation) = cache
        .read_guarded_dword_then_fetch(&image, &state, next, keys, || {
            panic!("resident guarded read must not walk")
        })
        .unwrap();
    assert_eq!((value, translation.physical), (0xaabbccdd, 0xb024));
    assert_eq!(
        (
            translation.linear_base,
            translation.physical_base,
            translation.span
        ),
        (0x2000, 0xb000, 4096)
    );
    let entries = cache.entries.borrow();
    let newest = entries.slots[entries.hint().unwrap()].unwrap();
    assert_eq!(newest.translation.cache_id, fetch.cache_id);
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
    let before = cache.entries.borrow().recency().clock;
    assert_eq!(read(&image, true), None);
    assert_eq!(image.physical_read(0x4010, 8).unwrap(), 0xb007);
    assert_eq!(cache.entries.borrow().recency().clock, before);
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
        (actual.recency().clock, actual.hint(), actual.next_id),
        (exact.recency().clock, exact.hint(), exact.next_id)
    );
    assert_eq!(
        actual
            .slots
            .map(|entry| entry.map(|entry| entry.translation)),
        exact
            .slots
            .map(|entry| entry.map(|entry| entry.translation)),
    );
    assert_eq!(actual.recency().ages, exact.recency().ages);
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
    assert_eq!(cache.entries.borrow().hint(), Some(2));
    assert_eq!(
        cache.read_dword_then_fetch(&image, &state, data, keys, || {
            walk::peek(&image, &state, data, keys).ok()
        }),
        Some(0)
    );
    let entries = cache.entries.borrow();
    assert_eq!(entries.find(&state, data), Some(0));
    assert_eq!(entries.find(&state, old), Some(1));
    assert_eq!(entries.hint(), Some(2));
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
    assert_eq!(capacity_one.entries.borrow().recency().clock, 1);
    assert!(!capacity_one.take_changed());

    // A stale small code entry can coexist with a new covering PDE.
    image.physical_write(0x3000, 8, 0xa7).unwrap();
    assert_eq!(
        cached.read_dword_then_fetch(&image, &state, data, keys, || {
            walk::peek(&image, &state, data, keys).ok()
        }),
        None
    );
    assert_eq!(cached.entries.borrow().recency().clock, 1);
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
    let clock = cache.entries.borrow().recency().clock;
    #[cfg(feature = "parallel")]
    let hit = Some(0xa234);
    #[cfg(not(feature = "parallel"))]
    let hit = RetainedSpan::Hit(0xa234);
    assert_eq!(
        cache.touch_read_span(&state, request, &ProtectionKeys::ZERO, 16),
        hit
    );
    assert_eq!(cache.entries.borrow().recency().clock, clock + 1);
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
        assert_eq!(cache.entries.borrow().recency().clock, clock + 1);
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
    assert_eq!(cache.entries.borrow().recency().clock, clock + 1);
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

#[cfg(not(feature = "parallel"))]
#[test]
fn indexed_read_collision_and_slot_reuse_preserve_translation_identity() {
    let (mut image, state, first) = fixture();
    let cache = Cache::with_capacity(2);
    let first_page = first.linear & !4095;
    let second = Request {
        linear: first.linear + (INDEXED_READ_ENTRIES as u64 * 4096),
        ..first
    };
    let second_page = second.linear & !4095;
    let pte = 0x4000 + ((second_page >> 12) & 511) * 8;
    image.physical_write(pte, 8, 0xb067).unwrap();
    let mut fill = |request| {
        let translation = walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap();
        let result = cache.fill(&state, request, translation);
        let slot = cache.entries.borrow().find(&state, request).unwrap();
        (slot, result.cache_id.unwrap())
    };
    let publish = |slot, page, host| {
        cache.remember_indexed_read(slot, page, host);
        let entries = cache.entries.borrow();
        let index = ((page >> 12) as usize) & (INDEXED_READ_ENTRIES - 1);
        entries.recency().entries[index]
    };
    let (first_slot, first_id) = fill(first);
    let first_record = publish(first_slot, first_page, 0x10000);
    assert_eq!(
        (first_record.tag, first_record.slot_offset),
        (first_page | INDEXED_VALID, first_slot as u32 * 8),
    );
    let (second_slot, second_id) = fill(second);
    assert_ne!(second_slot, first_slot);
    let second_record = publish(second_slot, second_page, 0x20000);
    assert_eq!(
        (
            second_record.tag,
            (second_page as u32).wrapping_add(second_record.host_bias),
        ),
        (second_page | INDEXED_VALID, 0x20000),
    );
    {
        let entries = cache.entries.borrow();
        assert_eq!(entries.recency().live_ids[first_slot], first_id);
        assert_eq!(entries.recency().live_ids[second_slot], second_id);
        assert_eq!(entries.hint(), Some(second_slot));
    }
    cache.invalidate_page(state.pcid(), second_page, false);
    {
        let entries = cache.entries.borrow();
        let table = entries.recency();
        assert_eq!(table.live_ids[second_slot], 0);
        assert_ne!(
            second_record.tag,
            second_page | INDEXED_VALID | table.epoch,
            "retirement must reject the stale operand with a single tag comparison"
        );
    }
    let (reused_slot, new_id) = fill(second);
    assert_eq!(reused_slot, second_slot);
    assert_ne!(new_id, second_id);
    assert_eq!(
        cache.entries.borrow().recency().live_ids[reused_slot],
        new_id
    );
    cache.clear_indexed_pages();
    assert!(
        cache
            .entries
            .borrow()
            .recency()
            .entries
            .iter()
            .all(|record| record.tag == 0)
    );
    assert_eq!(
        cache.entries.borrow().recency().live_ids[reused_slot],
        new_id
    );
}

#[cfg(not(feature = "parallel"))]
#[test]
fn indexed_page_epoch_wrap_retires_both_access_banks() {
    let mut table = IndexedReadTable::default();
    table.entries[0] = IndexedReadEntry::new(0, 0, 0, 4096, false);
    table.writes[0] = IndexedReadEntry::new(0, 0, 0, 4096, true);

    for _ in 0..1024 {
        table.retire_pages();
    }

    assert_eq!(table.epoch, 0);
    assert_eq!((table.entries[0].tag, table.writes[0].tag), (0, 0));
}
