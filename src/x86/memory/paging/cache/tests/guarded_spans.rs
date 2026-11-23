use super::super::*;
use super::{fixture, insert};

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
