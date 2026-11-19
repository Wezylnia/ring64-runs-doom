use super::{Emitter, GPR, REASON};
use crate::embedding::layout::RunState;
use crate::x86::instruction::ProtectionKeyInstruction;
use std::mem::offset_of;
use wasm_encoder::{Instruction as Op, MemArg};

impl Emitter {
    pub(super) fn protection_key(&mut self, instruction: ProtectionKeyInstruction, depth: u32) {
        self.op(Op::I32Const(instruction as i32));
        for register in [0, 1, 2] {
            self.op(Op::LocalGet(GPR + register)).op(Op::I32WrapI64);
        }
        self.op(Op::Call(
            self.protection_key.expect("protection key import"),
        ))
        .op(Op::LocalTee(REASON))
        .op(Op::BrIf(depth));
        if instruction == ProtectionKeyInstruction::Read {
            for (register, offset) in [(0, 0), (2, 8)] {
                // Like RDMSR, RDPKRU clears the upper halves in legacy modes too.
                self.op(Op::LocalGet(0))
                    .op(Op::I64Load(MemArg {
                        offset: (offset_of!(RunState, vector_scratch) + offset) as u64,
                        align: 3,
                        memory_index: 0,
                    }))
                    .op(Op::LocalSet(GPR + register));
            }
        }
    }
}
