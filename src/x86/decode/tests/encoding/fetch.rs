use super::super::*;

#[test]
fn multibyte_nop_consumes_its_operand_before_lock_or_length_faults() {
    for bytes in [
        &[0x0f, 0x1f, 0xc0][..],
        &[0x4f, 0x0f, 0x1f, 0xc7],
        &[0x64, 0x67, 0x0f, 0x1f, 0x44, 0x25, 0x7f],
        &[0x66, 0x0f, 0x1f, 0x84, 0x00, 1, 2, 3, 4],
        &[0x0f, 0x1f, 0x05, 1, 2, 3, 4],
    ] {
        assert_eq!(operation(bytes), Operation::Nop);
        let locked = [&[0xf0][..], bytes].concat();
        assert_eq!(at(0x1000, &locked), Err(DecodeError::Invalid));
        for encoding in [bytes, &locked] {
            for len in 0..encoding.len() {
                assert_eq!(
                    at(0x1000, &encoding[..len]),
                    Err(DecodeError::Fetch(missing(0x1000 + len as u64))),
                );
            }
        }
    }
    let mut longest = [0x66; 15];
    longest[7..].copy_from_slice(&[0x0f, 0x1f, 0x84, 0, 0, 0, 0, 0]);
    assert_eq!(operation(&longest), Operation::Nop);
    let overlong_locked = [&[0xf0][..], &longest].concat();
    assert_eq!(
        at(0x1000, &overlong_locked),
        Err(DecodeError::InstructionLength)
    );

    for selector in 1..=7 {
        assert_eq!(
            at(0x1000, &[0x0f, 0x1f, selector << 3 | 4]),
            Err(DecodeError::Unsupported)
        );
    }
    for repeat in [0xf2, 0xf3] {
        assert_eq!(
            at(0x1000, &[repeat, 0x0f, 0x1f]),
            Err(DecodeError::Unsupported)
        );
    }
}

#[test]
fn ud2_has_a_selected_exception_frontier_without_admitting_unknown_opcodes() {
    for bytes in [&[0x0f, 0x0b][..], &[0xf0, 0x0f, 0x0b], &[0x66, 0x0f, 0x0b]] {
        assert_eq!(at(0x1000, bytes), Err(DecodeError::Undefined));
        assert_eq!(
            at(0x1000, &bytes[..bytes.len() - 1]),
            Err(DecodeError::Fetch(missing(0x1000 + bytes.len() as u64 - 1)))
        );
    }
    for bytes in [
        &[0xf3, 0x0f, 0x0b][..],
        &[0xf0, 0xf0, 0x0f, 0x0b],
        &[0x0f, 0xff],
    ] {
        assert_eq!(at(0x1000, bytes), Err(DecodeError::Unsupported));
    }
}

#[test]
fn halt_decodes_ignored_prefixes_and_keeps_lock_as_a_selected_undefined_frontier() {
    for bytes in [&[0xf4][..], &[0x66, 0x67, 0xf3, 0x64, 0x48, 0xf4]] {
        assert_eq!(operation(bytes), Operation::Halt);
    }
    assert_eq!(
        at(0x1000, &[0xf0, 0xf4]),
        Err(DecodeError::UndefinedEncoding)
    );
    assert_eq!(
        at(0x1000, &[0xf0]),
        Err(DecodeError::Fetch(missing(0x1001)))
    );
    assert_eq!(at(0x1000, &[0x66; 15]), Err(DecodeError::InstructionLength));
}

#[test]
fn valid_forms_outside_the_subset_are_unsupported_without_operand_effects() {
    for bytes in [&[0x82, 0xd0, 1][..], &[0x82][..]] {
        assert_eq!(
            at(0x1000, bytes),
            Err(DecodeError::Unsupported),
            "{bytes:02x?}"
        );
    }
}

#[test]
fn truncation_reports_only_the_first_required_unavailable_byte() {
    for bytes in [
        &[0xb8, 1, 2][..],
        &[0x48][..],
        &[0x48, 0x89][..],
        &[0x81, 0xc0, 1, 2, 3][..],
        &[0x8b, 0x83, 1, 2, 3][..],
        &[0xeb][..],
        &[0x0f][..],
        &[0x0f, 0x85, 1, 2, 3][..],
    ] {
        assert_eq!(
            at(0x1000, bytes),
            Err(DecodeError::Fetch(missing(0x1000 + bytes.len() as u64)))
        );
    }
    let bytes = [0xb8, 0, 0, 0, 0, 0x48, 0xb8];
    assert_eq!(at(0x1000, &bytes).unwrap().next_pc, 0x1005);
}

