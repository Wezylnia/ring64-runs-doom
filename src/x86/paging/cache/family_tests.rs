use super::tests::fixture;
use super::*;
use crate::x86::paging::walk::{self, AccessClass, PageFault};

#[test]
fn every_existing_page_geometry_retains_exact_original_extent_and_permissions() {
    for (kind, expected_span, expected_physical) in [
        (0, 0x1000, 0xa234),
        (1, 0x1000, 0xa234),
        (2, 0x1000, 0xa234),
        (3, 0x400000, 0x401234),
        (4, 0x200000, 0x201234),
        (5, 0x200000, 0x201234),
        (6, 0x40000000, 0x40001234),
        (7, 0x1000, 0xa234),
    ] {
        let (mut image, mut state, mut request) = fixture();
        match kind {
            0 | 3 => {
                state.efer = 0;
                state.cr4 &= !(1 << 5);
                image
                    .physical_write(0x1000, 4, if kind == 3 { 0x4000e7 } else { 0x4007 })
                    .unwrap();
                image.physical_write(0x4004, 4, 0xa067).unwrap();
                if kind == 3 {
                    state.cr4 |= 1 << 4;
                }
            }
            1 | 4 => {
                state.efer = 0;
                state.pdpte = Some([0x3001, 0, 0, 0]);
                if kind == 4 {
                    image.physical_write(0x3000, 8, 0x2000e7).unwrap();
                }
            }
            5 => image.physical_write(0x3000, 8, 0x2000e7).unwrap(),
            6 => image.physical_write(0x2000, 8, 0x400000e7).unwrap(),
            7 => {
                request.linear = 0xffff800000001234;
                image.physical_write(0x1800, 8, 0x2007).unwrap();
            }
            _ => {}
        }
        let cache = Cache::default();
        let walked = walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap();
        assert_eq!(
            (walked.span, walked.physical),
            (expected_span, expected_physical)
        );
        cache.fill(&state, request, walked);
        let hit = cache
            .lookup(&state, request, &ProtectionKeys::ZERO)
            .unwrap()
            .unwrap();
        assert_eq!((hit.span, hit.physical), (expected_span, expected_physical));
        assert_eq!(
            hit.rights,
            walk::Rights {
                user: true,
                writable: true,
                executable: true,
                key: 0
            }
        );
        cache.invalidate_page(
            0,
            request.linear + (expected_span - (request.linear & (expected_span - 1))) - 1,
            true,
        );
        assert!(
            cache
                .lookup(&state, request, &ProtectionKeys::ZERO)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn upper_nx_and_live_supervisor_classes_keep_literal_fault_codes() {
    let (mut image, mut state, request) = fixture();
    state.efer |= 1 << 11;
    image.physical_write(0x1000, 8, 0x8000000000002027).unwrap();
    let cache = Cache::default();
    let read = walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap();
    assert!(!read.rights.executable);
    cache.fill(&state, request, read);
    assert_eq!(
        cache.lookup(
            &state,
            Request {
                access: Access::Execute,
                ..request
            },
            &ProtectionKeys::ZERO
        ),
        Err(Failure::Page(PageFault {
            address: 0x1234,
            access: Access::Execute,
            error_code: 21
        }))
    );
    image.physical_write(0x1000, 8, 0x2027).unwrap();
    cache.fill(
        &state,
        request,
        walk::walk(&mut image, &state, request, &ProtectionKeys::ZERO).unwrap(),
    );
    state.cr4 |= 1 << 21;
    let explicit = Request {
        class: AccessClass::Supervisor { ac: true },
        ..request
    };
    assert!(
        cache
            .lookup(&state, explicit, &ProtectionKeys::ZERO)
            .unwrap()
            .is_some()
    );
    let implicit = Request {
        class: AccessClass::ImplicitSupervisor,
        ..request
    };
    assert_eq!(
        cache.lookup(&state, implicit, &ProtectionKeys::ZERO),
        Err(Failure::Page(PageFault {
            address: 0x1234,
            access: Access::Read,
            error_code: 1
        }))
    );
}
