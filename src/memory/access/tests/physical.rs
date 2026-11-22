use crate::memory::{Access, Cause, Fault, Image, MappingInput, MemoryLimits, Permissions};

const END: u64 = 1 << 52;
const HOLE: u64 = 0x0000_8000_0000_0000;

fn mapping(base: u64, offset: u32, length: u32, permissions: Permissions) -> MappingInput {
    MappingInput {
        base,
        backing: "bytes".into(),
        offset,
        length,
        permissions,
    }
}

fn all() -> Permissions {
    Permissions {
        read: true,
        write: true,
        execute: true,
    }
}

fn physical(bytes: Vec<u8>, mappings: Vec<MappingInput>) -> Image {
    Image::physical_mapped(
        vec![("bytes".into(), bytes)],
        mappings,
        MemoryLimits {
            max_backing_bytes: 32,
            max_backings: 1,
            max_mappings: 8,
        },
    )
    .unwrap()
}

#[test]
fn physical_sparse_high_addresses_and_the_last_byte_are_not_linear_addresses() {
    let mut image = physical(
        (0..16).collect(),
        vec![
            mapping(HOLE, 0, 4, all()),
            mapping(1 << 51, 4, 4, all()),
            mapping(END - 8, 8, 8, all()),
        ],
    );
    assert_eq!(image.physical_fetch(HOLE + 1), Ok(1));
    assert_eq!(image.physical_read(1 << 51, 4), Ok(0x0706_0504));
    assert_eq!(image.physical_read(END - 8, 8), Ok(0x0f0e_0d0c_0b0a_0908));
    image.physical_write(END - 1, 1, 0xa5).unwrap();
    assert_eq!(image.physical_read_bytes(END - 2, 2), Ok(vec![14, 0xa5]));
    assert_eq!(image.read(HOLE, 1).unwrap_err().cause, Cause::Noncanonical);
    for (address, width) in [(END - 1, 2), (END, 1), (u64::MAX, 8)] {
        let fault = image.physical_read(address, width).unwrap_err();
        assert_eq!(
            (fault.address, fault.width, fault.cause),
            (address, width, Cause::AddressRange)
        );
        assert_eq!(fault.failing, u128::from(address.max(END)));
    }
}

