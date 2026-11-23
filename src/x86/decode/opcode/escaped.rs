use crate::x86::instruction::{Bit, BitIndex, Count, DoubleShift, Operation, Width};

use super::super::{
    DecodeError, cursor::Cursor, numeric, operand::ModRm, prefix::Prefixes, system,
};
use super::{branch, byte_or_wide, condition};

pub(super) fn decode<F>(
    opcode: u8,
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    if matches!(opcode, 0x31 | 0xa2) {
        return if prefixes.lock {
            Err(DecodeError::UndefinedEncoding)
        } else {
            Ok(Operation::Processor(if opcode == 0xa2 {
                crate::x86::processor::Instruction::Cpuid
            } else {
                crate::x86::processor::Instruction::Rdtsc
            }))
        };
    }
    if matches!(opcode, 0x38 | 0x3a) {
        return super::three_byte::decode(opcode, prefixes, cursor);
    }
    if let Some(result) = system::paging::two_byte(opcode, prefixes, cursor) {
        return result;
    }
    if matches!(opcode, 0x21 | 0x23) {
        if prefixes.repeat.is_some() {
            return Err(DecodeError::Unsupported);
        }
        let byte = cursor.byte()?;
        let register = (byte >> 3) & 7;
        if prefixes.lock
            || prefixes.rex.reg_extension() != 0
            || !prefixes.paged && matches!(register, 4 | 5)
        {
            return Err(DecodeError::Invalid);
        }
        return Ok(Operation::DebugRegister {
            register,
            gpr: (byte & 7) | prefixes.rex.base_extension(),
            write: opcode == 0x23,
            width: if prefixes.long64 {
                Width::W64
            } else {
                Width::W32
            },
        });
    }
    if opcode == 0x0b && prefixes.repeat.is_none() {
        return Err(DecodeError::Undefined);
    }
    if let Some(result) = system::protection::two_byte(opcode, prefixes, cursor) {
        return result;
    }
    if let Some(result) = numeric::streaming::refine(opcode, prefixes, cursor) {
        return result;
    }
    if let Some(result) = simd(opcode, prefixes, cursor) {
        return result;
    }
    if let Some(repeat) = prefixes.repeat
        && (opcode != 0xbc || repeat != 0xf3)
    {
        return Err(DecodeError::Unsupported);
    }
    let rex = prefixes.rex;
    let width = prefixes.width();
    if prefixes.lock
        && !matches!(
            opcode,
            0x1f | 0xa3 | 0xab | 0xb3 | 0xbb | 0xba | 0xb0 | 0xb1 | 0xc0 | 0xc1 | 0xc7
        )
    {
        return Err(DecodeError::Unsupported);
    }
    match opcode {
        0x1f => {
            let operands = ModRm::read(cursor, rex)?;
            if operands.selector != 0 {
                return Err(DecodeError::Unsupported);
            }
            operands.location(cursor, prefixes, width)?;
            Ok(Operation::Nop)
        }
        0xb0 | 0xb1 | 0xc0 | 0xc1 => {
            let width = byte_or_wide(opcode, width);
            let operands = ModRm::read(cursor, rex)?;
            let dst = operands.location(cursor, prefixes, width)?;
            let src = rex.view(operands.register, width);
            Ok(if opcode & 0xf0 == 0xb0 {
                Operation::CompareExchange { dst, src, width }
            } else {
                Operation::Xadd { dst, src, width }
            })
        }
        0xc7 => {
            let operands = ModRm::read(cursor, rex)?;
            if operands.mode != 3
                && matches!(operands.selector, 3..=5)
                && !prefixes.operand_override
            {
                // XRSTORS, XSAVEC and XSAVES require features absent from CPUID.0DH.01H.
                return Err(DecodeError::UndefinedEncoding);
            }
            if operands.selector != 1 {
                return Err(DecodeError::Unsupported);
            }
            if operands.mode == 3 {
                return Err(DecodeError::Invalid);
            }
            let address = operands.address(cursor, prefixes)?;
            Ok(if width == Width::W64 {
                Operation::CompareExchange16b { address }
            } else {
                Operation::CompareExchange8b { address }
            })
        }
        0xaf => {
            let operands = ModRm::read(cursor, rex)?;
            Ok(Operation::Imul {
                dst: rex.view(operands.register, width),
                src: operands.location(cursor, prefixes, width)?,
                immediate: None,
                width,
            })
        }
        0xa3 | 0xab | 0xb3 | 0xbb => {
            let operands = ModRm::read(cursor, rex)?;
            Ok(Operation::Bit {
                kind: match opcode {
                    0xa3 => Bit::Test,
                    0xab => Bit::Set,
                    0xb3 => Bit::Reset,
                    0xbb => Bit::Complement,
                    _ => unreachable!(),
                },
                dst: operands.location(cursor, prefixes, width)?,
                index: BitIndex::Register(rex.view(operands.register, width)),
                width,
            })
        }
        0xba => {
            let operands = ModRm::read(cursor, rex)?;
            let kind = match operands.selector {
                4 => Bit::Test,
                5 => Bit::Set,
                6 => Bit::Reset,
                7 => Bit::Complement,
                _ => return Err(DecodeError::Unsupported),
            };
            let dst = operands.location(cursor, prefixes, width)?;
            Ok(Operation::Bit {
                kind,
                dst,
                index: BitIndex::Immediate(cursor.byte()?),
                width,
            })
        }
        0xbc | 0xbd => scan(opcode, prefixes, cursor),
        0xc8..=0xcf => {
            if width == Width::W16 {
                return Err(DecodeError::Unsupported);
            }
            Ok(Operation::Bswap {
                dst: rex.view((opcode - 0xc8) | rex.base_extension(), width),
                width,
            })
        }
        0xa4 | 0xa5 | 0xac | 0xad => {
            let operands = ModRm::read(cursor, rex)?;
            let dst = operands.location(cursor, prefixes, width)?;
            let count = if opcode & 1 == 0 {
                Count::Immediate(cursor.byte()?)
            } else {
                Count::Cl
            };
            Ok(Operation::DoubleShift {
                kind: if opcode & 8 == 0 {
                    DoubleShift::Left
                } else {
                    DoubleShift::Right
                },
                dst,
                src: rex.view(operands.register, width),
                count,
                width,
            })
        }
        0xb6 | 0xb7 | 0xbe | 0xbf => {
            let source_width = byte_or_wide(opcode, Width::W16);
            if source_width == Width::W16 && width == Width::W16 {
                return Err(DecodeError::Unsupported);
            }
            let operands = ModRm::read(cursor, rex)?;
            Ok(Operation::Extend {
                dst: rex.view(operands.register, width),
                src: operands.location(cursor, prefixes, source_width)?,
                source_width,
                width,
                signed: opcode & 8 != 0,
            })
        }
        0x40..=0x4f => {
            let operands = ModRm::read(cursor, rex)?;
            Ok(Operation::Cmov {
                dst: rex.view(operands.register, width),
                src: operands.location(cursor, prefixes, width)?,
                width,
                condition: condition(opcode),
            })
        }
        0x90..=0x9f => {
            let operands = ModRm::read(cursor, rex)?;
            Ok(Operation::Setcc {
                dst: operands.location(cursor, prefixes, Width::W8)?,
                condition: condition(opcode),
            })
        }
        0x80..=0x8f => branch(opcode, prefixes, cursor),
        _ => Err(DecodeError::Unsupported),
    }
}

pub(super) fn simd<F>(
    opcode: u8,
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Option<Result<Operation, DecodeError>>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    if let Some(result) = numeric::mmx::refine(opcode, prefixes, cursor) {
        return Some(result);
    }
    if let Some(result) = numeric::float::refine(opcode, prefixes, cursor) {
        return Some(result);
    }
    numeric::vector::refine(opcode, prefixes, cursor).map(|result| result.map(Operation::Vector))
}

pub(super) fn scan<F>(
    opcode: u8,
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    let width = prefixes.width();
    let operands = ModRm::read(cursor, prefixes.rex)?;
    let dst = prefixes.rex.view(operands.register, width);
    let src = operands.location(cursor, prefixes, width)?;
    if prefixes.repeat == Some(0xf3) {
        return Ok(Operation::Bmi {
            kind: crate::x86::instruction::Bmi::TrailingZeros,
            dst,
            src,
            width,
        });
    }
    Ok(Operation::Scan {
        reverse: opcode == 0xbd,
        dst,
        src,
        width,
    })
}
