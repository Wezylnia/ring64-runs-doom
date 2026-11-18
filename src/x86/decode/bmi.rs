use super::{
    DecodeError,
    cursor::Cursor,
    operand::ModRm,
    prefix::{Prefixes, Rex},
};
use crate::x86::{
    context::Mode,
    instruction::{Bmi, Operation, RegisterView},
};

pub(super) fn vex<F>(
    map: u8,
    mut prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    if map & 0x1f != 2 {
        return Err(DecodeError::Unsupported);
    }
    let fields = cursor.byte()?;
    if fields & 3 != 0 {
        return Err(DecodeError::Unsupported);
    }
    let opcode = cursor.byte()?;
    if !matches!(opcode, 0xf2 | 0xf3 | 0xf7) {
        return Err(DecodeError::Unsupported);
    }
    if prefixes.long64 {
        prefixes.rex = Rex::from_vex(map, fields);
    }
    let vvvv = RegisterView::low((!fields >> 3) & if prefixes.long64 { 15 } else { 7 });
    let operands = ModRm::read(cursor, prefixes.rex)?;
    let reg = RegisterView::low(operands.register);
    let (kind, dst) = match opcode {
        0xf2 => (Bmi::AndNot(vvvv), reg),
        0xf7 => (Bmi::Extract(vvvv), reg),
        _ => {
            let kind = match operands.selector {
                1 => Bmi::Reset,
                2 => Bmi::Mask,
                3 => Bmi::Isolate,
                _ => return Err(DecodeError::Unsupported),
            };
            (kind, vvvv)
        }
    };
    // VEX-encoded GPR operands default to dword even in a 16-bit code segment.
    let width = prefixes.rex.width();
    let src = operands.location(cursor, prefixes, width)?;
    if prefixes.lock
        || prefixes.operand_override
        || prefixes.repeat.is_some()
        || prefixes.had_rex
        || matches!(prefixes.mode, Mode::Real | Mode::Virtual8086)
        || fields & 4 != 0
    {
        return Err(DecodeError::UndefinedEncoding);
    }
    Ok(Operation::Bmi {
        kind,
        dst,
        src,
        width,
    })
}
