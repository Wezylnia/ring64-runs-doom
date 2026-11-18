use super::*;
use crate::x86::{
    decode::decode_with_context,
    instruction::{AddressIndex, Bmi, Location, ProtectionKind},
};

fn contextual(bytes: &[u8], mode: &str, code32: bool) -> Result<Instruction, DecodeError> {
    decode_with_context(0x1000, &modes::context(mode, code32), |address| {
        bytes
            .get((address - 0x1000) as usize)
            .copied()
            .ok_or_else(|| DecodeError::Fetch(missing(address)))
    })
}

fn reg(bank: u8) -> RegisterView {
    RegisterView::low(bank)
}

#[test]
fn vex_gpr_fields_keep_opcode_extensions_separate_from_register_extensions() {
    for (opcode, modrm, kind, dst) in [
        (0xf2, 0xd1, Bmi::AndNot(reg(8)), 10),
        (0xf7, 0xd1, Bmi::Extract(reg(8)), 10),
        (0xf3, 0xc9, Bmi::Reset, 8),
        (0xf3, 0xd1, Bmi::Mask, 8),
        (0xf3, 0xd9, Bmi::Isolate, 8),
    ] {
        for (fields, width) in [(0x38, Width::W32), (0xb8, Width::W64)] {
            assert_eq!(
                operation(&[0xc4, 0x42, fields, opcode, modrm]),
                Operation::Bmi {
                    kind,
                    dst: reg(dst),
                    src: Location::Register(reg(9)),
                    width,
                }
            );
        }
    }
}

#[test]
fn vex_memory_honors_extended_sib_and_reads_the_entire_encoding() {
    let bytes = [
        0x64, 0x67, 0xc4, 0x02, 0x98, 0xf7, 0x84, 0xed, 0xff, 0xff, 0xff, 0xff,
    ];
    assert_eq!(
        operation(&bytes),
        Operation::Bmi {
            kind: Bmi::Extract(reg(12)),
            dst: reg(8),
            width: Width::W64,
            src: Location::Memory(Address {
                base: AddressBase::Register(13),
                index: Some(AddressIndex {
                    register: 13,
                    scale: 8
                }),
                displacement: -1,
                size: Width::W32,
                segment: crate::x86::context::Segment::Fs
            }),
        }
    );
    for len in 0..bytes.len() {
        assert_eq!(
            at(0x1000, &bytes[..len]),
            Err(DecodeError::Fetch(missing(0x1000 + len as u64)))
        );
    }
    let long = [&[0x64; 3][..], &bytes].concat();
    assert!(matches!(operation(&long), Operation::Bmi { .. }));
    assert_eq!(
        at(0x1000, &[&[0x64][..], &long].concat()),
        Err(DecodeError::InstructionLength)
    );
}

#[test]
fn nonlong_vex_uses_eight_dword_registers_and_the_segment_address_size() {
    for mode in ["protected", "compatibility"] {
        for code32 in [false, true] {
            let register = contextual(&[0xc4, 0xc2, 0xb8, 0xf3, 0xc9], mode, code32).unwrap();
            assert_eq!(
                register.operation,
                Operation::Bmi {
                    kind: Bmi::Reset,
                    dst: reg(0),
                    src: Location::Register(reg(1)),
                    width: Width::W32,
                }
            );
            for alternate in [false, true] {
                let mut bytes = Vec::new();
                if alternate {
                    bytes.push(0x67);
                }
                // rm=6 is [disp16] under 16-bit addressing and [ESI] under 32-bit addressing.
                bytes.extend([0xc4, 0xe2, 0xf8, 0xf3, 0x0e]);
                let expected = if code32 == alternate {
                    bytes.extend([0x34, 0x12]);
                    Address {
                        base: AddressBase::None,
                        displacement: 0x1234,
                        size: Width::W16,
                        ..Address::default()
                    }
                } else {
                    Address {
                        base: AddressBase::Register(6),
                        size: Width::W32,
                        ..Address::default()
                    }
                };
                let decoded = contextual(&bytes, mode, code32).unwrap();
                assert_eq!(decoded.next_pc, 0x1000 + bytes.len() as u64);
                assert_eq!(
                    decoded.operation,
                    Operation::Bmi {
                        kind: Bmi::Reset,
                        dst: reg(0),
                        src: Location::Memory(expected),
                        width: Width::W32,
                    }
                );
            }
        }
    }
}

