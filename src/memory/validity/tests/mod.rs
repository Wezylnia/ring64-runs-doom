use super::*;

mod dependencies;

#[test]
fn reports_source_invalidation_only_when_an_owner_becomes_stale() {
    use super::super::{MappingInput, Permissions};
    let mut image = Image::new(0x1000, vec![0; 16], 0x4000, vec![0; 8]).unwrap();
    let code = MappingInput {
        base: 0x1000,
        backing: "code".into(),
        offset: 0,
        length: 16,
        permissions: Permissions {
            read: true,
            write: true,
            execute: true,
        },
    };
    image.set_mappings(vec![code.clone()]).unwrap();
    let first = image.register_source([(0x1000, 4)]).unwrap();
    let second = image.register_source([(0x1008, 4)]).unwrap();
    image.activate(first);
    image.write(0x1004, 1, 7).unwrap();
    image.set_mappings(vec![code.clone()]).unwrap();
    assert!(!image.take_source_invalidation());

    image.write(0x1008, 1, 1).unwrap();
    assert!(image.take_source_invalidation());
    assert!(image.owner_valid(first));
    assert!(!image.owner_valid(second));
    assert!(!image.active_changed());
    image.write(0x1008, 1, 2).unwrap();
    assert!(!image.take_source_invalidation());

    image
        .set_mappings(vec![MappingInput {
            length: 2,
            ..code.clone()
        }])
        .unwrap();
    assert!(image.take_source_invalidation());
    assert!(!image.owner_valid(first));
    image.set_mappings(vec![code]).unwrap();
    assert!(!image.take_source_invalidation());

    let fresh = image.register_source([(0x1000, 4)]).unwrap();
    assert!(image.activate(fresh));
    image.invalidate_sources();
    assert!(image.take_source_invalidation());
    image.invalidate_sources();
    assert!(!image.take_source_invalidation());
    assert!(image.active_changed());
}

#[test]
fn repeated_runtime_fences_keep_the_active_exit_until_a_new_activation() {
    let mut image = Image::new(0x1000, vec![0x90; 8], 0x4000, vec![0; 8]).unwrap();
    let owner = image.register_source([(0x1000, 1)]).unwrap();
    assert!(image.activate(owner));
    image.invalidate_sources();
    image.invalidate_sources();
    assert!(image.active_changed());
    assert!(!image.owner_valid(owner));
    let fresh = image.register_source([(0x1000, 1)]).unwrap();
    assert!(image.activate(fresh));
    assert!(!image.active_changed());
}

#[test]
fn source_union_retains_gaps_and_reclaims_released_owners() {
    let mut image = Image::new(0x1000, vec![0; 64], 0x4000, vec![0; 8]).unwrap();
    image
        .set_mappings(vec![super::super::MappingInput {
            base: 0x1000,
            backing: "code".into(),
            offset: 0,
            length: 64,
            permissions: super::super::Permissions {
                read: true,
                write: true,
                execute: true,
            },
        }])
        .unwrap();
    let first = image.register_source([(0x1000, 4)]).unwrap();
    let second = image.register_source([(0x1010, 4)]).unwrap();
    let key = Rc::as_ptr(&image.backings[0].incarnation) as usize;
    assert_eq!(image.validity.sources[&key], [(0, 4), (16, 20)]);
    let bridge = image.register_source([(0x1004, 12)]).unwrap();
    assert_eq!(image.validity.sources[&key], [(0, 20)]);
    image.release(bridge);
    assert_eq!(image.validity.sources[&key], [(0, 4), (16, 20)]);
    image.activate(first);
    image.write(0x1008, 1, 7).unwrap();
    assert!(image.owner_valid(first) && image.owner_valid(second));
    image.write(0x1011, 1, 8).unwrap();
    assert!(image.owner_valid(first));
    assert!(!image.owner_valid(second));
    assert!(!image.active_changed());
    image.release(second);
    assert_eq!(image.validity.sources[&key], [(0, 4)]);
    image.release(first);
    assert!(image.validity.sources.is_empty());
}

#[test]
fn repeated_source_writes_cache_only_a_span_without_live_dependencies() {
    use super::super::{MappingInput, Permissions};
    let mut image = Image::new(0x1000, vec![0; 32], 0x4000, vec![0; 8]).unwrap();
    let permissions = Permissions {
        read: true,
        write: true,
        execute: true,
    };
    let mappings = vec![
        MappingInput {
            base: 0x1000,
            backing: "code".into(),
            offset: 0,
            length: 32,
            permissions,
        },
        MappingInput {
            base: 0x4000,
            backing: "ram".into(),
            offset: 0,
            length: 8,
            permissions,
        },
    ];
    image.set_mappings(mappings.clone()).unwrap();
    let active = image.register_source([(0x1000, 4)]).unwrap();
    let other = image.register_source([(0x1010, 4)]).unwrap();
    let separate = image.register_source([(0x4000, 4)]).unwrap();
    image.activate(active);
    image.write(0x1010, 4, 7).unwrap();
    assert!(!image.owner_valid(other));
    assert!(image.owner_valid(active));
    let completed = image.validity.completed_write;
    assert!(completed.is_some());
    for _ in 0..32 {
        image.write(0x1011, 1, 8).unwrap();
    }
    assert_eq!(image.validity.completed_write, completed);
    assert!(!image.active_changed());
    // A matching backing offset in another incarnation is never cached out.
    image.write(0x4000, 4, 9).unwrap();
    assert!(!image.owner_valid(separate));
    let replacement = image.register_source([(0x1010, 4)]).unwrap();
    assert!(image.validity.completed_write.is_none());
    assert!(!image.backings[1].has_sources);
    image.write(0x1011, 1, 10).unwrap();
    assert!(!image.owner_valid(replacement));
    image.release(replacement);
    assert!(image.validity.completed_write.is_none());
    image.write(0x1011, 1, 11).unwrap();
    image.set_mappings(mappings).unwrap();
    assert!(image.validity.completed_write.is_none());
    assert!(image.owner_valid(active));
}

#[test]
fn exhausted_compile_identity_never_breaks_a_guest_store_or_reuses_a_token() {
    let mut image = Image::new(0x1000, vec![0xb8, 1, 0, 0, 0], 0x4000, vec![0; 8]).unwrap();
    let owner = image.register_source([(0x1000, 5)]).unwrap();
    image.activate(owner);
    image.validity.next = u64::MAX;
    assert!(matches!(
        image.register_source([(0x1000, 5)]),
        Err(MemoryError::Capacity(_))
    ));
    assert!(image.owner_valid(owner));
    assert_eq!(image.write(0x4000, 8, 0x1234), Ok(()));
    assert_eq!(image.read(0x4000, 8), Ok(0x1234));
    image.release(owner);
    assert!(!image.owner_valid(owner));
}
