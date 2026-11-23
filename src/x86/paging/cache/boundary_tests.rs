use super::tests::fixture;
use super::*;
use crate::x86::paging::walk;

fn insert(
    cache: &Cache,
    image: &mut crate::memory::Image,
    state: &State,
    request: Request,
) -> Translation {
    cache.fill(
        state,
        request,
        walk::walk(image, state, request, &ProtectionKeys::ZERO).unwrap(),
    )
}

#[test]
fn fill_chooses_the_first_vacant_slot_then_the_first_oldest_slot() {
    let (mut image, state, first) = fixture();
    for (address, leaf) in [(0x4010, 0xb067), (0x4018, 0xc067), (0x4020, 0xd067)] {
        image.physical_write(address, 8, leaf).unwrap();
    }
    let at = |linear| Request { linear, ..first };
    let cache = Cache::with_capacity(3);
    let id = |translation: Translation| translation.cache_id.unwrap();
    let slots = || -> [Option<u64>; 3] {
        let entries = cache.entries.borrow();
        std::array::from_fn(|index| entries.slots[index].map(|entry| id(entry.translation)))
    };
    let first_id = id(insert(&cache, &mut image, &state, first));
    let second_id = id(insert(&cache, &mut image, &state, at(0x2234)));
    let third = at(0x3234);
    let third_id = id(insert(&cache, &mut image, &state, third));
    assert_eq!(slots(), [Some(first_id), Some(second_id), Some(third_id)]);
    assert!(!cache.take_changed());
    cache.invalidate_page(state.pcid(), first.linear, false);
    assert!(cache.take_changed());
    let replaced = id(insert(&cache, &mut image, &state, third));
    assert_eq!(replaced, third_id + 1);
    assert_eq!(slots(), [Some(replaced), Some(second_id), None]);
    assert!(cache.take_changed());

    let first_id = id(insert(&cache, &mut image, &state, first));
    assert_eq!(slots(), [Some(replaced), Some(second_id), Some(first_id)]);
    assert!(!cache.take_changed());
    {
        // Force a tie to verify the historical first-slot selection.
        let mut entries = cache.entries.borrow_mut();
        entries.recency_mut().ages[0] = 1;
        entries.recency_mut().ages[1] = 1;
    }
    let fourth_id = id(insert(&cache, &mut image, &state, at(0x4234)));
    assert_eq!(fourth_id, first_id + 1);
    assert_eq!(slots(), [Some(fourth_id), Some(second_id), Some(first_id)]);
    assert!(cache.take_changed());
}

