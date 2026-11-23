use crate::dbt::wasm::{Emitter, GPR, REASON};
use crate::embedding::layout::RunState;
use crate::x86::processor::Instruction;
use std::mem::offset_of;
use wasm_encoder::{Instruction as Op, MemArg};

impl Emitter {
    pub(in crate::dbt::wasm) fn processor(&mut self, instruction: Instruction, depth: u32) {
        self.op(Op::I32Const(instruction as i32))
            .op(Op::LocalGet(GPR))
            .op(Op::I32WrapI64)
            .op(Op::LocalGet(GPR + 1))
            .op(Op::I32WrapI64);
        match instruction {
            Instruction::Xsetbv => {
                self.op(Op::LocalGet(GPR + 2));
            }
            Instruction::Xgetbv => {
                self.op(Op::I64Const(0));
            }
            _ => self.retired(),
        }
        self.op(Op::Call(self.processor.expect("processor import")))
            .op(Op::LocalTee(REASON))
            .op(Op::BrIf(depth));
        let outputs: &[(u32, usize)] = match instruction {
            Instruction::Cpuid => &[(0, 0), (3, 4), (1, 8), (2, 12)],
            Instruction::Rdtsc | Instruction::Xgetbv => &[(0, 0), (2, 4)],
            Instruction::Xsetbv => &[],
            Instruction::Rdtscp => &[(0, 0), (2, 4), (1, 8)],
        };
        for &(register, offset) in outputs {
            self.op(Op::LocalGet(0))
                .op(Op::I64Load32U(MemArg {
                    offset: (offset_of!(RunState, vector_scratch) + offset) as u64,
                    align: 2,
                    memory_index: 0,
                }))
                .op(Op::LocalSet(GPR + register));
        }
    }
}
