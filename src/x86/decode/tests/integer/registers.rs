use super::super::*;

#[test]
fn immediate_moves_preserve_raw_bits_and_use_the_opcode_register_extension() {
    assert_eq!(
        operation(&[0xb8, 0xef, 0xcd, 0xab, 0x89]),
        Operation::Mov {
            dst: RegisterView::low(0),
            src: Operand::Immediate(0x89ab_cdef),
            width: Width::W32,
        }
    );
    assert_eq!(
        operation(&[0x41, 0xbf, 0xff, 0xff, 0xff, 0xff]),
        Operation::Mov {
            dst: RegisterView::low(15),
            src: Operand::Immediate(0xffff_ffff),
            width: Width::W32,
        }
    );
    assert_eq!(
        operation(&[0x4f, 0xb8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]),
        Operation::Mov {
            dst: RegisterView::low(8),
            src: Operand::Immediate(0x1122_3344_5566_7788),
            width: Width::W64,
        }
    );
}

#[test]
fn register_moves_decode_both_directions_and_default_width_with_rex() {
    for bytes in [&[0x89, 0xd0][..], &[0x8b, 0xc2][..]] {
        assert_eq!(
            operation(bytes),
            Operation::Mov {
                dst: RegisterView::low(0),
                src: Operand::Register(RegisterView::low(2)),
                width: Width::W32
            }
        );
    }
    assert_eq!(
        operation(&[0x45, 0x89, 0xc8]),
        Operation::Mov {
            dst: RegisterView::low(8),
            src: Operand::Register(RegisterView::low(9)),
            width: Width::W32
        }
    );
    assert_eq!(
        operation(&[0x4d, 0x8b, 0xc1]),
        Operation::Mov {
            dst: RegisterView::low(8),
            src: Operand::Register(RegisterView::low(9)),
            width: Width::W64
        }
    );
}

#[test]
fn register_arithmetic_decodes_each_operation_at_both_widths() {
    for (bytes, kind, width) in [
        (&[0x01, 0xd0][..], Alu::Add, Width::W32),
        (&[0x48, 0x01, 0xd0][..], Alu::Add, Width::W64),
        (&[0x29, 0xd0][..], Alu::Sub, Width::W32),
        (&[0x48, 0x29, 0xd0][..], Alu::Sub, Width::W64),
        (&[0x39, 0xd0][..], Alu::Cmp, Width::W32),
        (&[0x48, 0x39, 0xd0][..], Alu::Cmp, Width::W64),
    ] {
        assert_eq!(
            operation(bytes),
            Operation::Alu {
                kind,
                dst: RegisterView::low(0),
                src: Operand::Register(RegisterView::low(2)),
                width
            }
        );
    }
    assert_eq!(
        operation(&[0x45, 0x39, 0xc8]),
        Operation::Alu {
            kind: Alu::Cmp,
            dst: RegisterView::low(8),
            src: Operand::Register(RegisterView::low(9)),
            width: Width::W32,
        }
    );
}

#[test]
fn arithmetic_immediates_are_signed_to_the_operand_width() {
    for (bytes, kind, width, value) in [
        (
            &[0x81, 0xc0, 0xff, 0xff, 0xff, 0xff][..],
            Alu::Add,
            Width::W32,
            0xffff_ffff,
        ),
        (
            &[0x48, 0x81, 0xc0, 0xff, 0xff, 0xff, 0xff][..],
            Alu::Add,
            Width::W64,
            u64::MAX,
        ),
        (&[0x83, 0xe8, 0xff][..], Alu::Sub, Width::W32, 0xffff_ffff),
        (
            &[0x48, 0x83, 0xe8, 0xff][..],
            Alu::Sub,
            Width::W64,
            u64::MAX,
        ),
        (
            &[0x81, 0xf8, 0, 0, 0, 0x80][..],
            Alu::Cmp,
            Width::W32,
            0x8000_0000,
        ),
        (
            &[0x48, 0x81, 0xf8, 0, 0, 0, 0x80][..],
            Alu::Cmp,
            Width::W64,
            0xffff_ffff_8000_0000,
        ),
        (&[0x83, 0xc0, 0x80][..], Alu::Add, Width::W32, 0xffff_ff80),
        (&[0x48, 0x83, 0xc0, 0x7f][..], Alu::Add, Width::W64, 0x7f),
        (
            &[0x81, 0xe8, 0xff, 0xff, 0xff, 0x7f][..],
            Alu::Sub,
            Width::W32,
            0x7fff_ffff,
        ),
        (
            &[0x48, 0x81, 0xe8, 0xff, 0xff, 0xff, 0x7f][..],
            Alu::Sub,
            Width::W64,
            0x7fff_ffff,
        ),
        (&[0x83, 0xf8, 0xff][..], Alu::Cmp, Width::W32, 0xffff_ffff),
        (
            &[0x48, 0x83, 0xf8, 0xff][..],
            Alu::Cmp,
            Width::W64,
            u64::MAX,
        ),
    ] {
        assert_eq!(
            operation(bytes),
            Operation::Alu {
                kind,
                dst: RegisterView::low(0),
                src: Operand::Immediate(value),
                width
            }
        );
    }
}

