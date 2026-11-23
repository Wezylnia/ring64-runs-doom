use super::super::{at, missing, operation};
use crate::x86::decode::DecodeError;
use crate::x86::instruction::{
    AddressBase, Operation, Ssse3Op::*, VectorInstruction, VectorOperand,
};
use crate::x86::mmx;

#[test]
fn both_banks_decode_each_opcode_and_consume_only_its_operands() {
    for (map, opcode, expected) in [
        (0x38, 0x00, Pshufb),
        (0x38, 0x01, Phaddw),
        (0x38, 0x02, Phaddd),
        (0x38, 0x03, Phaddsw),
        (0x38, 0x04, Pmaddubsw),
        (0x38, 0x05, Phsubw),
        (0x38, 0x06, Phsubd),
        (0x38, 0x07, Phsubsw),
        (0x38, 0x08, Psignb),
        (0x38, 0x09, Psignw),
        (0x38, 0x0a, Psignd),
        (0x38, 0x0b, Pmulhrsw),
        (0x38, 0x1c, Pabsb),
        (0x38, 0x1d, Pabsw),
        (0x38, 0x1e, Pabsd),
        (0x3a, 0x0f, Palignr),
    ] {
        for xmm in [false, true] {
            let mut bytes = Vec::new();
            if xmm {
                bytes.push(0x66);
            }
            bytes.extend([0x45, 0x0f, map, opcode, 0xd3]);
            let immediate = if expected == Palignr {
                bytes.push(0xfa);
                0xfa
            } else {
                0
            };
            let end = 0x1000 + bytes.len() as u64;
            bytes.push(0x90);
            let ins = at(0x1000, &bytes).unwrap();
            assert_eq!(ins.next_pc, end);
            assert_eq!(
                ins.operation,
                if xmm {
                    Operation::Vector(VectorInstruction::Ssse3 {
                        op: expected,
                        dst: 10,
                        src: VectorOperand::Register(11),
                        immediate,
                    })
                } else {
                    Operation::Mmx {
                        op: mmx::Op {
                            kind: mmx::Kind::Ssse3(expected),
                            dst: 2,
                            src: 3,
                            immediate,
                            ..Default::default()
                        },
                        address: None,
                    }
                }
            );
        }
    }
}

#[test]
fn memory_address_precedes_palignr_immediate_and_retains_rex_address_bits() {
    for prefix in [&[][..], &[0x66][..]] {
        let mut bytes = prefix.to_vec();
        bytes.extend([
            0x47, 0x0f, 0x3a, 0x0f, 0x94, 0xb3, 0xfc, 0xff, 0xff, 0xff, 0xe1,
        ]);
        for n in 1..bytes.len() {
            assert_eq!(
                at(0x1000, &bytes[..n]),
                Err(DecodeError::Fetch(missing(0x1000 + n as u64)))
            );
        }
        let address = match operation(&bytes) {
            Operation::Vector(VectorInstruction::Ssse3 {
                op: Palignr,
                dst: 10,
                src: VectorOperand::Memory(address),
                immediate: 0xe1,
            }) => address,
            Operation::Mmx {
                op:
                    mmx::Op {
                        kind: mmx::Kind::Ssse3(Palignr),
                        dst: 2,
                        src: 0,
                        memory: true,
                        immediate: 0xe1,
                        ..
                    },
                address: Some(address),
            } => address,
            other => panic!("{other:?}"),
        };
        assert_eq!(address.base, AddressBase::Register(11));
        let index = address.index.unwrap();
        assert_eq!(
            (index.register, index.scale, address.displacement),
            (14, 4, -4)
        );
    }
}

#[test]
fn selected_map_frontiers_preserve_unsupported_neighbors_and_the_length_limit() {
    for map in [0x38, 0x3a] {
        for bytes in [vec![0x0f, map], vec![0x66, 0x0f, map]] {
            assert_eq!(
                at(0x1000, &bytes),
                Err(DecodeError::Fetch(missing(0x1000 + bytes.len() as u64)))
            );
        }
    }
    for bytes in [
        &[0xf0, 0x0f, 0x38, 0x00][..],
        &[0xf0, 0x66, 0x0f, 0x3a, 0x0f],
    ] {
        assert_eq!(at(0x1000, bytes), Err(DecodeError::UndefinedEncoding));
    }
    for bytes in [
        &[0x66, 0x0f, 0x38, 0x0c][..],
        &[0xf3, 0x0f, 0x38],
        &[0x66, 0x0f, 0x3a, 0x10],
    ] {
        assert_eq!(at(0x1000, bytes), Err(DecodeError::Unsupported));
    }
    let mut bytes = vec![0x66; 10];
    bytes.extend([0x0f, 0x3a, 0x0f, 0xc0, 0xff]);
    assert_eq!(at(0x1000, &bytes).unwrap().next_pc, 0x100f);
    bytes.insert(0, 0x66);
    assert_eq!(at(0x1000, &bytes), Err(DecodeError::InstructionLength));
}
