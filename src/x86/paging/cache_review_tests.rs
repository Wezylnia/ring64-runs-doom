use super::*;
use crate::memory::{Image, MappingInput, MemoryLimits, Permissions};
use crate::x86::paging::walk::{self, AccessClass, PageFault};

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
        walk::walk(&mut image, &origin, read).unwrap(),
    );
    let current = State {
        cr3: 0x5002,
        ..origin
    };
    assert_eq!(
        cache.lookup(&current, read).unwrap().unwrap().physical,
        0xa234
    );
    let write = Request {
        access: Access::Write,
        ..read
    };
    assert_eq!(
        cache.lookup(&current, write),
        Err(Failure::Page(PageFault {
            address: 0x1234,
            access: Access::Write,
            error_code: 3,
        }))
    );
    assert!(cache.observe(&current, read).unwrap().is_none());
    assert!(cache.observe(&origin, read).unwrap().is_none());
    let fresh = walk::walk(&mut image, &current, write).unwrap();
    assert_eq!(fresh.physical, 0xb234);
    cache.fill(&current, write, fresh);
    assert_eq!(
        cache.lookup(&current, write).unwrap().unwrap().physical,
        0xb234
    );
}

#[test]
fn captured_dirty_feedback_cannot_mark_a_new_fill_with_the_same_leaf_slot() {
    let (mut image, state, read) = fixture();
    let cache = Cache::with_capacity(1);
    let old = cache.fill(&state, read, walk::walk(&mut image, &state, read).unwrap());
    cache.invalidate_page(0, read.linear, false);
    image.physical_write(0x4008, 8, 0xb027).unwrap();
    let replacement = cache.fill(&state, read, walk::walk(&mut image, &state, read).unwrap());
    assert_eq!(old.leaf, replacement.leaf);
    assert_ne!(old.cache_id, replacement.cache_id);
    cache.mark_dirty(old);
    let retained = cache.observe(&state, read).unwrap().unwrap();
    assert_eq!(retained.physical, 0xb234);
    assert!(retained.dirty);
    assert!(
        cache
            .lookup(
                &state,
                Request {
                    access: Access::Write,
                    ..read
                }
            )
            .unwrap()
            .is_none()
    );
}
