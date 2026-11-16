use super::*;
use crate::memory::{MappingInput, MemoryLimits, Permissions};
use crate::x86::alignment;
use crate::x86::paging::{
    State,
    access::{AccessState, Memory},
};

fn image() -> Image {
    Image::physical_mapped(
        vec![("ram".into(), vec![0xa5; 0x10000])],
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
            max_mappings: 4,
        },
    )
    .unwrap()
}

fn controls() -> State {
    State {
        cr0: 0x40011,
        ..State::reset()
    }
}

#[test]
fn explicit_system_record_checks_fields_after_nominal_admission_without_partial_payload() {
    for kind in [ProtectionKind::StoreGdt, ProtectionKind::StoreIdt] {
        for (offset, fault) in [
            (0x6000, Some((0x6002, 4, 4))),
            (0x6001, Some((0x6001, 2, 2))),
            (0x6002, None),
        ] {
            let mut image = image();
            let mut context = tests::context("protected", 3);
            let table = TableRegister {
                base: 0x11223344,
                limit: 0xabcd,
            };
            context.set_gdtr(table);
            context.set_idtr(table);
            let before = context.clone();
            let state = controls();
            let mut work = AccessState::default();
            let mut memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work);
            let result = execute(
                &mut context,
                &mut memory,
                ProtectionDescriptor {
                    kind,
                    width: Width::W32,
                    memory: true,
                },
                offset,
                0,
                Segment::Ds,
            );
            if let Some((address, width, alignment)) = fault {
                assert_eq!(
                    result,
                    Err(Failure::Operand(AccessFailure::AlignmentCheck(
                        alignment::Fault {
                            address,
                            width,
                            access: Access::Write,
                            alignment
                        }
                    )))
                );
            } else {
                assert_eq!(result, Ok(Outcome::default()));
            }
            assert_eq!(context, before);
            assert_eq!(
                image.physical_read_bytes(0x6000, 10).unwrap(),
                if fault.is_some() {
                    vec![0xa5; 10]
                } else {
                    vec![0xa5, 0xa5, 0xcd, 0xab, 0x44, 0x33, 0x22, 0x11, 0xa5, 0xa5]
                }
            );
        }
    }
}

#[test]
fn explicit_selector_and_far_pointer_do_not_inherit_metadata_exemption() {
    for (kind, width, offset, expected_width, expected_alignment, access) in [
        (
            ProtectionKind::ReadTask,
            Width::W16,
            0x6001,
            2,
            2,
            Access::Write,
        ),
        (
            ProtectionKind::ReadLdt,
            Width::W16,
            0x6001,
            2,
            2,
            Access::Write,
        ),
        (ProtectionKind::Lar, Width::W32, 0x6001, 2, 2, Access::Read),
        (
            ProtectionKind::LoadFar(Segment::Fs),
            Width::W16,
            0x6001,
            4,
            2,
            Access::Read,
        ),
        (
            ProtectionKind::LoadFar(Segment::Fs),
            Width::W32,
            0x6002,
            6,
            4,
            Access::Read,
        ),
    ] {
        let mut image = image();
        let mut context = tests::context("protected", 3);
        let before = context.clone();
        let state = controls();
        let mut work = AccessState::default();
        let mut memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work);
        assert_eq!(
            execute(
                &mut context,
                &mut memory,
                ProtectionDescriptor {
                    kind,
                    width,
                    memory: true
                },
                offset,
                0,
                Segment::Ds
            ),
            Err(Failure::Operand(AccessFailure::AlignmentCheck(
                alignment::Fault {
                    address: offset,
                    width: expected_width,
                    access,
                    alignment: expected_alignment
                }
            )))
        );
        assert_eq!(context, before);
        assert_eq!(
            image.physical_read_bytes(0x6000, 10).unwrap(),
            vec![0xa5; 10]
        );
    }
}

#[test]
fn system_privilege_and_complete_record_backing_refusal_precede_field_alignment() {
    let mut image = image();
    image
        .set_mappings(vec![
            MappingInput {
                base: 0,
                backing: "ram".into(),
                offset: 0,
                length: 0x6005,
                permissions: Permissions {
                    read: true,
                    write: true,
                    execute: true,
                },
            },
            MappingInput {
                base: 0x6005,
                backing: "ram".into(),
                offset: 0x6005,
                length: 1,
                permissions: Permissions {
                    read: true,
                    write: false,
                    execute: true,
                },
            },
        ])
        .unwrap();
    let mut context = tests::context("protected", 3);
    let before = context.clone();
    let state = controls();
    let mut work = AccessState::default();
    let mut memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work);
    assert!(matches!(
        execute(
            &mut context,
            &mut memory,
            ProtectionDescriptor {
                kind: ProtectionKind::StoreGdt,
                width: Width::W32,
                memory: true
            },
            0x6000,
            0,
            Segment::Ds
        ),
        Err(Failure::Operand(AccessFailure::Paging(_)))
    ));
    assert_eq!(
        execute(
            &mut context,
            &mut memory,
            ProtectionDescriptor {
                kind: ProtectionKind::LoadGdt,
                width: Width::W32,
                memory: true
            },
            0x6001,
            0,
            Segment::Ds
        ),
        Err(Failure::Protection(ProtectionFault {
            vector: 13,
            error_code: 0,
            phase: ProtectionPhase::Privilege,
            selector: None
        }))
    );
    assert_eq!(context, before);
    assert_eq!(image.physical_read_bytes(0x6000, 6).unwrap(), vec![0xa5; 6]);
}

#[test]
fn far_pointer_and_return_stack_reads_keep_their_ordinary_alignment_intent() {
    use crate::x86::instruction::{FarDescriptor, FarKind};
    for (kind, memory_operand, address, width, granule) in [
        (FarKind::Jump, true, 0x6002, 6, 4),
        (FarKind::Return, false, 0x6001, 4, 4),
    ] {
        let mut image = image();
        let mut context = tests::context("protected", 3);
        let before = context.clone();
        let state = controls();
        let mut work = AccessState::default();
        let mut memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work);
        assert_eq!(
            far::execute(
                &mut context,
                &mut memory,
                FarDescriptor {
                    kind,
                    width: Width::W32,
                    memory: memory_operand
                },
                far::FarOperands {
                    offset: address,
                    selector: 8,
                    input_sp: address,
                    next_pc: 0x1000,
                    segment: Segment::Ds
                }
            ),
            Err(far::FarFailure::Operand {
                segment: if memory_operand {
                    Segment::Ds
                } else {
                    Segment::Ss
                },
                failure: AccessFailure::AlignmentCheck(alignment::Fault {
                    address,
                    width,
                    access: Access::Read,
                    alignment: granule
                })
            })
        );
        assert_eq!(context, before);
    }
}
