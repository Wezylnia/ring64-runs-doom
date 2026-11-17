use super::{DecodeError, cursor::Cursor, operand::ModRm, paging, prefix::Prefixes};
use crate::x86::instruction::{Operation, Ssse3Op, VectorInstruction, VectorOperand};
use crate::x86::mmx;

pub(super) fn decode<F>(
    map: u8,
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    if prefixes.repeat.is_some() {
        return Err(DecodeError::Unsupported);
    }
    let opcode = cursor.byte()?;
    let op = match (map, opcode) {
        (0x38, 0x82) => return paging::invpcid(prefixes, cursor),
        (0x38, 0x00..=0x0b) => Ssse3Op::ALL[usize::from(opcode)],
        (0x38, 0x1c) => Ssse3Op::Pabsb,
        (0x38, 0x1d) => Ssse3Op::Pabsw,
        (0x38, 0x1e) => Ssse3Op::Pabsd,
        (0x3a, 0x0f) => Ssse3Op::Palignr,
        _ => return Err(DecodeError::Unsupported),
    };
    if prefixes.lock {
        return Err(DecodeError::UndefinedEncoding);
    }
    let operands = ModRm::read(cursor, prefixes.rex)?;
    let address = if operands.mode == 3 {
        None
    } else {
        Some(operands.address(cursor, prefixes)?)
    };
    let immediate = if op == Ssse3Op::Palignr {
        cursor.byte()?
    } else {
        0
    };
    if prefixes.operand_override {
        let src = address.map_or(VectorOperand::Register(operands.rm), VectorOperand::Memory);
        Ok(Operation::Vector(VectorInstruction::Ssse3 {
            op,
            dst: operands.register,
            src,
            immediate,
        }))
    } else {
        Ok(Operation::Mmx {
            op: mmx::Op {
                kind: mmx::Kind::Ssse3(op),
                dst: operands.selector,
                src: if address.is_some() {
                    0
                } else {
                    operands.rm & 7
                },
                memory: address.is_some(),
                immediate,
                ..Default::default()
            },
            address,
        })
    }
}
