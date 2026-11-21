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
            cache.entries.borrow_mut().clock = u64::MAX;
        }
        let clock = cache.entries.borrow().clock;
        assert_eq!(
            cache.read_then_fetch(
                &state,
                missing,
                &ProtectionKeys::ZERO,
                4,
                |_| unreachable!()
            ),
            None
        );
        assert_eq!(
            cache.read_then_fetch(&state, data, &ProtectionKeys::ZERO, 4, |_| None),
            None
        );
        assert_eq!(cache.entries.borrow().clock, clock);
        assert_eq!(
            cache.read_then_fetch(&state, data, &ProtectionKeys::ZERO, 4, |physical| {
                assert_eq!(physical, 0xa234);
                Some(u32::MAX)
            }),
            Some(u32::MAX)
        );
        assert_eq!(
            cache.entries.borrow().clock,
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
    cache.entries.borrow_mut().clock = u64::MAX;
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