#[test]
fn c4_can_be_les_in_every_legacy_mode_and_only_vex_bmi_is_mode_restricted() {
    for mode in ["protected", "compatibility", "real", "virtual8086"] {
        let bytes = [0x66, 0xc4, 0x62, 0x78, 0xf3, 0xc9];
        let les = contextual(&bytes, mode, false).unwrap();
        assert!(
            matches!(les.operation, Operation::Protection(p) if p.kind == ProtectionKind::LoadFar(crate::x86::context::Segment::Es))
        );
        assert_eq!(les.next_pc, 0x1004);
    }
    for mode in ["real", "virtual8086"] {
        for (opcode, modrm) in [(0xf7, 0xc0), (0xf3, 0xc8)] {
            assert_eq!(
                contextual(&[0xc4, 0xe2, 0x78, opcode, modrm], mode, false),
                Err(DecodeError::UndefinedEncoding)
            );
        }
        assert!(matches!(
            contextual(&[0xf3, 0x0f, 0xbc, 0xc1], mode, false)
                .unwrap()
                .operation,
            Operation::Bmi {
                kind: Bmi::TrailingZeros,
                width: Width::W16,
                ..
            }
        ));
    }
}

#[test]
fn vex_restrictions_apply_only_to_recognized_bmi1_forms() {
    for before in [0x66, 0xf2, 0xf3, 0xf0, 0x40, 0x4f] {
        assert_eq!(
            at(0x1000, &[before, 0xc4, 0xe2, 0x78, 0xf2, 0xc1]),
            Err(DecodeError::UndefinedEncoding)
        );
    }
    for (opcode, selector) in [(0xf2, 0), (0xf7, 0), (0xf3, 1), (0xf3, 2), (0xf3, 3)] {
        assert_eq!(
            at(0x1000, &[0xc4, 0xe2, 0x7c, opcode, selector << 3]),
            Err(DecodeError::UndefinedEncoding)
        );
    }
    for bytes in [
        &[0xc4, 0xe1][..],
        &[0xc4, 0xe2, 0x79, 0xf7],
        &[0xc4, 0xe2, 0x78, 0xf5],
        &[0xc4, 0xe2, 0x78, 0xf3, 0x04],
        &[0xc4, 0xe2, 0x7c, 0xf3, 0x3c],
        &[0xc5, 0xf8],
    ] {
        assert_eq!(at(0x1000, bytes), Err(DecodeError::Unsupported));
    }
}

#[test]
fn invalid_bmi1_fetches_all_required_encoding_bytes_before_undefined_opcode() {
    for bytes in [
        &[0xc4, 0xe2, 0x7c, 0xf2, 0x84, 0x00, 1, 2, 3, 4][..],
        &[0xf0, 0xc4, 0xe2, 0x78, 0xf2, 0x84, 0x00, 1, 2, 3, 4],
        &[0x48, 0x67, 0xc4, 0xe2, 0x78, 0xf3, 0x8c, 0x00, 1, 2, 3, 4],
        &[0x64, 0x48, 0xc4, 0xe2, 0x78, 0xf7, 0x84, 0x00, 1, 2, 3, 4],
        &[0x48, 0x64, 0xc4, 0xe2, 0x78, 0xf7, 0x84, 0x00, 1, 2, 3, 4],
    ] {
        for len in 0..bytes.len() {
            assert_eq!(
                at(0x1000, &bytes[..len]),
                Err(DecodeError::Fetch(missing(0x1000 + len as u64)))
            );
        }
        assert_eq!(at(0x1000, bytes), Err(DecodeError::UndefinedEncoding));
    }
    let real = [0xc4, 0xe2, 0x78, 0xf3, 0x0e, 0x34, 0x12];
    for mode in ["real", "virtual8086"] {
        assert_eq!(
            contextual(&real[..6], mode, false),
            Err(DecodeError::Fetch(missing(0x1006)))
        );
        assert_eq!(
            contextual(&real, mode, false),
            Err(DecodeError::UndefinedEncoding)
        );
    }
}