#[test]
fn physical_read_into_checks_the_whole_span_before_copying_split_backings() {
    let first = mapping(HOLE, 5, 2, all());
    let second = mapping(HOLE + 2, 1, 3, all());
    let mut image = physical((0..8).collect(), vec![first.clone(), second.clone()]);
    let mut output = [0xff; 5];
    image.physical_read_into(HOLE, &mut output).unwrap();
    assert_eq!(output, [5, 6, 1, 2, 3]);
    assert_eq!(image.physical_read_bytes(HOLE, 5).unwrap(), output);

    let denied = MappingInput {
        permissions: Permissions {
            read: false,
            ..all()
        },
        ..second
    };
    image.set_mappings(vec![first, denied]).unwrap();
    let expected = image.physical_read_bytes(HOLE, 5).unwrap_err();
    assert_eq!(expected.failing, u128::from(HOLE + 2));
    assert_eq!(image.physical_read_into(HOLE, &mut output), Err(expected));
    assert_eq!(output, [5, 6, 1, 2, 3]);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn mapped_dword_leaves_split_and_denied_operands_for_the_full_reader() {
    let mut image = physical((0..16).collect(), vec![mapping(END - 8, 1, 8, all())]);
    assert_eq!(image.physical_read_mapped_dword(END - 4), Some(0x0807_0605));
    assert_eq!(image.physical_read_mapped_dword(END - 3), None);
    assert_eq!(image.physical_read_mapped_dword(END), None);

    let first = mapping(HOLE, 5, 2, all());
    let mut second = mapping(HOLE + 2, 1, 3, all());
    image
        .set_mappings(vec![first.clone(), second.clone()])
        .unwrap();
    assert_eq!(image.physical_read_mapped_dword(HOLE), None);
    assert_eq!(image.physical_read(HOLE, 4), Ok(0x0201_0605));

    second.permissions.read = false;
    image.set_mappings(vec![first, second]).unwrap();
    assert_eq!(image.physical_read_mapped_dword(HOLE), None);
    let fault = image.physical_read(HOLE, 4).unwrap_err();
    assert_eq!(
        (fault.cause, fault.failing),
        (Cause::Permission, u128::from(HOLE + 2))
    );
}

#[cfg(not(feature = "parallel"))]
#[test]
fn direct_physical_byte_store_requires_plain_writable_ram() {
    let rw = Permissions {
        execute: false,
        ..all()
    };
    let data = mapping(HOLE, 1, 4, rw);
    let alias = mapping(HOLE + 0x1000, 5, 1, all());
    let mut image = physical((0..8).collect(), vec![data.clone(), alias]);
    assert_eq!(image.physical_direct_write(HOLE + 1, [0xaa]), None);
    assert_eq!(
        image.physical_read_mapped_byte_for_store(HOLE + 1, || true),
        Some((2, None))
    );
    assert_eq!(image.physical_read(HOLE + 1, 1), Ok(2));

    image.set_mappings(vec![data.clone()]).unwrap();
    assert_eq!(image.physical_direct_write(HOLE + 1, [0xaa]), Some(()));
    assert_eq!(image.physical_read(HOLE + 1, 1), Ok(0xaa));
    assert_eq!(
        image.physical_read_mapped_byte_for_store(HOLE + 1, || false),
        Some((0xaa, None))
    );
    assert_eq!(image.physical_direct_write(HOLE + 4, [0xbb]), None);
    assert_eq!(
        image.physical_read_mapped_byte_for_store(HOLE + 4, || true),
        None
    );

    let owner = image.register_dependencies([], [(HOLE, 4)]).unwrap();
    assert!(image.activate(owner));
    assert_eq!(
        image.physical_read_mapped_byte_for_store(HOLE + 1, || true),
        Some((0xaa, None))
    );
    assert!(image.owner_valid(owner));
    image.release(owner);
    let (value, pointer) = image
        .physical_read_mapped_byte_for_store(HOLE + 1, || true)
        .unwrap();
    assert_eq!(value, 0xaa);
    // This is the direct store's entire lease, with no intervening Image use.
    unsafe { pointer.unwrap().write(0x5c) };
    assert_eq!(image.physical_read(HOLE + 1, 1), Ok(0x5c));

    let denied = MappingInput {
        permissions: Permissions { write: false, ..rw },
        ..data.clone()
    };
    image.set_mappings(vec![denied]).unwrap();
    assert_eq!(image.physical_direct_write(HOLE + 1, [0xbb]), None);
    assert_eq!(
        image.physical_read_mapped_byte_for_store(HOLE + 1, || true),
        Some((0x5c, None))
    );
    assert_eq!(image.physical_read(HOLE + 1, 1), Ok(0x5c));

    let unreadable = MappingInput {
        permissions: Permissions { read: false, ..rw },
        ..data
    };
    image.set_mappings(vec![unreadable]).unwrap();
    assert_eq!(
        image.physical_read_mapped_byte_for_store(HOLE + 1, || panic!(
            "read must be admitted first"
        )),
        None
    );
}

#[cfg(not(feature = "parallel"))]
#[test]
fn direct_physical_dword_store_preserves_mapping_and_dependency_boundaries() {
    let rw = Permissions {
        execute: false,
        ..all()
    };
    let data = mapping(HOLE, 0, 4, rw);
    let next = mapping(HOLE + 4, 4, 4, rw);
    let value = 0xaabb_ccddu32.to_le_bytes();
    let mut image = physical((0..8).collect(), vec![data.clone(), next.clone()]);
    assert_eq!(image.physical_direct_write(HOLE + 2, value), None);
    assert_eq!(image.physical_read(HOLE, 8), Ok(0x0706_0504_0302_0100));

    let owner = image.register_dependencies([], [(HOLE + 4, 4)]).unwrap();
    assert_eq!(image.physical_direct_write(HOLE, value), None);
    assert!(image.owner_valid(owner));
    assert_eq!(image.physical_read(HOLE, 4), Ok(0x0302_0100));
    image.release(owner);
    assert_eq!(image.physical_direct_write(HOLE, value), Some(()));
    assert_eq!(image.physical_read(HOLE, 4), Ok(0xaabb_ccdd));
}

#[test]
fn physical_mapping_replacement_rejects_domain_overflow_atomically() {
    let mut image = physical(vec![0x90; 8], vec![mapping(HOLE, 0, 8, all())]);
    for candidate in [
        mapping(END - 3, 0, 4, all()),
        mapping(END, 0, 1, all()),
        mapping(u64::MAX, 0, 2, all()),
    ] {
        assert!(image.set_mappings(vec![candidate]).is_err());
        assert_eq!(image.physical_fetch(HOLE), Ok(0x90));
    }
    image
        .set_mappings(vec![mapping(END - 8, 0, 8, all())])
        .unwrap();
    assert_eq!(image.physical_fetch(END - 1), Ok(0x90));
    assert!(
        Image::mapped(
            vec![("bytes".into(), vec![0; 8])],
            vec![mapping(HOLE, 0, 8, all())],
            MemoryLimits {
                max_backing_bytes: 8,
                max_backings: 1,
                max_mappings: 1
            },
        )
        .is_err()
    );
}

#[cfg(not(feature = "parallel"))]
#[test]
fn mapped_byte_probe_covers_only_readable_physical_ram() {
    let unreadable = Permissions {
        read: false,
        ..all()
    };
    let mut image = physical(
        (0..8).collect(),
        vec![
            mapping(HOLE, 0, 4, all()),
            mapping(HOLE + 4, 4, 2, unreadable),
            mapping(END - 1, 7, 1, all()),
        ],
    );
    image
        .install_mmio(vec![crate::memory::MmioRange {
            base: HOLE + 6,
            length: 1,
        }])
        .unwrap();
    for address in [HOLE - 1, HOLE, HOLE + 3, HOLE + 4, HOLE + 6, END - 1, END] {
        let expected = image
            .physical_read(address, 1)
            .ok()
            .map(|value| value as u8);
        assert_eq!(image.physical_read_mapped_byte(address), expected);
        assert_eq!(
            image.physical_read_mapped_byte_for_store(address, || false),
            expected.map(|value| (value, None))
        );
    }
    let mut linear = Image::new(0x1000, vec![0x90], 0x2000, vec![1]).unwrap();
    assert_eq!(linear.physical_read_mapped_byte(0x2000), None);
    assert_eq!(
        linear.physical_read_mapped_byte_for_store(0x2000, || true),
        None
    );
}

#[test]
fn physical_alias_writes_preserve_backing_offsets_and_source_invalidation() {
    let mut image = physical(
        (0..16).collect(),
        vec![mapping(HOLE, 3, 4, all()), mapping(0x1000, 2, 8, all())],
    );
    let owner = image.register_source([(HOLE, 4)]).unwrap();
    assert!(image.activate(owner));
    image.physical_write_bytes(0x1002, &[0xa1, 0xb2]).unwrap();
    assert_eq!(
        image.physical_read_bytes(HOLE, 4),
        Ok(vec![3, 0xa1, 0xb2, 6])
    );
    assert!(!image.owner_valid(owner));
    assert!(image.active_changed());
}

#[test]
fn physical_accesses_check_the_actual_private_permission_and_full_span_before_writing() {
    let mut image = physical(
        vec![1, 2, 3, 4],
        vec![
            mapping(HOLE, 0, 2, all()),
            mapping(
                HOLE + 2,
                2,
                2,
                Permissions {
                    read: true,
                    write: false,
                    execute: false,
                },
            ),
        ],
    );
    assert_eq!(image.physical_read(HOLE, 4), Ok(0x0403_0201));
    assert_eq!(image.physical_check_read(HOLE, 4), Ok(()));
    let fault = image.physical_write(HOLE, 4, 0xaaaa_bbbb).unwrap_err();
    assert_eq!(
        (fault.access, fault.cause, fault.failing),
        (Access::Write, Cause::Permission, u128::from(HOLE + 2))
    );
    assert_eq!(image.physical_read_bytes(HOLE, 4), Ok(vec![1, 2, 3, 4]));
    assert_eq!(image.physical_check_write(HOLE, 4), Err(fault));
    assert_eq!(
        image.physical_check_execute(HOLE, 4).unwrap_err().failing,
        u128::from(HOLE + 2)
    );
    assert_eq!(
        image.physical_fetch(HOLE + 2).unwrap_err().access,
        Access::Execute
    );
    assert_eq!(
        image.physical_read(HOLE + 4, 1).unwrap_err().cause,
        Cause::Unmapped
    );

    image
        .set_mappings(vec![mapping(
            HOLE,
            0,
            4,
            Permissions {
                read: false,
                write: true,
                execute: false,
            },
        )])
        .unwrap();
    image.physical_write(HOLE, 1, 0xcc).unwrap();
    assert_eq!(
        image.physical_read(HOLE, 1).unwrap_err().cause,
        Cause::Permission
    );
}

#[test]
fn physical_rmw_admission_keeps_the_first_combined_read_write_failure() {
    let mut image = physical(
        vec![1, 2, 3, 4],
        vec![
            mapping(
                HOLE,
                0,
                2,
                Permissions {
                    read: false,
                    write: true,
                    execute: false,
                },
            ),
            mapping(
                HOLE + 2,
                2,
                2,
                Permissions {
                    read: true,
                    write: false,
                    execute: false,
                },
            ),
        ],
    );
    assert_eq!(
        image.physical_check_write(HOLE, 4).unwrap_err().failing,
        u128::from(HOLE + 2)
    );
    let expected = Fault {
        address: HOLE,
        width: 4,
        access: Access::Write,
        cause: Cause::Permission,
        failing: u128::from(HOLE),
    };
    assert_eq!(
        image.physical_check_access(HOLE, 4, Access::Write, true),
        Err(expected)
    );
    assert_eq!(image.physical_read_for_write(HOLE, 4), Err(expected));
    image
        .set_mappings(vec![mapping(HOLE, 0, 4, all())])
        .unwrap();
    assert_eq!(image.physical_read_for_write(HOLE, 4), Ok(0x0403_0201));
    assert_eq!(image.physical_read_bytes(HOLE, 4), Ok(vec![1, 2, 3, 4]));
}
