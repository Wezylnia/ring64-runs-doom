use super::{DecodeError, cursor::Cursor, operand::ModRm, prefix::Prefixes};
use crate::x86::instruction::{Operation, VectorShift, Width};
use crate::x86::mmx::{Kind, Op};

pub(super) fn refine<F>(
    opcode: u8,
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Option<Result<Operation, DecodeError>>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    let mandatory = prefixes.mandatory();
    let kind = match (mandatory, opcode) {
        (None, 0x77) => Kind::Empty,
        (None, 0x6e) => Kind::FromGpr,
        (None, 0x7e) => Kind::ToGpr,
        (None, 0x6f | 0x7f) => Kind::Move,
        (None, 0x70) => Kind::Shuffle,
        (None, 0xc4) => Kind::InsertWord,
        (None, 0xc5) => Kind::ExtractWord,
        (None, 0xd7) => Kind::Mask,
        (Some(0xf2), 0xd6) => Kind::FromXmm,
        (Some(0xf3), 0xd6) => Kind::ToXmm,
        (None, 0x2a) => Kind::PiToPs,
        (Some(0x66), 0x2a) => Kind::PiToPd,
        (None, 0x2c) => Kind::TruncPsToPi,
        (None, 0x2d) => Kind::PsToPi,
        (Some(0x66), 0x2c) => Kind::TruncPdToPi,
        (Some(0x66), 0x2d) => Kind::PdToPi,
        (None, 0x71..=0x73) => Kind::Shift(VectorShift::Left(Width::W16)),
        (None, 0xd1..=0xd3) => Kind::Shift(VectorShift::Right(lane(opcode - 0xd1))),
        (None, 0xe1..=0xe2) => Kind::Shift(VectorShift::ArithmeticRight(lane(opcode - 0xe1))),
        (None, 0xf1..=0xf3) => Kind::Shift(VectorShift::Left(lane(opcode - 0xf1))),
        (None, _) => Kind::Binary(super::packed::binary(opcode)?),
        _ => return None,
    };
    Some(decode(kind, opcode, prefixes, cursor))
}

fn lane(index: u8) -> Width {
    [Width::W16, Width::W32, Width::W64][usize::from(index)]
}

fn decode<F>(
    kind: Kind,
    opcode: u8,
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    let mut op = Op {
        kind,
        dst: 0,
        src: 0,
        memory: false,
        wide: false,
        immediate: 0,
        immediate_count: false,
    };
    if kind == Kind::Empty {
        return Ok(Operation::Mmx { op, address: None });
    }
    let operands = ModRm::read(cursor, prefixes.rex)?;
    if matches!(opcode, 0x71..=0x73) {
        let width = lane(opcode - 0x71);
        op.kind = Kind::Shift(match operands.selector {
            2 => VectorShift::Right(width),
            4 if width != Width::W64 => VectorShift::ArithmeticRight(width),
            6 => VectorShift::Left(width),
            _ => return Err(DecodeError::Unsupported),
        });
        op.immediate_count = true;
        op.dst = operands.rm & 7;
    } else {
        match kind {
            Kind::FromGpr | Kind::InsertWord => {
                op.dst = operands.selector;
                op.src = if operands.mode == 3 { operands.rm } else { 0 };
            }
            Kind::ToGpr => {
                op.dst = if operands.mode == 3 { operands.rm } else { 0 };
                op.src = operands.selector;
                if operands.mode != 3 {
                    op.kind = Kind::Store;
                }
            }
            Kind::Move if opcode == 0x7f => {
                op.dst = operands.rm & 7;
                op.src = operands.selector;
                if operands.mode != 3 {
                    op.kind = Kind::Store;
                    op.dst = 0;
                    op.wide = true;
                }
            }
            Kind::ToXmm | Kind::PiToPs | Kind::PiToPd | Kind::ExtractWord | Kind::Mask => {
                op.dst = operands.register;
                op.src = operands.rm & 7;
            }
            Kind::FromXmm | Kind::PsToPi | Kind::PdToPi | Kind::TruncPsToPi | Kind::TruncPdToPi => {
                op.dst = operands.selector;
                op.src = operands.rm;
            }
            _ => {
                op.dst = operands.selector;
                op.src = operands.rm & 7;
            }
        }
    }
    if matches!(opcode, 0x6e | 0x7e) {
        op.wide = prefixes.long64 && prefixes.rex.width() == Width::W64;
    }
    let register_only = op.immediate_count
        || matches!(
            kind,
            Kind::ExtractWord | Kind::Mask | Kind::FromXmm | Kind::ToXmm
        );
    if register_only && operands.mode != 3 {
        return Err(DecodeError::Invalid);
    }
    op.memory = operands.mode != 3;
    let address = if op.memory {
        op.src = if op.is_store() { op.src } else { 0 };
        Some(operands.address(cursor, prefixes)?)
    } else {
        None
    };
    if op.immediate_count || matches!(kind, Kind::Shuffle | Kind::InsertWord | Kind::ExtractWord) {
        op.immediate = cursor.byte()?;
    }
    Ok(Operation::Mmx { op, address })
}