#[test]
fn observation_does_not_fill_evict_or_promote_lru() {
    let (mut image, state, first) = fixture();
    image.physical_write(0x4010, 8, 0xb067).unwrap();
    image.physical_write(0x4018, 8, 0xc067).unwrap();
    let second = Request {
        linear: 0x2234,
        ..first
    };
    let third = Request {
        linear: 0x3234,
        ..first
    };
    let cache = Cache::with_capacity(2);
    insert(&cache, &mut image, &state, first);
    insert(&cache, &mut image, &state, second);
    assert_eq!(
        cache
            .observe(&state, first, &ProtectionKeys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xa234
    );
    assert!(
        cache
            .observe(&state, third, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
    insert(&cache, &mut image, &state, third);
    assert!(
        cache
            .lookup(&state, first, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .lookup(&state, second, &ProtectionKeys::ZERO)
            .unwrap()
            .is_some()
    );
    assert!(cache.take_changed());
    assert!(!cache.take_changed());
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_read_ages_data_then_code_only_on_success() {
    for clock_overflow in [false, true] {
        let (mut image, state, data) = fixture();
        image.physical_write(0x4010, 8, 0xb067).unwrap();
        image.physical_write(0x4018, 8, 0xc067).unwrap();
        let code = Request {
            linear: 0x2234,
            access: Access::Execute,
            ..data
        };
        let missing = Request {
            linear: 0x3234,
            ..data
        };
        let cache = Cache::with_capacity(2);
        insert(&cache, &mut image, &state, data);
        insert(&cache, &mut image, &state, code);
        assert!(cache.touch_current_fetch(&state, code, 0xb234));
        if clock_overflow {
            cache.entries.borrow_mut().recency_mut().clock = u64::MAX;
        }
        let clock = cache.entries.borrow().recency().clock;
        assert_eq!(
            cache.data_then_fetch::<false, u32>(
                &state,
                missing,
                &ProtectionKeys::ZERO,
                4,
                false,
                |_| unreachable!()
            ),
            None
        );
        assert_eq!(
            cache.data_then_fetch::<false, u32>(
                &state,
                data,
                &ProtectionKeys::ZERO,
                4,
                false,
                |_| None
            ),
            None
        );
        assert_eq!(cache.entries.borrow().recency().clock, clock);
        assert_eq!(
            cache.data_then_fetch::<false, u32>(
                &state,
                data,
                &ProtectionKeys::ZERO,
                4,
                false,
                |physical| {
                    assert_eq!(physical, 0xa234);
                    Some(u32::MAX)
                }
            ),
            Some(u32::MAX)
        );
        assert_eq!(
            cache.entries.borrow().recency().clock,
            if clock_overflow { 4 } else { clock + 2 }
        );
        insert(&cache, &mut image, &state, missing);
        assert!(
            cache
                .observe(&state, data, &ProtectionKeys::ZERO)
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .observe(&state, code, &ProtectionKeys::ZERO)
                .unwrap()
                .is_some()
        );
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_hits_on_the_newest_pair_preserve_all_victims_through_rollover() {
    for overflow in [false, true] {
        let (mut image, state, data) = fixture();
        for leaf in 2..=6 {
            image
                .physical_write(0x4000 + 8 * leaf, 8, ((0x9 + leaf) << 12) | 0x67)
                .unwrap();
        }
        let at = |page: u64| Request {
            linear: (page << 12) | 0x234,
            ..data
        };
        let code = Request {
            access: Access::Execute,
            ..at(2)
        };
        let oldest = at(3);
        let cache = Cache::with_capacity(3);
        for request in [oldest, data, code] {
            insert(&cache, &mut image, &state, request);
        }
        if overflow {
            let mut entries = cache.entries.borrow_mut();
            let offset = u64::MAX - entries.recency().clock;
            for index in 0..entries.capacity {
                if entries.slots[index].is_some() {
                    entries.recency_mut().ages[index] += offset;
                }
            }
            entries.recency_mut().clock = u64::MAX;
        }
        let clock = cache.entries.borrow().recency().clock;
        for operand in [at(2), data, at(2), data] {
            assert!(
                cache
                    .data_then_fetch::<false, _>(
                        &state,
                        operand,
                        &ProtectionKeys::ZERO,
                        4,
                        false,
                        Some,
                    )
                    .is_some()
            );
        }
        assert_eq!(cache.entries.borrow().recency().clock, clock);
        assert!(!cache.take_changed());
        for (page, victim) in (4..=6).zip([oldest, data, code]) {
            insert(&cache, &mut image, &state, at(page));
            assert!(
                cache
                    .observe(&state, victim, &ProtectionKeys::ZERO)
                    .unwrap()
                    .is_none()
            );
            assert!(cache.take_changed());
        }
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_read_span_rejects_global_or_split_without_touching_retained_ages() {
    let (mut image, mut state, data) = fixture();
    state.cr4 |= 1 << 7;
    image.physical_write(0x4008, 8, 0xa167).unwrap();
    image.physical_write(0x4010, 8, 0xb067).unwrap();
    let cache = Cache::with_capacity(2);
    let code = Request {
        linear: 0x2234,
        access: Access::Execute,
        ..data
    };
    insert(&cache, &mut image, &state, data);
    insert(&cache, &mut image, &state, code);
    assert!(cache.touch_current_fetch(&state, code, 0xb234));
    let clock = cache.entries.borrow().recency().clock;
    let split = Request {
        linear: 0x2ffc,
        ..data
    };
    for (request, local_only) in [(data, true), (split, false)] {
        assert_eq!(
            cache.data_then_fetch::<false, u32>(
                &state,
                request,
                &ProtectionKeys::ZERO,
                8,
                local_only,
                |_| { unreachable!("unproved spans never reach the physical backing") }
            ),
            None
        );
    }
    assert_eq!(cache.entries.borrow().recency().clock, clock);
    assert!(!cache.take_changed());
}

#[cfg(not(feature = "parallel"))]
#[test]
fn split_pair_ages_match_ordered_reads_when_data_and_fetch_alias() {
    for (other, code) in [
        (0x3000, 0x4000),
        (0x1100, 0x4000),
        (0x2100, 0x4000),
        (0x3000, 0x1100),
        (0x3000, 0x2100),
        (0x4100, 0x4000),
    ] {
        for starts in [[0x1fe0, other], [other, 0x1fe0]] {
            let order = |split_pair| {
                let (mut image, state, first) = fixture();
                for page in 2..=5 {
                    image
                        .physical_write(0x4000 + page * 8, 8, ((0x9 + page) << 12) | 0x67)
                        .unwrap();
                }
                let at = |linear| Request { linear, ..first };
                let fetch = Request {
                    access: Access::Execute,
                    ..at(code)
                };
                let cache = Cache::with_capacity(5);
                for page in [5, 3, 1, 4, 2] {
                    insert(&cache, &mut image, &state, at(page << 12));
                }
                assert!(cache.touch_current_fetch(&state, fetch, code + 0x9000));
                if split_pair {
                    assert_eq!(
                        cache.read_split_pair_then_fetch(
                            &state,
                            &ProtectionKeys::ZERO,
                            starts.map(|start| (at(start), 64)),
                            Some,
                        ),
                        Some(starts.map(|start| start + 0x9000)),
                    );
                } else {
                    for offset in (0..64).step_by(4) {
                        for start in starts {
                            assert!(
                                cache
                                    .lookup(&state, at(start + offset), &ProtectionKeys::ZERO)
                                    .unwrap()
                                    .is_some()
                            );
                            assert!(cache.touch_current_fetch(&state, fetch, code + 0x9000));
                        }
                    }
                }
                assert!(!cache.take_changed());
                let entries = cache.entries.borrow();
                let mut age: Vec<_> = entries
                    .slots
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| {
                        entry.map(|entry| {
                            (entries.recency().ages[index], entry.translation.linear_base)
                        })
                    })
                    .collect();
                age.sort_unstable();
                age.into_iter()
                    .map(|(_, linear)| linear)
                    .collect::<Vec<_>>()
            };
            assert_eq!(order(true), order(false), "{starts:x?}, code {code:x}");
        }
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn split_pair_refusal_preserves_ages_and_never_partially_resolves() {
    for case in [
        "missing",
        "noncontiguous",
        "global",
        "final-lap",
        "both",
        "physical",
    ] {
        let (mut image, mut state, first) = fixture();
        state.cr4 |= 1 << 7;
        let second = match case {
            "noncontiguous" => 0xf067,
            "global" => 0xb167,
            _ => 0xb067,
        };
        for (page, leaf) in [(2, second), (3, 0xc067), (4, 0xd067), (5, 0xe067)] {
            image.physical_write(0x4000 + page * 8, 8, leaf).unwrap();
        }
        let at = |linear| Request { linear, ..first };
        let cache = Cache::with_capacity(5);
        for page in [3, 2, 4, 1, 5] {
            if page != 2 || case != "missing" {
                insert(&cache, &mut image, &state, at(page << 12));
            }
        }
        let spans = match case {
            "final-lap" => [(at(0x1ff8), 16), (at(0x3000), 16)],
            "both" => [(at(0x1fe0), 64), (at(0x3fe0), 64)],
            _ => [(at(0x1fe0), 64), (at(0x3000), 64)],
        };
        let clock = cache.entries.borrow().recency().clock;
        assert_eq!(
            cache.read_split_pair_then_fetch::<()>(
                &state,
                &ProtectionKeys::ZERO,
                spans,
                |physical| {
                    assert_eq!(case, "physical");
                    assert_eq!(physical, [0xafe0, 0xc000]);
                    None
                },
            ),
            None,
        );
        assert_eq!(cache.entries.borrow().recency().clock, clock, "{case}");
        assert!(!cache.take_changed());
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn contiguous_retained_span_touches_three_pages_atomically_in_linear_order() {
    for layout in [
        "local",
        "missing",
        "noncontiguous",
        "global",
        "physical-denied",
    ] {
        let (mut image, mut state, first) = fixture();
        state.cr4 |= 1 << 7;
        let second_leaf = match layout {
            "noncontiguous" => 0xf067,
            "global" => 0xb167,
            _ => 0xb067,
        };
        for (address, leaf) in [
            (0x4010, second_leaf),
            (0x4018, 0xc067),
            (0x4020, 0xd067),
            (0x4028, 0xe067),
        ] {
            image.physical_write(address, 8, leaf).unwrap();
        }
        let at = |linear| Request { linear, ..first };
        let code = Request {
            access: Access::Execute,
            ..at(0x4234)
        };
        let cache = Cache::with_capacity(4);
        insert(&cache, &mut image, &state, at(0x3000));
        if layout != "missing" {
            insert(&cache, &mut image, &state, at(0x2000));
        }
        insert(&cache, &mut image, &state, first);
        insert(&cache, &mut image, &state, code);
        // A changed PTE does not replace the already retained translation.
        if layout == "local" {
            image.physical_write(0x4010, 8, 0xf007).unwrap();
        }
        let prior = cache.entries.borrow().recency().clock;
        let resolved = cache.contiguous_read_then_fetch(
            &state,
            at(0x1ffc),
            &ProtectionKeys::ZERO,
            0x1008,
            |physical| {
                assert!(matches!(layout, "local" | "physical-denied"));
                assert_eq!(physical, 0xaffc);
                (layout == "local").then_some(physical)
            },
        );
        assert_eq!(resolved, (layout == "local").then_some(0xaffc));
        if layout == "local" {
            insert(&cache, &mut image, &state, at(0x5234));
            assert!(
                cache
                    .observe(&state, first, &ProtectionKeys::ZERO)
                    .unwrap()
                    .is_none()
            );
            for request in [at(0x2000), at(0x3000), code] {
                assert!(
                    cache
                        .observe(&state, request, &ProtectionKeys::ZERO)
                        .unwrap()
                        .is_some()
                );
            }
            assert_eq!(image.physical_read(0x4010, 8), Ok(0xf007));
        } else {
            assert_eq!(cache.entries.borrow().recency().clock, prior, "{layout}");
            assert!(!cache.take_changed());
        }
    }
}

#[test]
fn current_pcid_non_global_precedes_global_and_targeted_eviction_keeps_global() {
    let (mut image, mut first, request) = fixture();
    first.cr4 |= (1 << 7) | (1 << 17);
    first.cr3 |= 1;
    let mut second = first;
    second.cr3 = 0x1002;
    image.physical_write(0x4008, 8, 0xa167).unwrap();
    let cache = Cache::default();
    insert(&cache, &mut image, &first, request);
    image.physical_write(0x4008, 8, 0xb067).unwrap();
    insert(&cache, &mut image, &second, request);
    assert_eq!(
        cache
            .lookup(&first, request, &ProtectionKeys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xa234
    );
    assert_eq!(
        cache
            .lookup(&second, request, &ProtectionKeys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xb234
    );
    cache.invalidate_page(2, 0x1fff, false);
    assert_eq!(
        cache
            .lookup(&second, request, &ProtectionKeys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xa234
    );
    cache.invalidate_non_global();
    assert_eq!(
        cache
            .lookup(&first, request, &ProtectionKeys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xa234
    );
    cache.invalidate_page(2, 0x1001, true);
    assert!(
        cache
            .lookup(&first, request, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
}

#[cfg(not(feature = "parallel"))]
#[test]
fn ready_dense_span_installs_only_after_all_three_pages_are_admitted() {
    for (capacity, last_pte, backing_ready, admit) in [
        (4, 0xc067, true, true),
        (3, 0xc067, true, false),
        (4, 0xc007, true, false),
        (4, 0xf067, true, false),
        (4, 0, true, false),
        (4, 0xc067, false, false),
    ] {
        let (mut image, state, first) = fixture();
        for (address, leaf) in [(0x4010, 0xb067), (0x4018, last_pte), (0x4020, 0xd067)] {
            image.physical_write(address, 8, leaf).unwrap();
        }
        let at = |linear| Request { linear, ..first };
        let code = Request {
            access: Access::Execute,
            ..at(0x4234)
        };
        let cache = Cache::with_capacity(capacity);
        insert(&cache, &mut image, &state, at(0x2000));
        insert(&cache, &mut image, &state, code);
        image.physical_write(0x4010, 8, 0xf007).unwrap();
        let snapshot = || {
            let entries = cache.entries.borrow();
            (entries.recency().clock, entries.next_id, entries.hint())
        };
        let before = snapshot();
        let reads = [at(0x1ff0), at(0x2000), at(0x3000)];
        let value = cache.ready_dense_read_then_fetch(
            &image,
            &state,
            reads[0],
            &ProtectionKeys::ZERO,
            0x1030,
            |physical| {
                assert_eq!(physical, 0xaff0);
                backing_ready.then_some(physical)
            },
        );
        assert_eq!(value, admit.then_some(0xaff0));
        assert_eq!(image.physical_read(0x4010, 8), Ok(0xf007));
        assert_eq!(image.physical_read(0x4018, 8), Ok(last_pte));
        assert!(!cache.take_changed());
        if admit {
            let entries = cache.entries.borrow();
            let ages = [reads[0], reads[1], reads[2], code]
                .map(|request| entries.recency().ages[entries.find(&state, request).unwrap()]);
            assert!(ages.windows(2).all(|ages| ages[0] < ages[1]));
            assert_eq!(entries.next_id, before.1 + 2);
            assert_eq!(entries.hint(), before.2);
        } else {
            assert_eq!(snapshot(), before);
            for request in [reads[0], reads[2]] {
                assert!(
                    cache
                        .observe(&state, request, &ProtectionKeys::ZERO)
                        .unwrap()
                        .is_none()
                );
            }
        }
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_store_requires_dirty_ready_writable_retention_before_aging() {
    for (leaf, width, admit) in [
        (0xa027, 1, false),
        (0xa065, 1, false),
        (0xa067, 1, true),
        (0xa027, 4, false),
        (0xa065, 4, false),
        (0xa067, 4, true),
    ] {
        let (mut image, state, data) = fixture();
        image.physical_write(0x4008, 8, leaf).unwrap();
        image.physical_write(0x4010, 8, 0xb067).unwrap();
        let code = Request {
            linear: 0x2234,
            access: Access::Execute,
            ..data
        };
        let store = Request {
            access: Access::Write,
            ..data
        };
        let cache = Cache::with_capacity(2);
        insert(&cache, &mut image, &state, data);
        insert(&cache, &mut image, &state, code);
        assert!(cache.touch_current_fetch(&state, code, 0xb234));
        let clock = cache.entries.borrow().recency().clock;
        let result = cache.data_then_fetch::<true, _>(
            &state,
            store,
            &ProtectionKeys::ZERO,
            width,
            false,
            |pa| {
                assert!(admit, "unproven stores leave their payload untouched");
                assert_eq!(pa, 0xa234);
                Some(())
            },
        );
        assert_eq!(result, admit.then_some(()));
        assert_eq!(
            cache.entries.borrow().recency().clock,
            clock + if admit { 2 } else { 0 }
        );
        assert_eq!(image.physical_read(0x4008, 8), Ok(leaf));
        assert!(!cache.take_changed());
    }
}

#[test]
fn original_large_translation_is_one_invalidation_extent() {
    let (mut image, state, request) = fixture();
    image.physical_write(0x3000, 8, 0x2000e7).unwrap();
    let cache = Cache::default();
    let first = insert(&cache, &mut image, &state, request);
    assert_eq!(
        (first.linear_base, first.physical_base, first.span),
        (0, 0x200000, 0x200000)
    );
    assert_eq!(
        cache
            .lookup(
                &state,
                Request {
                    linear: 0x1ffabc,
                    ..request
                },
                &ProtectionKeys::ZERO
            )
            .unwrap()
            .unwrap()
            .physical,
        0x3ffabc
    );
    cache.invalidate_page(0, 0x100000, false);
    assert!(
        cache
            .lookup(&state, request, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
}

#[test]
fn aggregate_denial_fault_evicts_once_and_observation_does_not() {
    let (mut image, state, read) = fixture();
    image.physical_write(0x2000, 8, 0x3025).unwrap();
    let cache = Cache::default();
    let translation = insert(&cache, &mut image, &state, read);
    assert!(!translation.rights.writable);
    assert!(!translation.dirty);
    image.physical_write(0x2000, 8, 0x3027).unwrap();
    let write = Request {
        access: Access::Write,
        ..read
    };
    assert!(cache.observe(&state, write, &ProtectionKeys::ZERO).is_err());
    assert!(
        cache
            .observe(&state, read, &ProtectionKeys::ZERO)
            .unwrap()
            .is_some()
    );
    assert!(cache.lookup(&state, write, &ProtectionKeys::ZERO).is_err());
    assert!(
        cache
            .lookup(&state, read, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
    assert!(
        walk::walk(&mut image, &state, write, &ProtectionKeys::ZERO)
            .unwrap()
            .rights
            .writable
    );
}

#[test]
fn accessed_zero_projection_cannot_become_a_ready_fill() {
    let (image, state, request) = fixture();
    let cache = Cache::default();
    let translation = walk::peek(&image, &state, request, &ProtectionKeys::ZERO).unwrap();
    assert!(!translation.accessed);
    assert_eq!(cache.fill(&state, request, translation).cache_id, None);
    assert!(
        cache
            .lookup(&state, request, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
}

#[test]
fn evicted_dirty_feedback_cannot_authorize_a_recycled_slot() {
    let (mut image, state, request) = fixture();
    image.physical_write(0x4008, 8, 0xa027).unwrap();
    image.physical_write(0x4010, 8, 0xb027).unwrap();
    let other = Request {
        linear: 0x2234,
        ..request
    };
    let cache = Cache::with_capacity(1);
    let old = insert(&cache, &mut image, &state, request);
    let replacement = insert(&cache, &mut image, &state, other);
    assert_ne!(old.cache_id, replacement.cache_id);
    cache.mark_dirty(old.cache_id, old.leaf);
    assert!(
        cache
            .lookup(
                &state,
                Request {
                    access: Access::Write,
                    ..other
                },
                &ProtectionKeys::ZERO
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn identity_exhaustion_drops_authority_without_reusing_a_captured_id() {
    let (mut image, state, request) = fixture();
    let cache = Cache::with_capacity(1);
    let old = insert(&cache, &mut image, &state, request);
    cache.entries.borrow_mut().next_id = u64::MAX;
    image.physical_write(0x4008, 8, 0xb067).unwrap();
    assert_eq!(insert(&cache, &mut image, &state, request).cache_id, None);
    cache.mark_dirty(old.cache_id, old.leaf);
    assert!(
        cache
            .lookup(&state, request, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
    cache.clear();
    assert_eq!(insert(&cache, &mut image, &state, request).cache_id, None);
}

#[test]
fn lru_clock_renormalization_preserves_actual_order() {
    let (mut image, state, first) = fixture();
    image.physical_write(0x4010, 8, 0xb067).unwrap();
    image.physical_write(0x4018, 8, 0xc067).unwrap();
    let second = Request {
        linear: 0x2234,
        ..first
    };
    let third = Request {
        linear: 0x3234,
        ..first
    };
    let cache = Cache::with_capacity(2);
    insert(&cache, &mut image, &state, first);
    insert(&cache, &mut image, &state, second);
    cache.entries.borrow_mut().recency_mut().clock = u64::MAX;
    cache.lookup(&state, first, &ProtectionKeys::ZERO).unwrap();
    insert(&cache, &mut image, &state, third);
    assert!(
        cache
            .lookup(&state, second, &ProtectionKeys::ZERO)
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .lookup(&state, first, &ProtectionKeys::ZERO)
            .unwrap()
            .is_some()
    );
}

#[cfg(not(feature = "parallel"))]
#[test]
fn paired_data_spans_commit_recency_only_after_both_succeed() {
    let (mut image, state, first) = fixture();
    for (address, pte) in [(0x4010, 0xb067), (0x4018, 0xc067), (0x4020, 0xd067)] {
        image.physical_write(address, 8, pte).unwrap();
    }
    let last = Request {
        linear: 0x2234,
        ..first
    };
    let code = Request {
        linear: 0x3234,
        access: Access::Execute,
        ..first
    };
    let extra = Request {
        linear: 0x4234,
        ..first
    };
    let cache = Cache::with_capacity(3);
    for request in [last, first, code] {
        insert(&cache, &mut image, &state, request);
    }
    let pair = [(first, 4), (last, 8)];
    let prior = cache.entries.borrow().recency().clock;
    assert_eq!(
        cache.read_pair_then_fetch::<()>(&state, &ProtectionKeys::ZERO, pair, |_| None),
        None
    );
    assert_eq!(cache.entries.borrow().recency().clock, prior);
    assert_eq!(
        cache.read_pair_then_fetch(&state, &ProtectionKeys::ZERO, pair, Some),
        Some([0xa234, 0xb234])
    );
    insert(&cache, &mut image, &state, extra);
    for (request, present) in [(first, false), (last, true), (code, true)] {
        assert_eq!(
            cache
                .observe(&state, request, &ProtectionKeys::ZERO)
                .unwrap()
                .is_some(),
            present
        );
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn a_cold_copy_source_cannot_keep_a_destination_the_load_would_evict() {
    let (mut image, state, read) = fixture();
    let write = Request {
        linear: 0x2234,
        access: Access::Write,
        ..read
    };
    let code = Request {
        linear: 0x3234,
        access: Access::Execute,
        ..read
    };
    let other = Request {
        linear: 0x4234,
        ..read
    };
    let keys = &ProtectionKeys::ZERO;
    let cache = Cache::with_capacity(3);
    for (entry, word) in [(0x4010, 0xb067), (0x4018, 0xc067), (0x4020, 0xd067)] {
        image.physical_write(entry, 8, word).unwrap();
    }
    for request in [write, other, code] {
        insert(&cache, &mut image, &state, request);
    }
    let clock = cache.entries.borrow().recency().clock;
    assert_eq!(
        cache.ready_copy_operand(
            &mut image,
            &state,
            [read, write],
            keys,
            16,
            |_, _, _, _, _| panic!("the checked source allocation would evict the destination")
        ),
        None
    );
    assert_eq!(cache.entries.borrow().recency().clock, clock);
    for request in [write, code] {
        cache.lookup(&state, request, keys).unwrap().unwrap();
    }
    assert_eq!(
        cache.ready_copy_operand(
            &mut image,
            &state,
            [read, write],
            keys,
            16,
            |_, r, w, earlier_writes, _| {
                assert_eq!((r, w, earlier_writes), (0xa234, 0xb234, 0));
                Some(())
            }
        ),
        Some(())
    );
    assert!(cache.observe(&state, other, keys).unwrap().is_none());
    for request in [read, write, code] {
        assert!(cache.observe(&state, request, keys).unwrap().is_some());
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn a_ready_copy_fills_the_destination_after_admitting_both_operands() {
    let (mut image, state, read) = fixture();
    let write = Request {
        linear: 0x2234,
        access: Access::Write,
        ..read
    };
    let code = Request {
        linear: 0x3234,
        access: Access::Execute,
        ..read
    };
    let other = Request {
        linear: 0x4234,
        ..read
    };
    let keys = &ProtectionKeys::ZERO;
    let cache = Cache::with_capacity(3);
    image.physical_write(0x4018, 8, 0xc067).unwrap();
    image.physical_write(0x4020, 8, 0xd067).unwrap();
    for request in [read, other, code] {
        insert(&cache, &mut image, &state, request);
    }
    let before = cache.entries.borrow().recency().clock;
    for leaf in [0xb007, 0xb027, 0xb065, 0xb067] {
        image.physical_write(0x4010, 8, leaf).unwrap();
        assert_eq!(
            cache.ready_copy_operand(
                &mut image,
                &state,
                [read, write],
                keys,
                32,
                |_, _, _, _, _| {
                    assert_eq!(leaf, 0xb067);
                    None
                }
            ),
            None
        );
        assert_eq!(image.physical_read(0x4010, 8).unwrap(), leaf);
        assert_eq!(cache.entries.borrow().recency().clock, before);
        assert!(cache.observe(&state, write, keys).unwrap().is_none());
    }
    assert_eq!(
        cache.ready_copy_operand(
            &mut image,
            &state,
            [read, write],
            keys,
            32,
            |_, source, destination, _, _| ((source, destination) == (0xa234, 0xb234))
                .then_some(())
        ),
        Some(())
    );
    assert!(cache.observe(&state, other, keys).unwrap().is_none());
    for request in [read, write, code] {
        assert!(cache.observe(&state, request, keys).unwrap().is_some());
    }
    let entries = cache.entries.borrow();
    let ages = [read, write, code]
        .map(|request| entries.recency().ages[entries.find(&state, request).unwrap()]);
    assert!(ages[0] < ages[1] && ages[1] < ages[2]);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn a_split_destination_fills_only_after_its_prefix_and_walk_are_admitted() {
    let (mut image, state, read) = fixture();
    let write = Request {
        linear: 0x2ff8,
        access: Access::Write,
        ..read
    };
    let next = Request {
        linear: 0x3000,
        ..write
    };
    let code = Request {
        linear: 0x4234,
        access: Access::Execute,
        ..read
    };
    let cache = Cache::with_capacity(4);
    let keys = &ProtectionKeys::ZERO;
    image.physical_write(0x4010, 8, 0xb067).unwrap();
    image.physical_write(0x4020, 8, 0xe067).unwrap();
    for request in [read, write, code] {
        insert(&cache, &mut image, &state, request);
    }
    let before = cache.entries.borrow().recency().clock;
    for leaf in [0xd067, 0xc027, 0xc067] {
        image.physical_write(0x4018, 8, leaf).unwrap();
        assert_eq!(
            cache.ready_copy_operand(
                &mut image,
                &state,
                [read, write],
                keys,
                32,
                |_, source, dest, prefix, walked| {
                    assert_eq!(leaf, 0xc067);
                    assert_eq!((source, dest, prefix), (0xa234, 0xbff8, 8));
                    assert!(walked.entries().contains(&0x4018));
                    None
                }
            ),
            None
        );
        assert_eq!(cache.entries.borrow().recency().clock, before);
        assert!(cache.observe(&state, next, keys).unwrap().is_none());
    }
    cache
        .ready_copy_operand(
            &mut image,
            &state,
            [read, write],
            keys,
            32,
            |_, _, _, _, _| Some(()),
        )
        .unwrap();
    assert_eq!(
        cache.copy_split_pair_then_fetch(&state, [read, write], keys, 32, [16; 4], |a, b| Some((
            a, b
        ))),
        Some((0xa234, 0xbff8))
    );
}

#[cfg(not(feature = "parallel"))]
#[test]
fn retained_copy_destination_checks_the_current_write_permission() {
    let (mut image, state, read) = fixture();
    image.physical_write(0x4010, 8, 0xb067).unwrap();
    image.physical_write(0x4018, 8, 0xc067).unwrap();
    let write = Request {
        linear: 0x2234,
        access: Access::Write,
        ..read
    };
    let code = Request {
        linear: 0x3234,
        access: Access::Execute,
        ..read
    };
    let cache = Cache::with_capacity(3);
    let keys = &ProtectionKeys::ZERO;
    for request in [read, write, code] {
        insert(&cache, &mut image, &state, request);
    }
    let resolve = |write| {
        cache.copy_pair_then_fetch(
            &state,
            [read, write],
            keys,
            16,
            |[(source, _), (destination, _)], _| Some((source, destination)),
        )
    };
    assert_eq!(resolve(write), Some((0xa234, 0xb234)));
    assert_eq!(
        resolve(Request {
            linear: 0x223c,
            ..write
        }),
        Some((0xa234, 0xb23c))
    );

    cache.invalidate_page(state.pcid(), write.linear, true);
    image.physical_write(0x4010, 8, 0xd065).unwrap();
    let readonly = Request {
        access: Access::Read,
        ..write
    };
    insert(&cache, &mut image, &state, readonly);
    assert!(cache.lookup(&state, code, keys).unwrap().is_some());
    let before = cache.entries.borrow().recency().clock;
    assert_eq!(resolve(write), None);
    assert_eq!(cache.entries.borrow().recency().clock, before);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn copy_sources_resolve_current_leaves_after_a_collision() {
    let (mut image, state, first) = fixture();
    for (address, leaf) in [(0x4010, 0xb067), (0x4018, 0xc067), (0x4088, 0xd067)] {
        image.physical_write(address, 8, leaf).unwrap();
    }
    let second = Request {
        linear: 0x11234,
        ..first
    };
    let write = Request {
        linear: 0x2234,
        access: Access::Write,
        ..first
    };
    let code = Request {
        linear: 0x3234,
        access: Access::Execute,
        ..first
    };
    let cache = Cache::with_capacity(4);
    let keys = &ProtectionKeys::ZERO;
    for request in [first, second, write, code] {
        insert(&cache, &mut image, &state, request);
    }
    let copy =
        |read| cache.copy_pair_then_fetch(&state, [read, write], keys, 16, |spans, _| Some(spans));
    assert_eq!(copy(first), Some([(0xa234, 0xdcc), (0xb234, 0xdcc)]));
    assert_eq!(copy(second), Some([(0xd234, 0xdcc), (0xb234, 0xdcc)]));
    assert_eq!(copy(first), Some([(0xa234, 0xdcc), (0xb234, 0xdcc)]));

    cache.invalidate_page(state.pcid(), first.linear, true);
    image.physical_write(0x4008, 8, 0xe067).unwrap();
    insert(&cache, &mut image, &state, first);
    assert!(cache.lookup(&state, code, keys).unwrap().is_some());
    assert_eq!(copy(first), Some([(0xe234, 0xdcc), (0xb234, 0xdcc)]));
}

#[cfg(not(feature = "parallel"))]
#[test]
fn same_large_copy_leaf_checks_both_ranges_and_current_mapping() {
    let (mut image, state, read) = fixture();
    image.physical_write(0x3000, 8, 0x2000e7).unwrap();
    image.physical_write(0x3008, 8, 0x4000e7).unwrap();
    let write = Request {
        linear: 0x5234,
        access: Access::Write,
        ..read
    };
    let code = Request {
        linear: 0x200200,
        access: Access::Execute,
        ..read
    };
    let cache = Cache::with_capacity(2);
    let keys = &ProtectionKeys::ZERO;
    insert(&cache, &mut image, &state, write);
    insert(&cache, &mut image, &state, code);
    let resolve = |read| {
        cache.copy_pair_then_fetch(
            &state,
            [read, write],
            keys,
            16,
            |[(source, _), (destination, _)], _| Some((source, destination)),
        )
    };
    assert_eq!(resolve(read), Some((0x201234, 0x205234)));
    let before = cache.entries.borrow().recency().clock;
    assert_eq!(
        resolve(Request {
            linear: 0x1ffff8,
            ..read
        }),
        None
    );
    assert_eq!(cache.entries.borrow().recency().clock, before);

    cache.invalidate_page(state.pcid(), write.linear, true);
    image.physical_write(0x3000, 8, 0x6000e7).unwrap();
    insert(&cache, &mut image, &state, write);
    assert!(cache.lookup(&state, code, keys).unwrap().is_some());
    assert_eq!(resolve(read), Some((0x601234, 0x605234)));
}

#[cfg(not(feature = "parallel"))]
#[test]
fn split_copy_preflights_rights_and_keeps_ordered_recency() {
    let (mut image, state, request) = fixture();
    for (pte, leaf) in [
        (0x4010, 0xb067),
        (0x4018, 0xc067),
        (0x4020, 0xd067),
        (0x4028, 0xe067),
    ] {
        image.physical_write(pte, 8, leaf).unwrap();
    }
    let cache = Cache::with_capacity(5);
    let at = |linear, access| Request {
        linear,
        access,
        ..request
    };
    let read = at(0x1ff8, Access::Read);
    let write = at(0x3ff0, Access::Write);
    let code = at(0x5002, Access::Execute);
    let keys = &ProtectionKeys::ZERO;
    for operand in [
        read,
        at(0x2000, Access::Read),
        write,
        at(0x4000, Access::Write),
        code,
    ] {
        insert(&cache, &mut image, &state, operand);
    }
    let clock = || cache.entries.borrow().recency().clock;
    let start = clock();
    assert_eq!(
        cache.copy_split_pair_then_fetch(
            &state,
            [read, write],
            keys,
            32,
            [16; 4],
            |_, _| None::<()>
        ),
        None
    );
    assert_eq!(clock(), start);
    assert_eq!(
        cache.copy_split_pair_then_fetch(
            &state,
            [read, write],
            keys,
            32,
            [16; 4],
            |source, destination| {
                assert_eq!((source, destination), (0xaff8, 0xcff0));
                Some(())
            }
        ),
        Some(())
    );
    {
        let entries = cache.entries.borrow();
        let mut order = [0, 1, 2, 3, 4];
        order.sort_unstable_by_key(|&slot| entries.recency().ages[slot]);
        assert_eq!(order, [0, 2, 1, 3, 4]);
    }

    // The last two-byte write starts on the second page after the last read.
    assert_eq!(
        cache.copy_split_pair_then_fetch(&state, [read, write], keys, 18, [8, 8, 2, 0], |_, _| {
            Some(())
        }),
        Some(())
    );
    {
        let entries = cache.entries.borrow();
        let mut order = [0, 1, 2, 3, 4];
        order.sort_unstable_by_key(|&slot| entries.recency().ages[slot]);
        assert_eq!(order, [0, 2, 1, 3, 4]);
    }

    cache.invalidate_page(state.pcid(), 0x4000, false);
    image.physical_write(0x4020, 8, 0xd065).unwrap();
    insert(&cache, &mut image, &state, at(0x4000, Access::Read));
    assert!(cache.lookup(&state, code, keys).unwrap().is_some());
    let start = clock();
    assert_eq!(
        cache.copy_split_pair_then_fetch(
            &state,
            [read, write],
            keys,
            32,
            [16; 4],
            |_, _| -> Option<()> {
                unreachable!("a denied second destination leaf cannot reach backing admission")
            }
        ),
        None
    );
    assert_eq!(clock(), start);
}
