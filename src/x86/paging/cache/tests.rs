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
fn read_span_touches_only_the_authorized_whole_operand() {
    let (mut image, mut state, request) = fixture();
    let cache = Cache::default();
    let translation = walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap();
    cache.fill(&state, request, translation);
    let clock = cache.entries.borrow().clock;
    assert_eq!(
        cache.touch_read_span(&state, request, &ProtectionKeys::ZERO, 16),
        Some(0xa234)
    );
    assert_eq!(cache.entries.borrow().clock, clock + 1);
    for linear in [0x1fff, 0x2000] {
        assert_eq!(
            cache.touch_read_span(
                &state,
                Request { linear, ..request },
                &ProtectionKeys::ZERO,
                2
            ),
            None
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
        None
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
