use super::{
    DIRECTION, Emitter, FLAGS, GPR, REMAINING, REPEAT_REMAINING, RESULT, RIP, SHADOW, SYSTEM, Unit,
};
use crate::embedding::layout::RunState;
use crate::x86::{
    context::Mode,
    instruction::{Location, Operand, Operation, Width},
    processor::TimeState,
};
use std::mem::{offset_of, size_of};
use wasm_encoder::{Instruction as Op, MemArg};

fn memory(offset: usize) -> MemArg {
    MemArg {
        offset: offset as u64,
        align: 3,
        memory_index: 0,
    }
}

pub(super) fn narrow_gprs(unit: &Unit) -> bool {
    unit.context.mode() == Mode::Protected
        && unit.context.code32()
        && unit.instructions.iter().all(|i| match i.operation {
            Operation::Nop | Operation::Jump { .. } | Operation::Jcc { .. } => true,
            Operation::Mov { width, .. }
            | Operation::Alu { width, .. }
            | Operation::Pop { width, .. }
            | Operation::Leave { width }
            | Operation::Return { width, .. }
            | Operation::JumpRegister { width, .. }
            | Operation::CountBranch {
                count_width: width, ..
            } => width != Width::W64,
            Operation::Load { address, width, .. }
            | Operation::Store { address, width, .. }
            | Operation::NonTemporalStore { address, width, .. }
            | Operation::Lea { address, width, .. }
            | Operation::MemoryAlu { address, width, .. }
            | Operation::PopMemory { address, width }
            | Operation::JumpMemory { address, width } => {
                width != Width::W64 && address.size != Width::W64
            }
            Operation::Unary {
                dst: location,
                width,
                ..
            }
            | Operation::Shift {
                dst: location,
                width,
                ..
            }
            | Operation::DoubleShift {
                dst: location,
                width,
                ..
            }
            | Operation::Cmov {
                src: location,
                width,
                ..
            }
            | Operation::Multiply {
                src: location,
                width,
                ..
            }
            | Operation::Imul {
                src: location,
                width,
                ..
            } => width != Width::W64 && narrow_location(location),
            Operation::Extend {
                src,
                source_width,
                width,
                ..
            } => width != Width::W64 && source_width != Width::W64 && narrow_location(src),
            Operation::Push { src, width } | Operation::Call { target: src, width } => {
                width != Width::W64
                    && !matches!(src, Operand::Memory(address) if address.size == Width::W64)
            }
            _ => false,
        })
}

fn narrow_location(location: Location) -> bool {
    !matches!(location, Location::Memory(address) if address.size == Width::W64)
}

fn gpr_memory(register: usize, narrow: bool) -> MemArg {
    MemArg {
        align: if narrow { 2 } else { 3 },
        ..memory(offset_of!(RunState, registers) + register * size_of::<u64>())
    }
}

const PARITY_SHIFT: i64 = 56;
const PARITY_INPUTS: i64 = ((0xffu64 << PARITY_SHIFT) as i64) | 4;
pub(super) const PACKED_RESULT_MASK: i64 = !0xffff_ffff;
pub(super) const PACKED_SIGN_INPUTS: i64 = i64::MIN | 0x80;
const PACKED_PARITY_INPUTS: i64 = (0xffi64 << 32) | 4;

impl Emitter {
    // XOR bit 2 with the parity of the high byte to recover PF.
    // All other status bits retain their architectural positions.
    pub(super) fn defer_result_parity(&mut self) {
        self.op(Op::LocalGet(RESULT))
            .op(Op::I64Const(PARITY_SHIFT))
            .op(Op::I64Shl)
            .op(Op::I64Const(4))
            .op(Op::I64Or);
    }

