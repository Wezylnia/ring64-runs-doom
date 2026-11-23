use crate::x86::instruction::{Condition, Location, MemoryAluOperands, Operation, Width};

use super::{DecodeError, cursor::Cursor, prefix::Prefixes};

mod escaped;
mod primary;

pub(super) fn refine<F>(
    opcode: u8,
    prefixes: Prefixes,
    multiple_repeats: bool,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    if prefixes.lock && prefixes.repeat.is_some() {
        return mixed_lock(opcode, prefixes, multiple_repeats, cursor);
    }
    if opcode == 0xf4 {
        return if prefixes.lock {
            Err(DecodeError::UndefinedEncoding)
        } else {
            Ok(Operation::Halt)
        };
    }
    let operation = primary::decode(opcode, prefixes, cursor)?;
    if prefixes.lock && !lockable(operation) {
        Err(DecodeError::Invalid)
    } else {
        Ok(operation)
    }
}

fn mixed_lock<F>(
    opcode: u8,
    prefixes: Prefixes,
    multiple_repeats: bool,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    if opcode == 0x90 && prefixes.repeat == Some(0xf3) && !multiple_repeats {
        return Err(DecodeError::Invalid);
    }
    if opcode != 0x0f {
        return Err(DecodeError::Unsupported);
    }
    let opcode = cursor.byte()?;
    if opcode == 0xae && prefixes.repeat == Some(0xf3) {
        return super::streaming::segment_base(prefixes, cursor);
    }
    // preserve multi-selector frontiers outside the existing fsgsbase path.
    if multiple_repeats {
        return Err(DecodeError::Unsupported);
    }
    if opcode == 0xbc && prefixes.repeat == Some(0xf3) {
        escaped::scan(opcode, prefixes, cursor)?;
    } else {
        escaped::simd(opcode, prefixes, cursor).ok_or(DecodeError::Unsupported)??;
    }
    Err(DecodeError::Invalid)
}

fn lockable(operation: Operation) -> bool {
    match operation {
        Operation::MemoryAlu {
            kind,
            operands: MemoryAluOperands::MemoryFirst(_),
            ..
        } => kind.writes_result(),
        Operation::Unary {
            dst: Location::Memory(_),
            ..
        }
        | Operation::Exchange {
            dst: Location::Memory(_),
            ..
        }
        | Operation::Xadd {
            dst: Location::Memory(_),
            ..
        }
        | Operation::CompareExchange {
            dst: Location::Memory(_),
            ..
        }
        | Operation::CompareExchange8b { .. }
        | Operation::CompareExchange16b { .. } => true,
        Operation::Bit {
            kind,
            dst: Location::Memory(_),
            ..
        } => kind.writes_result(),
        _ => false,
    }
}

fn byte_or_wide(opcode: u8, wide: Width) -> Width {
    if opcode & 1 == 0 { Width::W8 } else { wide }
}

fn relative_displacement<F>(cursor: &mut Cursor<F>, width: Width) -> Result<i64, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    Ok(if width == Width::W16 {
        i64::from(cursor.unsigned(2)? as u16 as i16)
    } else {
        i64::from(cursor.unsigned(4)? as u32 as i32)
    })
}

fn branch<F>(
    opcode: u8,
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    let width = prefixes.near_width();
    let displacement = if matches!(opcode, 0xeb | 0x70..=0x7f) {
        i64::from(cursor.byte()? as i8)
    } else {
        relative_displacement(cursor, width)?
    };
    let target = cursor.next_pc().wrapping_add(displacement as u64) & width.mask();
    if matches!(opcode, 0xe9 | 0xeb) {
        Ok(Operation::Jump { target })
    } else {
        Ok(Operation::Jcc {
            condition: condition(opcode),
            target,
        })
    }
}

fn condition(opcode: u8) -> Condition {
    match opcode & 15 {
        0x0 => Condition::Overflow,
        0x1 => Condition::NotOverflow,
        0x2 => Condition::Below,
        0x3 => Condition::AboveOrEqual,
        0x4 => Condition::Equal,
        0x5 => Condition::NotEqual,
        0x6 => Condition::BelowOrEqual,
        0x7 => Condition::Above,
        0x8 => Condition::Sign,
        0x9 => Condition::NotSign,
        0xa => Condition::Parity,
        0xb => Condition::NotParity,
        0xc => Condition::Less,
        0xd => Condition::GreaterOrEqual,
        0xe => Condition::LessOrEqual,
        0xf => Condition::Greater,
        _ => unreachable!(),
    }
}