#[test]
fn required_byte_failure_keeps_the_memory_owners_metadata() {
    let fault = Fault {
        address: 0x8000_0000_0000,
        width: 1,
        access: Access::Execute,
        cause: Cause::Noncanonical,
        failing: 0x8000_0000_0000,
    };
    assert_eq!(
        decode(0x8000_0000_0000, |_| Err(fault)),
        Err(DecodeError::Fetch(fault))
    );
}

#[test]
fn last_instruction_byte_may_be_max_but_required_bytes_may_not_wrap() {
    assert_eq!(at(u64::MAX - 4, &[0xb8, 0, 0, 0, 0]).unwrap().next_pc, 0);
    let pc = u64::MAX;
    let mut reads = Vec::new();
    let decoded = decode(pc, |address| {
        reads.push(address);
        Ok(0xb8)
    });
    assert_eq!(
        decoded,
        Err(DecodeError::Fetch(Fault {
            address: pc,
            width: 2,
            access: Access::Execute,
            cause: Cause::AddressRange,
            failing: 1u128 << 64
        }))
    );
    assert_eq!(reads, vec![u64::MAX]);
}

#[test]
fn overlapping_entries_are_independently_decoded_at_the_requested_pc() {
    let bytes = [0xb8, 0xb9, 3, 0, 0, 0];
    assert_eq!(
        at(0x1000, &bytes).unwrap(),
        Instruction {
            pc: 0x1000,
            next_pc: 0x1005,
            operation: Operation::Mov {
                dst: RegisterView::low(0),
                src: Operand::Immediate(0x3b9),
                width: Width::W32
            }
        }
    );
    assert_eq!(
        at(0x1001, &bytes[1..]).unwrap(),
        Instruction {
            pc: 0x1001,
            next_pc: 0x1006,
            operation: Operation::Mov {
                dst: RegisterView::low(1),
                src: Operand::Immediate(3),
                width: Width::W32
            }
        }
    );
}

#[test]
fn integrated_literal_has_the_declared_instruction_boundaries_and_loop_target() {
    let bytes = [
        0xb8, 0, 0, 0, 0, 0xb9, 3, 0, 0, 0, 0x48, 1, 0xc8, 0x48, 0x83, 0xe9, 1, 0x75, 0xf7, 0x48,
        0x89, 3,
    ];
    let cases = [
        (
            0,
            5,
            Operation::Mov {
                dst: RegisterView::low(0),
                src: Operand::Immediate(0),
                width: Width::W32,
            },
        ),
        (
            5,
            10,
            Operation::Mov {
                dst: RegisterView::low(1),
                src: Operand::Immediate(3),
                width: Width::W32,
            },
        ),
        (
            10,
            13,
            Operation::Alu {
                kind: Alu::Add,
                dst: RegisterView::low(0),
                src: Operand::Register(RegisterView::low(1)),
                width: Width::W64,
            },
        ),
        (
            13,
            17,
            Operation::Alu {
                kind: Alu::Sub,
                dst: RegisterView::low(1),
                src: Operand::Immediate(1),
                width: Width::W64,
            },
        ),
        (
            17,
            19,
            Operation::Jcc {
                condition: Condition::NotEqual,
                target: 0x100a,
            },
        ),
        (
            19,
            22,
            Operation::Store {
                src: Operand::Register(RegisterView::low(0)),
                address: Address {
                    base: AddressBase::Register(3),
                    index: None,
                    displacement: 0,
                    ..Address::default()
                },
                width: Width::W64,
            },
        ),
    ];
    for (offset, end, expected) in cases {
        let pc = 0x1000 + offset as u64;
        assert_eq!(
            at(pc, &bytes[offset..]).unwrap(),
            Instruction {
                pc,
                next_pc: 0x1000 + end,
                operation: expected,
            }
        );
    }
}

#[test]
fn decoding_an_instruction_does_not_probe_its_future_or_branch_target() {
    let mut reads = Vec::new();
    let instruction = decode(0x1000, |address| {
        reads.push(address);
        match address {
            0x1000 => Ok(0xeb),
            0x1001 => Ok(0x7f),
            _ => panic!("unexpected lookahead at {address:x}"),
        }
    })
    .unwrap();
    assert_eq!(instruction.operation, Operation::Jump { target: 0x1081 });
    assert_eq!(reads, [0x1000, 0x1001]);
}