    pub(super) fn parity_clear(&mut self) {
        let inputs = if self.packed_result_status {
            PACKED_PARITY_INPUTS
        } else {
            PARITY_INPUTS
        };
        self.op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(inputs))
            .op(Op::I64And)
            .op(Op::I64Popcnt)
            .op(Op::I64Const(1))
            .op(Op::I64And)
            .op(Op::I64Eqz);
    }

    pub(super) fn status_flags(&mut self) {
        let keep = if self.packed_result_status {
            !(PACKED_RESULT_MASK | 0xc4)
        } else {
            !PARITY_INPUTS
        };
        self.op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(keep))
            .op(Op::I64And);
        if self.packed_result_status {
            for (mask, flag) in [(PACKED_RESULT_MASK, 6), (PACKED_SIGN_INPUTS, 7)] {
                self.op(Op::LocalGet(FLAGS))
                    .op(Op::I64Const(mask))
                    .op(Op::I64And)
                    .op(Op::I64Eqz);
                if flag == 7 {
                    self.op(Op::I32Eqz);
                }
                self.op(Op::I64ExtendI32U)
                    .op(Op::I64Const(flag))
                    .op(Op::I64Shl)
                    .op(Op::I64Or);
            }
        }
        self.parity_clear();
        self.op(Op::I32Eqz)
            .op(Op::I64ExtendI32U)
            .op(Op::I64Const(2))
            .op(Op::I64Shl)
            .op(Op::I64Or);
    }

    pub(super) fn retired(&mut self) {
        self.op(Op::LocalGet(1))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Sub);
    }

    pub(super) fn load_state(&mut self, gpr_banks: usize) {
        let narrow = self.narrow_gprs;
        for register in 0..gpr_banks {
            let field = gpr_memory(register, narrow);
            self.op(Op::LocalGet(0))
                .op(if narrow {
                    Op::I64Load32U(field)
                } else {
                    Op::I64Load(field)
                })
                .op(Op::LocalSet(GPR + register as u32));
        }
        for (local, offset) in [
            (FLAGS, offset_of!(RunState, flags)),
            (RIP, offset_of!(RunState, rip)),
        ] {
            self.op(Op::LocalGet(0))
                .op(Op::I64Load(memory(offset)))
                .op(Op::LocalSet(local));
        }
        // Keep direction independent from arithmetic status producers.
        self.op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(!0xcd7))
            .op(Op::I64And)
            .op(Op::LocalSet(SYSTEM))
            .op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(0x400))
            .op(Op::I64And)
            .op(Op::LocalSet(DIRECTION))
            .op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(0x8d7))
            .op(Op::I64And)
            .op(Op::LocalSet(FLAGS));
        if self.packed_result_status {
            // A nonzero witness outside the parity byte preserves imported
            // ZF independently of the original sign bit, even if both are set.
            self.op(Op::LocalGet(FLAGS))
                .op(Op::LocalGet(FLAGS))
                .op(Op::I64Const(0x40))
                .op(Op::I64And)
                .op(Op::I64Eqz)
                .op(Op::I64ExtendI32U)
                .op(Op::I64Const(40))
                .op(Op::I64Shl)
                .op(Op::I64Or)
                .op(Op::LocalSet(FLAGS));
        }
        self.op(Op::LocalGet(0))
            .op(Op::I32Load(MemArg {
                offset: offset_of!(RunState, interruptibility) as u64,
                align: 2,
                memory_index: 0,
            }))
            .op(Op::LocalSet(SHADOW));
        self.op(Op::LocalGet(1)).op(Op::LocalSet(REMAINING));
        if self.has_repeat {
            self.op(Op::LocalGet(0))
                .op(Op::I64Load(memory(offset_of!(RunState, repeat_remaining))))
                .op(Op::LocalSet(REPEAT_REMAINING));
        }
        for bank in 0..16 {
            if let Some(local) = self.vector_registers[bank] {
                self.op(Op::LocalGet(0))
                    .op(Op::V128Load(super::vector::memory(
                        offset_of!(RunState, xmm) + bank * 16,
                    )))
                    .op(Op::LocalSet(local));
            }
        }
    }

    pub(super) fn publish_state(&mut self, gpr_banks: usize) {
        let tsc = offset_of!(RunState, time) + offset_of!(TimeState, tsc);
        // Helpers can rebase this same bank for WRMSR; settle each native entry once.
        self.op(Op::LocalGet(0))
            .op(Op::LocalGet(0))
            .op(Op::I64Load(memory(tsc)));
        self.retired();
        self.op(Op::I64Add).op(Op::I64Store(memory(tsc)));
        let narrow = self.narrow_gprs;
        for register in 0..gpr_banks {
            let field = gpr_memory(register, narrow);
            self.op(Op::LocalGet(0))
                .op(Op::LocalGet(GPR + register as u32))
                .op(if narrow {
                    Op::I64Store32(field)
                } else {
                    Op::I64Store(field)
                });
        }
        for (local, offset) in [
            (FLAGS, offset_of!(RunState, flags)),
            (RIP, offset_of!(RunState, rip)),
            (REMAINING, offset_of!(RunState, remaining)),
        ] {
            self.op(Op::LocalGet(0));
            if local == FLAGS {
                self.full_flags();
            } else {
                self.op(Op::LocalGet(local));
            }
            self.op(Op::I64Store(memory(offset)));
        }
        if self.publish_shadow {
            self.op(Op::LocalGet(0))
                .op(Op::LocalGet(SHADOW))
                .op(Op::I32Store(MemArg {
                    offset: offset_of!(RunState, interruptibility) as u64,
                    align: 2,
                    memory_index: 0,
                }));
        }
        if self.has_repeat {
            self.op(Op::LocalGet(0))
                .op(Op::LocalGet(REPEAT_REMAINING))
                .op(Op::I64Store(memory(offset_of!(RunState, repeat_remaining))));
        }
        for bank in 0..16 {
            if self.vector_written & (1 << bank) != 0 {
                let local = self.vector_registers[bank].expect("published XMM bank");
                self.op(Op::LocalGet(0))
                    .op(Op::LocalGet(local))
                    .op(Op::V128Store(super::vector::memory(
                        offset_of!(RunState, xmm) + bank * 16,
                    )));
            }
        }
    }
}
