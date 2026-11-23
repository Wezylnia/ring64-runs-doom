use super::super::*;
use super::{fixture, insert};

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
        cache.copy_split_pair_then_fetch(&state, [read, write], keys, 32, [16; 4], |a, b, _| Some(
            (a, b)
        )),
        Some((0xa234, 0xbff8))
    );
}

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
            |_, _, _| None::<()>
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
            |source, destination, last| {
                assert_eq!((source, destination), (0xaff8, 0xcff0));
                assert_eq!(last, [1, 3]);
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
        cache.copy_split_pair_then_fetch(
            &state,
            [read, write],
            keys,
            18,
            [8, 8, 2, 0],
            |_, _, _| { Some(()) }
        ),
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
            |_, _, _| -> Option<()> {
                unreachable!("a denied second destination leaf cannot reach backing admission")
            }
        ),
        None
    );
    assert_eq!(clock(), start);
}
