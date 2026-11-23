use crate::x86::context::Segment;
use crate::x86::decode::{DecodeError, cursor::Cursor, operand::ModRm, prefix::Prefixes};
use crate::x86::instruction::{
    Address, AddressBase, DataOperand, Operation, RegisterView, VectorInstruction, VectorOperand,
    Width,
};
use crate::x86::mmx::{Kind, Op};

pub(in crate::x86::decode) fn refine<F>(
    opcode: u8,
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Option<Result<Operation, DecodeError>>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    if !matches!(opcode, 0x18 | 0x2b | 0xae | 0xc3 | 0xe7 | 0xf7) {
        return None;
    }
    let mandatory = prefixes.mandatory();
    if opcode == 0xae && mandatory == Some(0xf3) {
        return Some(segment_base(prefixes, cursor));
    }
    if opcode != 0x18
        && !matches!(
            (opcode, mandatory),
            (0x2b | 0xe7 | 0xf7, None | Some(0x66)) | (0xae | 0xc3, None)
        )
    {
        return None;
    }
    if opcode == 0xae {
        return Some(ae(prefixes, cursor));
    }
    Some((|| {
        let operands = ModRm::read(cursor, prefixes.rex)?;
        if opcode == 0x18 {
            if operands.mode == 3 || operands.selector > 3 {
                return Err(DecodeError::Unsupported);
            }
            operands.address(cursor, prefixes)?;
            return Ok(Operation::Nop);
        }
        if opcode == 0xf7 {
            if operands.mode != 3 {
                return Err(DecodeError::Invalid);
            }
            let mmx = mandatory.is_none();
            return Ok(Operation::MaskedStore {
                data: if mmx {
                    operands.selector
                } else {
                    operands.register
                },
                mask: if mmx { operands.rm & 7 } else { operands.rm },
                mmx,
                address: Address {
                    base: AddressBase::Register(7),
                    index: None,
                    displacement: 0,
                    size: prefixes.address_width(),
                    segment: prefixes.segment_override.unwrap_or(Segment::Ds),
                },
            });
        }
        if operands.mode == 3 {
            return Err(DecodeError::Invalid);
        }
        let address = operands.address(cursor, prefixes)?;
        if opcode == 0xc3 {
            return Ok(Operation::NonTemporalStore {
                src: DataOperand::Register(RegisterView::low(operands.register)),
                address,
                width: if prefixes.long64 && prefixes.rex.width() == Width::W64 {
                    Width::W64
                } else {
                    Width::W32
                },
            });
        }
        if opcode == 0xe7 && mandatory.is_none() {
            return Ok(Operation::Mmx {
                op: Op {
                    kind: Kind::Store,
                    dst: 0,
                    src: operands.selector,
                    memory: true,
                    wide: true,
                    immediate: 0,
                    immediate_count: false,
                },
                address: Some(address),
            });
        }
        Ok(Operation::Vector(VectorInstruction::Move {
            register: operands.register,
            operand: VectorOperand::Memory(address),
            bytes: 16,
            preserve_upper: false,
            aligned: true,
            store: true,
        }))
    })())
}

pub(in crate::x86::decode) fn segment_base<F>(
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    let operands = ModRm::read(cursor, prefixes.rex)?;
    if operands.selector > 3 {
        return Err(DecodeError::Unsupported);
    }
    if operands.mode != 3 {
        operands.address(cursor, prefixes)?;
        return Err(DecodeError::Invalid);
    }
    if prefixes.lock || !prefixes.long64 || !prefixes.paged {
        return Err(DecodeError::UndefinedEncoding);
    }
    Ok(Operation::SegmentBase {
        segment: if operands.selector & 1 == 0 {
            Segment::Fs
        } else {
            Segment::Gs
        },
        register: operands.rm,
        width: prefixes.rex.width(),
        write: operands.selector >= 2,
    })
}

fn ae<F>(prefixes: Prefixes, cursor: &mut Cursor<F>) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    let operands = ModRm::read(cursor, prefixes.rex)?;
    if operands.selector < 2 || operands.mode != 3 && matches!(operands.selector, 4 | 5) {
        return super::numeric_image::extended(&operands, prefixes, cursor);
    }
    if operands.selector == 4 && operands.mode == 3 {
        return Err(DecodeError::Invalid);
    }
    if operands.selector == 6 && operands.mode != 3 {
        // XSAVEOPT is not enumerated by CPUID.0DH.01H.
        return Err(DecodeError::UndefinedEncoding);
    }
    if matches!(operands.selector, 2 | 3) {
        if operands.mode == 3 {
            return Err(DecodeError::Invalid);
        }
        return Ok(Operation::Mxcsr {
            address: operands.address(cursor, prefixes)?,
            store: operands.selector == 3,
        });
    }
    if operands.mode == 3 && operands.selector >= 5 {
        return Ok(Operation::Nop);
    }
    if operands.mode != 3 && operands.selector == 7 {
        return Ok(Operation::Clflush {
            address: operands.address(cursor, prefixes)?,
        });
    }
    Err(DecodeError::Unsupported)
}
