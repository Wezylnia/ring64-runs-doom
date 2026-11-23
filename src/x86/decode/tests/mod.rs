use crate::memory::{Access, Cause, Fault};

use super::{DecodeError, decode};
use crate::x86::context::ExecutionContext;
use crate::x86::instruction::{
    Address, AddressBase, Alu, Condition, DataOperand as Operand, Instruction, Operation,
    RegisterView, Width,
};

mod control;
mod encoding;
mod integer;
mod numeric;
mod system;

fn missing(address: u64) -> Fault {
    Fault {
        address,
        width: 1,
        access: Access::Execute,
        cause: Cause::Unmapped,
        failing: address.into(),
    }
}

fn at(pc: u64, bytes: &[u8]) -> Result<Instruction, DecodeError> {
    decode(pc, |address| {
        address
            .checked_sub(pc)
            .and_then(|offset| usize::try_from(offset).ok())
            .and_then(|offset| bytes.get(offset))
            .copied()
            .ok_or_else(|| missing(address))
    })
}

fn operation(bytes: &[u8]) -> Operation {
    let instruction = at(0x1000, bytes).unwrap();
    assert_eq!(instruction.pc, 0x1000);
    assert_eq!(instruction.next_pc, 0x1000 + bytes.len() as u64);
    instruction.operation
}

fn mode_context(mode: &str, code32: bool) -> ExecutionContext {
    let mut segments = serde_json::Map::new();
    for name in ["cs", "ss", "ds", "es", "fs", "gs"] {
        segments.insert(name.into(), serde_json::json!({
            "base":"0x0", "limit": if mode == "virtual8086" { 0xffffu32 } else { u32::MAX },
            "default32": if mode == "virtual8086" { false } else { code32 }, "usable":true,
            "type": if name == "cs" && mode != "virtual8086" { "readable-code" } else { "read-write" },
        }));
    }
    ExecutionContext::from_json(&serde_json::json!({"mode":mode,"segments":segments})).unwrap()
}
