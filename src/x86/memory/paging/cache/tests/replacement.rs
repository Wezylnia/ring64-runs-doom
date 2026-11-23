use super::super::*;
use super::{fixture, insert};
use crate::x86::paging::walk;

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
