use super::*;
use crate::memory::{Image, MappingInput, MemoryLimits, Permissions};
use crate::x86::paging::walk::{self, AccessClass, PageFault};
use crate::x86::protection_keys::State as Keys;

fn fixture() -> (Image, State, Request) {
    let mut bytes = vec![0; 0x10000];
    for (address, value) in [
        (0x1000, 0x2027u64),
        (0x2000, 0x3027),
        (0x3000, 0x4027),
        (0x4008, 0xa027),
        (0x5000, 0x6027),
        (0x6000, 0x7027),
        (0x7000, 0x8027),
        (0x8008, 0xb067),
    ] {
        bytes[address..address + 8].copy_from_slice(&value.to_le_bytes());
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
fn guarded_byte_store_slot_keeps_write_authority_and_fetch_recency() {
    use std::cell::Cell;

    for (leaf, admitted) in [(0xa067, true), (0xa027, false), (0xa065, false)] {
        let (mut image, state, read) = fixture();
        image.physical_write(0x4008, 8, leaf).unwrap();
        image.physical_write(0x4010, 8, 0xc027).unwrap();
        let fetch = Request {
            linear: 0x2000,
            access: Access::Execute,
            ..read
        };
        let cache = Cache::with_capacity(2);
        for request in [read, fetch] {
            let entry = walk::peek(&image, &state, request, &Keys::ZERO).unwrap();
            cache.fill(&state, request, entry);
        }
        let slot = cache
            .data_byte_then_fetch::<false, _>(
                &state,
                read,
                &Keys::ZERO,
                None,
                |slot, translation| {
                    assert_eq!(translation.physical, 0xa234);
                    Some(slot)
                },
            )
            .unwrap();
        let write = Request {
            access: Access::Write,
            ..read
        };
        let oldest_order = {
            let entries = cache.entries.borrow();
            (entries.clock, entries.hint)
        };
        for selected in [slot, 1 - slot, usize::MAX] {
            let used = Cell::new(false);
            let result = cache.data_byte_then_fetch::<true, _>(
                &state,
                write,
                &Keys::ZERO,
                Some(selected),
                |actual_slot, translation| {
                    assert_eq!(actual_slot, slot);
                    used.set(true);
                    Some(translation.physical)
                },
            );
            assert_eq!(result, admitted.then_some(0xa234));
            assert_eq!(used.get(), admitted);
            let entries = cache.entries.borrow();
            assert_eq!((entries.clock, entries.hint), oldest_order);
        }
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_dword_store_memo_rechecks_evicted_and_replaced_slots() {
    let (mut image, mut first, read) = fixture();
    first.cr4 |= 1 << 17;
    first.cr3 = 0x1001;
    let second = State {
        cr3: 0x5002,
        ..first
    };
    for (address, value) in [(0x4008, 0xa067), (0x4010, 0xc027), (0x8010, 0xd027)] {
        image.physical_write(address, 8, value).unwrap();
    }
    let cache = Cache::with_capacity(2);
    let fetch = Request {
        linear: 0x2000,
        access: Access::Execute,
        ..read
    };
    let write = Request {
        access: Access::Write,
        ..read
    };
    let fill = |image: &Image, state, request| {
        let translation = walk::peek(image, state, request, &Keys::ZERO).unwrap();
        cache.fill(state, request, translation);
    };
    let store = |state| cache.write_dword_then_fetch(state, write, &Keys::ZERO, Some);
    fill(&image, &first, read);
    fill(&image, &first, fetch);
    assert_eq!(store(&first), Some(0xa234));
    let original_slot = cache.last_dword_write.get().unwrap();

    // A full-cache insertion evicts the memo's local occupant.
    fill(&image, &second, read);
    let index = usize::from(original_slot.get() - 1);
    assert_eq!(
        cache.entries.borrow().slots[index].unwrap().pcid,
        second.pcid()
    );
    assert_eq!(store(&first), None);
    assert_eq!(cache.last_dword_write.get(), Some(original_slot));
    fill(&image, &second, fetch);
    assert_eq!(store(&second), Some(0xb234));

    cache.invalidate_pcid(second.pcid());
    assert_eq!(store(&second), None);
    for leaf in [0xa027, 0xa065] {
        image.physical_write(0x4008, 8, leaf).unwrap();
        fill(&image, &first, read);
        fill(&image, &first, fetch);
        let clock = cache.entries.borrow().clock;
        assert_eq!(cache.last_dword_write.get(), Some(original_slot));
        assert_eq!(
            cache.write_dword_then_fetch(&first, write, &Keys::ZERO, |_| {
                panic!("Write rights or a pending dirty update must refuse the fast path")
            }),
            None::<()>
        );
        assert_eq!(cache.entries.borrow().clock, clock);
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_dword_store_memo_preserves_backing_admission_and_recency() {
    let (mut image, state, read) = fixture();
    image
        .set_mappings(vec![MappingInput {
            base: 0,
            backing: "ram".into(),
            offset: 0,
            length: 0x10000,
            permissions: Permissions {
                read: true,
                write: true,
                execute: false,
            },
        }])
        .unwrap();
    for (address, value) in [(0x4008, 0xa067), (0x4010, 0xc027), (0x4018, 0xd027)] {
        image.physical_write(address, 8, value).unwrap();
    }
    let cache = Cache::with_capacity(3);
    let fetch = Request {
        linear: 0x2000,
        access: Access::Execute,
        ..read
    };
    let write = Request {
        access: Access::Write,
        ..read
    };
    let unrelated = Request {
        linear: 0x3000,
        ..read
    };
    for request in [read, fetch] {
        let translation = walk::peek(&image, &state, request, &Keys::ZERO).unwrap();
        cache.fill(&state, request, translation);
    }
    let store = |image: &mut Image, value: u32| {
        cache.write_dword_then_fetch(&state, write, &Keys::ZERO, |physical| {
            image.physical_direct_write(physical, value.to_le_bytes())
        })
    };
    assert_eq!(store(&mut image, 0x11223344), Some(()));
    let other = walk::peek(&image, &state, unrelated, &Keys::ZERO).unwrap();
    cache.fill(&state, unrelated, other);
    cache.lookup(&state, fetch, &Keys::ZERO).unwrap().unwrap();
    let clock = cache.entries.borrow().clock;
    let owner = image.register_dependencies([], [(0xa234, 4)]).unwrap();
    assert!(image.activate(owner));
    assert_eq!(store(&mut image, 0xdeadbeef), None);
    assert_eq!(cache.entries.borrow().clock, clock);
    assert!(image.source_is_active(owner));
    assert_eq!(image.physical_read(0xa234, 4), Ok(0x11223344));

    image.release(owner);
    assert_eq!(store(&mut image, 0x55667788), Some(()));
    assert_eq!(image.physical_read(0xa234, 4), Ok(0x55667788));
    let entries = cache.entries.borrow();
    let data = entries.find(&state, write).unwrap();
    let code = entries.find(&state, fetch).unwrap();
    assert_eq!(entries.clock, clock + 2);
    assert_eq!(entries.hint, Some(code));
    assert_eq!(entries.slots[data].unwrap().used, entries.clock - 1);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_dword_candidates_revalidate_removals_and_later_ordinary_fills() {
    let (mut image, mut origin, request) = fixture();
    origin.cr4 |= (1 << 7) | (1 << 17);
    origin.cr3 = 0x1001;
    let local = State {
        cr3: 0x5002,
        ..origin
    };
    image.physical_write(0x4008, 8, 0xa127).unwrap();
    image.physical_write(0x4010, 8, 0xc027).unwrap();
    let cache = Cache::with_capacity(2);
    let check = |state: &State, request: Request, expected| {
        let mut entries = cache.entries.borrow_mut();
        entries.hint = None;
        let mut candidates = cache.guarded_candidates.borrow_mut();
        let found = candidates.find(&entries, state, request);
        assert_eq!(found, entries.find(state, request));
        assert_eq!(
            found.map(|index| entries.slots[index]
                .unwrap()
                .translation
                .at(request.linear)
                .physical),
            expected,
        );
    };
    let fill = |state, request| {
        cache.fill(
            state,
            request,
            walk::peek(&image, state, request, &Keys::ZERO).unwrap(),
        );
    };
    fill(&origin, request);
    fill(&local, request);
    check(&local, request, Some(0xb234));
    check(&origin, request, Some(0xa234));
    cache.invalidate_pcid(local.pcid());
    check(&local, request, Some(0xa234));
    let adjacent = Request {
        linear: 0x2344,
        ..request
    };
    fill(&origin, adjacent);
    check(&origin, adjacent, Some(0xc344));
    check(&local, adjacent, None);
    cache.clear();
    check(&origin, adjacent, None);
    check(&local, request, None);
}

#[test]
fn stale_global_denial_evicts_originating_other_pcid_before_fresh_root_retry() {
    let (mut image, mut origin, user) = fixture();
    origin.cr0 |= 1 << 16;
    origin.cr4 |= (1 << 7) | (1 << 17);
    origin.cr3 = 0x1001;
    image.physical_write(0x4008, 8, 0xa161).unwrap();
    let read = Request {
        class: AccessClass::Supervisor { ac: false },
        ..user
    };
    let cache = Cache::default();
    cache.fill(
        &origin,
        read,
        walk::walk(&mut image, &origin, read, &Keys::ZERO).unwrap(),
    );
    let current = State {
        cr3: 0x5002,
        ..origin
    };
    assert_eq!(
        cache
            .lookup(&current, read, &Keys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xa234
    );
    let write = Request {
        access: Access::Write,
        ..read
    };
    assert_eq!(
        cache.lookup(&current, write, &Keys::ZERO),
        Err(Failure::Page(PageFault {
            address: 0x1234,
            access: Access::Write,
            error_code: 3,
        }))
    );
    assert!(
        cache
            .observe(&current, read, &Keys::ZERO)
            .unwrap()
            .is_none()
    );
    assert!(cache.observe(&origin, read, &Keys::ZERO).unwrap().is_none());
    let fresh = walk::walk(&mut image, &current, write, &Keys::ZERO).unwrap();
    assert_eq!(fresh.physical, 0xb234);
    cache.fill(&current, write, fresh);
    assert_eq!(
        cache
            .lookup(&current, write, &Keys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xb234
    );
}

#[test]
fn a_local_large_page_fill_signals_when_it_shadows_a_smaller_global() {
    let (mut image, mut origin, read) = fixture();
    origin.cr4 |= (1 << 7) | (1 << 17);
    origin.cr3 = 0x1001;
    image.physical_write(0x4008, 8, 0xa127).unwrap();
    let cache = Cache::default();
    cache.fill(
        &origin,
        read,
        walk::walk(&mut image, &origin, read, &Keys::ZERO).unwrap(),
    );
    assert!(!cache.take_changed());
    let current = State {
        cr3: 0x5002,
        ..origin
    };
    for address in [0x7000, 0x7008] {
        image.physical_write(address, 8, 0xa7).unwrap();
    }
    assert_eq!(
        cache
            .observe(&current, read, &Keys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xa234
    );
    // Both reads install into free slots. Only the second large leaf covers
    // the global code page, which remains available to the original PCID.
    for (linear, shadows) in [(0x20_2234, false), (0x2234, true)] {
        let adjacent = Request { linear, ..read };
        assert!(
            cache
                .lookup(&current, adjacent, &Keys::ZERO)
                .unwrap()
                .is_none()
        );
        let walked = walk::walk(&mut image, &current, adjacent, &Keys::ZERO).unwrap();
        image
            .physical_check_access(walked.physical, 1, Access::Read, false)
            .unwrap();
        cache.fill(&current, adjacent, walked);
        assert_eq!(cache.take_changed(), shadows);
    }
    assert_eq!(
        cache
            .observe(&current, read, &Keys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0x1234
    );
    assert_eq!(
        cache
            .observe(&origin, read, &Keys::ZERO)
            .unwrap()
            .unwrap()
            .physical,
        0xa234
    );
    #[cfg(not(feature = "parallel"))]
    {
        let local = cache
            .data_translation_then_fetch::<false, _>(&current, read, &Keys::ZERO, 4, true, Some)
            .unwrap();
        assert_eq!(
            (local.linear_base, local.physical_base, local.span),
            (0, 0, 0x20_0000)
        );
        let old_clock = cache.entries.borrow().clock;
        assert!(
            cache
                .data_translation_then_fetch::<false, _>(
                    &origin,
                    read,
                    &Keys::ZERO,
                    4,
                    true,
                    |_| { Some(()) }
                )
                .is_none(),
        );
        assert_eq!(cache.entries.borrow().clock, old_clock);
    }
}

#[test]
fn captured_dirty_feedback_cannot_mark_a_new_fill_with_the_same_leaf_slot() {
    let (mut image, state, read) = fixture();
    let cache = Cache::with_capacity(1);
    let old = cache.fill(
        &state,
        read,
        walk::walk(&mut image, &state, read, &Keys::ZERO).unwrap(),
    );
    cache.invalidate_page(0, read.linear, false);
    image.physical_write(0x4008, 8, 0xb027).unwrap();
    let replacement = cache.fill(
        &state,
        read,
        walk::walk(&mut image, &state, read, &Keys::ZERO).unwrap(),
    );
    assert_eq!(old.leaf, replacement.leaf);
    assert_ne!(old.cache_id, replacement.cache_id);
    cache.mark_dirty(old.cache_id, old.leaf);
    let retained = cache.observe(&state, read, &Keys::ZERO).unwrap().unwrap();
    assert_eq!(retained.physical, 0xb234);
    assert!(retained.dirty);
    assert!(
        cache
            .lookup(
                &state,
                Request {
                    access: Access::Write,
                    ..read
                },
                &Keys::ZERO
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn observational_peek_miss_does_not_install_an_already_accessed_translation() {
    let (image, state, request) = fixture();
    let cache = Cache::default();
    let before = image.physical_read_bytes(0, 0x10000).unwrap();
    let observed = cache.peek(&image, &state, request, &Keys::ZERO).unwrap();
    assert_eq!(observed.physical, 0xa234);
    assert!(
        cache
            .observe(&state, request, &Keys::ZERO)
            .unwrap()
            .is_none(),
        "an observational miss must not become a retained translation fill"
    );
    assert!(!cache.take_changed());
    assert_eq!(image.physical_read_bytes(0, 0x10000).unwrap(), before);
}
