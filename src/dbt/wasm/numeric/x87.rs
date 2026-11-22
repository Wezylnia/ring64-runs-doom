use super::{Emitter, FLAGS, GPR, REASON, STORE_STATUS};
use crate::embedding::layout::RunState;
use crate::x86::context::Segment;
use crate::x86::instruction::X87Instruction;
use crate::x86::x87::Kind;
use std::mem::offset_of;
use wasm_encoder::{Instruction as Wasm, MemArg};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::{REMAINING, RIP};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::x86::{
    instruction::{Instruction, Operation},
    x87::{
        MEMORY_PAIR_SEGMENT, REGISTER_PAIR_SEGMENT, register_exchange_compare,
        register_load_arithmetic, register_load_memory_arithmetic, register_store_pop,
    },
};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use wasm_encoder::BlockType;

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn x87_pair(
    first: Instruction,
    second: Instruction,
) -> Option<(X87Instruction, X87Instruction)> {
    let (Operation::X87(a), Operation::X87(b)) = (first.operation, second.operation) else {
        return None;
    };
    (first.next_pc == second.pc
        && a.address.is_none()
        && if b.address.is_some() {
            register_load_memory_arithmetic(a.op, b.op)
        } else {
            register_load_arithmetic(a.op, b.op)
                || register_exchange_compare(a.op, b.op)
                || (register_store_pop(a.op) && register_store_pop(b.op))
        })
    .then_some((a, b))
}

impl Emitter {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn emit_x87_pair(
        &mut self,
        first: Instruction,
        second: Instruction,
        loop_depth: u32,
        exit_depth: u32,
    ) {
        if !self.x87_register_pairs || self.structured_loop.is_some() {
            return;
        }
        let Some((a, b)) = x87_pair(first, second) else {
            return;
        };
        // The first entry already passed its own gate. Keep the second entry
        // for short budget, stop, alternate predecessors or a refused pair.
        self.op(Wasm::LocalGet(REMAINING))
            .op(Wasm::I64Const(2))
            .op(Wasm::I64GeU)
            .op(Wasm::LocalGet(3))
            .op(Wasm::LocalGet(2))
            .op(Wasm::I64Const(second.pc as i64))
            .op(Wasm::I64Eq)
            .op(Wasm::I32And)
            .op(Wasm::I32Eqz)
            .op(Wasm::I32And)
            .op(Wasm::If(BlockType::Empty));
        // The private discriminator is rejected by older cores. Both
        // operands and PCs fit the existing x87 import without JS mediation.
        let helper = self.x87.expect("x87 import");
        self.op(Wasm::I32Const(a.op.encode() as i32));
        if let Some(address) = b.address {
            self.address(address, second.next_pc);
            self.op(Wasm::I32Const(
                (MEMORY_PAIR_SEGMENT | address.segment as u32) as i32,
            ))
            .op(Wasm::I64Const(0))
            .op(Wasm::I64Const(i64::from(b.op.encode())));
        } else {
            self.op(Wasm::I64Const(i64::from(b.op.encode())))
                .op(Wasm::I32Const(REGISTER_PAIR_SEGMENT as i32))
                .op(Wasm::I64Const(first.pc as i64));
            if b.op.writes_flags() {
                self.full_flags();
            } else {
                self.op(Wasm::I64Const(0));
            }
        }
        self.op(Wasm::I64Const(second.pc as i64))
            .op(Wasm::Call(helper))
            .op(Wasm::I32Eqz)
            .op(Wasm::If(BlockType::Empty));
        if b.op.writes_flags() {
            self.reload_x87_flags();
        }
        self.op(Wasm::I64Const(second.next_pc as i64))
            .op(Wasm::LocalSet(RIP));
        self.retire_instructions(2);
        self.route(second.next_pc, loop_depth + 2, exit_depth + 2);
        self.op(Wasm::End).op(Wasm::End);
    }

    pub(super) fn x87(
        &mut self,
        instruction: X87Instruction,
        pc: u64,
        next_pc: u64,
        exit_depth: u32,
    ) {
        let operation = instruction.op;
        let writes_ax = operation.writes_ax();
        let helper = self.x87.expect("x87 import");
        self.op(Wasm::I32Const(operation.encode() as i32));
        let segment = if let Some(address) = instruction.address {
            self.address(address, next_pc);
            address.segment
        } else {
            self.op(Wasm::I64Const(0));
            Segment::Ds
        };
        self.op(Wasm::I32Const(segment as i32))
            .op(Wasm::I64Const(pc as i64));
        if operation.writes_flags() {
            self.full_flags();
        } else if operation.kind == Kind::ConditionalMove {
            self.status_flags();
        } else {
            self.op(Wasm::I64Const(0));
        }
        self.op(if writes_ax {
            Wasm::LocalGet(GPR)
        } else {
            Wasm::I64Const(0)
        })
        .op(Wasm::Call(helper))
        .op(Wasm::LocalTee(REASON))
        .op(Wasm::I32Eqz)
        .op(Wasm::LocalGet(REASON))
        .op(Wasm::I32Const(2))
        .op(Wasm::I32Eq)
        .op(Wasm::I32Or)
        .op(Wasm::I32Eqz)
        .op(Wasm::BrIf(exit_depth));
        if operation.writes_flags() {
            self.reload_x87_flags();
        }
        if writes_ax {
            self.op(Wasm::LocalGet(0))
                .op(Wasm::I64Load(MemArg {
                    offset: offset_of!(RunState, registers) as u64,
                    align: 3,
                    memory_index: 0,
                }))
                .op(Wasm::LocalSet(GPR));
        }
        if operation.is_store() {
            self.op(Wasm::LocalGet(REASON))
                .op(Wasm::LocalSet(STORE_STATUS));
            self.stores = true;
        }
    }

    fn reload_x87_flags(&mut self) {
        self.op(Wasm::LocalGet(0))
            .op(Wasm::I64Load(MemArg {
                offset: offset_of!(RunState, flags) as u64,
                align: 3,
                memory_index: 0,
            }))
            .op(Wasm::I64Const(0x8d7))
            .op(Wasm::I64And)
            .op(Wasm::LocalSet(FLAGS));
    }
}
