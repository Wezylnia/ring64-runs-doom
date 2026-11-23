use crate::memory::Fault;
use crate::x86::context::{ExecutionContext, SegmentFault};

use super::instruction::Instruction;

mod bmi;
mod cursor;
mod numeric;
mod opcode;
mod operand;
mod prefix;
mod system;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Unsupported,
    Undefined,
    UndefinedEncoding,
    /// Recognized illegal form; unknown or unimplemented forms use Unsupported.
    Invalid,
    InstructionLength,
    Fetch(Fault),
    Segment(SegmentFault),
    Paging(crate::x86::paging::walk::Failure),
    PagingRestart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeFault {
    UndefinedOpcode,
    InstructionLength,
}

impl DecodeFault {
    pub fn vector_and_code(self) -> (u8, Option<u16>) {
        match self {
            Self::UndefinedOpcode => (6, None),
            Self::InstructionLength => (13, Some(0)),
        }
    }
}

impl DecodeError {
    pub fn architectural_fault(self) -> Option<DecodeFault> {
        match self {
            Self::Undefined | Self::UndefinedEncoding | Self::Invalid => {
                Some(DecodeFault::UndefinedOpcode)
            }
            Self::InstructionLength => Some(DecodeFault::InstructionLength),
            Self::Unsupported
            | Self::Fetch(_)
            | Self::Segment(_)
            | Self::Paging(_)
            | Self::PagingRestart => None,
        }
    }
}

pub fn decode<F>(pc: u64, mut read: F) -> Result<Instruction, DecodeError>
where
    F: FnMut(u64) -> Result<u8, Fault>,
{
    decode_with_context(pc, &ExecutionContext::default(), |address| {
        read(address).map_err(DecodeError::Fetch)
    })
}

pub fn decode_with_context<F>(
    pc: u64,
    context: &ExecutionContext,
    read: F,
) -> Result<Instruction, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    decode_with_profile(pc, context, false, read)
}

pub fn decode_with_profile<F>(
    pc: u64,
    context: &ExecutionContext,
    paged: bool,
    read: F,
) -> Result<Instruction, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    let mut cursor = cursor::Cursor::new(pc, context.is_long64(), read);
    let (mut prefixes, opcode, multiple_repeats) = prefix::read(&mut cursor, context)?;
    prefixes.paged = paged;
    let operation = opcode::refine(opcode, prefixes, multiple_repeats, &mut cursor)?;
    Ok(Instruction {
        pc,
        next_pc: cursor.next_pc(),
        operation,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        explicit_lock: prefixes.lock,
    })
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline]
pub(crate) fn decode_for_translation<F>(
    pc: u64,
    context: &ExecutionContext,
    paged: bool,
    mut read: F,
) -> Result<Instruction, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    decode_with_shared_reader(pc, context, paged, &mut read)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline(never)]
fn decode_with_shared_reader(
    pc: u64,
    context: &ExecutionContext,
    paged: bool,
    read: &mut dyn FnMut(u64) -> Result<u8, DecodeError>,
) -> Result<Instruction, DecodeError> {
    // Standalone admission and discovery can use the same decoder body.
    // Reached paged decoding keeps its specialized reader.
    decode_with_profile(pc, context, paged, read)
}

#[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
pub(crate) use decode_with_profile as decode_for_translation;

#[cfg(test)]
mod tests;