#[test]
fn rex_r_does_not_extend_an_opcode_group_selector() {
    assert_eq!(
        operation(&[0x4c, 0x83, 0xc0, 1]),
        Operation::Alu {
            kind: Alu::Add,
            dst: RegisterView::low(0),
            src: Operand::Immediate(1),
            width: Width::W64,
        }
    );
    assert_eq!(
        operation(&[0x45, 0x81, 0xef, 0xff, 0xff, 0xff, 0xff]),
        Operation::Alu {
            kind: Alu::Sub,
            dst: RegisterView::low(15),
            src: Operand::Immediate(0xffff_ffff),
            width: Width::W32,
        }
    );
}

#[test]
fn memory_operands_keep_the_full_base_identity_and_signed_displacement() {
    assert_eq!(
        operation(&[0x8b, 3]),
        Operation::Load {
            dst: RegisterView::low(0),
            address: Address {
                base: AddressBase::Register(3),
                index: None,
                displacement: 0,
                ..Address::default()
            },
            width: Width::W32
        }
    );
    assert_eq!(
        operation(&[0x48, 0x89, 3]),
        Operation::Store {
            src: Operand::Register(RegisterView::low(0)),
            address: Address {
                base: AddressBase::Register(3),
                index: None,
                displacement: 0,
                ..Address::default()
            },
            width: Width::W64
        }
    );
    assert_eq!(
        operation(&[0x45, 0x8b, 0x45, 0]),
        Operation::Load {
            dst: RegisterView::low(8),
            address: Address {
                base: AddressBase::Register(13),
                index: None,
                displacement: 0,
                ..Address::default()
            },
            width: Width::W32
        }
    );
    assert_eq!(
        operation(&[0x4d, 0x89, 0x45, 0xfc]),
        Operation::Store {
            src: Operand::Register(RegisterView::low(8)),
            address: Address {
                base: AddressBase::Register(13),
                index: None,
                displacement: -4,
                ..Address::default()
            },
            width: Width::W64
        }
    );
    assert_eq!(
        operation(&[0x4a, 0x8b, 0x83, 0, 0, 0, 0x80]),
        Operation::Load {
            dst: RegisterView::low(0),
            address: Address {
                base: AddressBase::Register(3),
                index: None,
                displacement: -2_147_483_648,
                ..Address::default()
            },
            width: Width::W64
        }
    );
    assert_eq!(
        operation(&[0x89, 0xbd, 0xff, 0xff, 0xff, 0x7f]),
        Operation::Store {
            src: Operand::Register(RegisterView::low(7)),
            address: Address {
                base: AddressBase::Register(5),
                index: None,
                displacement: 2_147_483_647,
                segment: crate::x86::context::Segment::Ss,
                ..Address::default()
            },
            width: Width::W32
        }
    );
    assert_eq!(
        operation(&[0x8b, 0x43, 0x80]),
        Operation::Load {
            dst: RegisterView::low(0),
            address: Address {
                base: AddressBase::Register(3),
                index: None,
                displacement: -128,
                ..Address::default()
            },
            width: Width::W32
        }
    );
    assert_eq!(
        operation(&[0x8b, 0x43, 0x7f]),
        Operation::Load {
            dst: RegisterView::low(0),
            address: Address {
                base: AddressBase::Register(3),
                index: None,
                displacement: 127,
                ..Address::default()
            },
            width: Width::W32
        }
    );
}
