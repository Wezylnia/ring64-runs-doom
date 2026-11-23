use super::{Emitter, FLAGS, GPR, REASON, STORE_STATUS};
use crate::embedding::layout::RunState;
use crate::x86::context::Segment;
use crate::x86::instruction::X87Instruction;
use crate::x86::x87::Kind;
use std::mem::offset_of;
use wasm_encoder::{Instruction as Wasm, MemArg};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::{ADDRESS, REMAINING, RIP, SHADOW, VALUE};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::x86::{
    instruction::{Instruction, Operation},
    x87::{
        MEMORY_FIRST_PAIR_SEGMENT, MEMORY_PAIR_SEGMENT, PAGED_LOAD_PAIR_SEGMENT,
        PAGED_STORE_PAIR_SEGMENT, REGISTER_PAIR_SEGMENT, memory_first_pair,
        register_exchange_compare, register_load_arithmetic, register_load_memory_arithmetic,
        register_store_pop,
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
        && if a.address.is_some() {
            memory_first_pair(a.op, b.op) && b.address.is_some() == b.op.is_store()
        } else if b.address.is_some() {
            register_load_memory_arithmetic(a.op, b.op)
        } else {
            register_load_arithmetic(a.op, b.op)
                || register_exchange_compare(a.op, b.op)
                || (register_store_pop(a.op) && register_store_pop(b.op))
        })
    .then_some((a, b))
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn paged_x87_memory_pair(first: Instruction, second: Instruction) -> bool {
    x87_pair(first, second).is_some_and(|(head, tail)| {
        head.address
            .is_some_and(|address| matches!(address.segment, Segment::Ds | Segment::Ss))
            && tail
                .address
                .is_none_or(|address| matches!(address.segment, Segment::Ds | Segment::Ss))
            && second.next_pc > second.pc
            && (second.next_pc - 1) >> 12 == first.pc >> 12
    })
}

impl Emitter {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn can_inline_x87_pop(&self) -> bool {
        use crate::x86::context::Mode;
        self.x87_register_pairs
            && !self.observed
            && !self.paged
            && self.structured_loop.is_none()
            && (self.context.is_long64()
                || self.context.mode() == Mode::Protected && self.context.code32())
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn inline_x87_pop_bank(&mut self, pc: u64, count: i32, destination: u8) {
        use crate::x86::x87::State;
        let x87 = offset_of!(RunState, x87);
        let mem = |offset, align| MemArg {
            offset: offset as u64,
            align,
            memory_index: 0,
        };
        let status = mem(x87 + offset_of!(State, status_word), 1);
        let control = mem(x87 + offset_of!(State, control_word), 1);
        let tags = mem(x87 + offset_of!(State, tag_word), 1);
        // Only the nonempty, no-pending branch may skip numeric admission.
        self.op(Wasm::LocalGet(0))
            .op(Wasm::I32Load16U(status))
            .op(Wasm::LocalTee(REASON))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I32Load16U(control))
            .op(Wasm::I32Const(-1))
            .op(Wasm::I32Xor)
            .op(Wasm::I32And)
            .op(Wasm::I32Const(63))
            .op(Wasm::I32And)
            .op(Wasm::I32Eqz)
            .op(Wasm::LocalGet(0))
            .op(Wasm::I32Load16U(tags))
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::I32Const(11))
            .op(Wasm::I32ShrU)
            .op(Wasm::I32Const(7))
            .op(Wasm::I32And)
            .op(Wasm::LocalTee(STORE_STATUS))
            .op(Wasm::I32Const(1))
            .op(Wasm::I32Shl)
            .op(Wasm::I32ShrU)
            .op(Wasm::I32Const(3))
            .op(Wasm::I32And)
            .op(Wasm::I32Const(3))
            .op(Wasm::I32Ne)
            .op(Wasm::I32And)
            .op(Wasm::If(BlockType::Empty));

        self.op(Wasm::LocalGet(0))
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::I32Const(!(0x8080 | 0x3800 | 0x200)))
            .op(Wasm::I32And)
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::I32Const(count << 11))
            .op(Wasm::I32Add)
            .op(Wasm::I32Const(0x3800))
            .op(Wasm::I32And)
            .op(Wasm::I32Or)
            .op(Wasm::I32Store16(status));

        let reclassify = destination != 0 && count == 1;
        if destination != 0 {
            self.op(Wasm::LocalGet(STORE_STATUS))
                .op(Wasm::I32Const(i32::from(destination)))
                .op(Wasm::I32Add)
                .op(Wasm::I32Const(7))
                .op(Wasm::I32And)
                .op(Wasm::LocalSet(REASON));
            let registers = x87 + offset_of!(State, registers);
            let exponent = mem(registers + 8, 0);
            for (load, store, scratch) in [
                (
                    Wasm::I64Load(mem(registers, 0)),
                    Wasm::I64Store(mem(registers, 0)),
                    reclassify.then_some(VALUE),
                ),
                if reclassify {
                    (
                        Wasm::I64Load16U(exponent),
                        Wasm::I64Store16(exponent),
                        Some(ADDRESS),
                    )
                } else {
                    (Wasm::I32Load16U(exponent), Wasm::I32Store16(exponent), None)
                },
            ] {
                for top in [REASON, STORE_STATUS] {
                    self.op(Wasm::LocalGet(0))
                        .op(Wasm::LocalGet(top))
                        .op(Wasm::I32Const(10))
                        .op(Wasm::I32Mul)
                        .op(Wasm::I32Add);
                }
                self.op(load);
                if let Some(local) = scratch {
                    self.op(Wasm::LocalTee(local));
                }
                self.op(store);
            }
        }

        self.op(Wasm::LocalGet(0))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I32Load16U(tags));
        for top in [STORE_STATUS, REASON].into_iter().take(count as usize) {
            self.op(Wasm::I32Const(3))
                .op(Wasm::LocalGet(top))
                .op(Wasm::I32Const(1))
                .op(Wasm::I32Shl)
                .op(Wasm::I32Shl)
                .op(Wasm::I32Or);
        }
        if reclassify {
            // The raw value owns the new tag, even for an imported bank whose
            // source tag disagrees with its payload. See x87::number::tag.
            self.op(Wasm::I32Const(3))
                .op(Wasm::LocalGet(REASON))
                .op(Wasm::I32Const(1))
                .op(Wasm::I32Shl)
                .op(Wasm::LocalTee(STORE_STATUS))
                .op(Wasm::I32Shl)
                .op(Wasm::I32Const(-1))
                .op(Wasm::I32Xor)
                .op(Wasm::I32And)
                .op(Wasm::LocalGet(ADDRESS))
                .op(Wasm::I32WrapI64)
                .op(Wasm::I32Const(0x7fff))
                .op(Wasm::I32And)
                .op(Wasm::LocalTee(REASON))
                .op(Wasm::I32Const(1))
                .op(Wasm::I32Sub)
                .op(Wasm::I32Const(0x7ffe))
                .op(Wasm::I32GeU)
                .op(Wasm::LocalGet(VALUE))
                .op(Wasm::I64Const(0))
                .op(Wasm::I64GeS)
                .op(Wasm::I32Or)
                .op(Wasm::I32Const(1))
                .op(Wasm::I32Shl)
                .op(Wasm::LocalGet(REASON))
                .op(Wasm::I32Eqz)
                .op(Wasm::LocalGet(VALUE))
                .op(Wasm::I64Eqz)
                .op(Wasm::I32And)
                .op(Wasm::I32Sub)
                .op(Wasm::LocalGet(STORE_STATUS))
                .op(Wasm::I32Shl)
                .op(Wasm::I32Or);
        }
        let saved_pc = if self.context.is_long64() {
            pc
        } else {
            u64::from(pc as u32)
        };
        self.op(Wasm::I32Store16(tags))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I64Const(saved_pc as i64))
            .op(Wasm::I64Store(mem(
                x87 + offset_of!(State, instruction_pointer),
                3,
            )))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I64Const(0))
            .op(Wasm::I64Store(mem(offset_of!(RunState, target), 3)))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I32Const(Segment::Cs as i32))
            .op(Wasm::I32Store(mem(offset_of!(RunState, fault_segment), 2)))
            .op(Wasm::I32Const(0))
            .op(Wasm::LocalSet(REASON));
    }

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
        let paged = self.paged;
        if paged && (self.guarded_paged_lap || !paged_x87_memory_pair(first, second)) {
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
            .op(Wasm::I32And);
        if paged {
            self.op(Wasm::LocalGet(SHADOW))
                .op(Wasm::I32Eqz)
                .op(Wasm::I32And);
        }
        self.op(Wasm::If(BlockType::Empty));
        if register_store_pop(a.op)
            && a.op.index == 1
            && register_store_pop(b.op)
            && b.op.index == 0
            && self.can_inline_x87_pop()
        {
            self.inline_x87_pop_bank(second.pc, 2, 1);
            self.op(Wasm::I64Const(second.next_pc as i64))
                .op(Wasm::LocalSet(RIP));
            self.retire_instructions(2);
            self.route(second.next_pc, loop_depth + 2, exit_depth + 2);
            self.op(Wasm::End);
        }
        // The private discriminator is rejected by older cores. Both
        // operands and PCs fit the existing x87 import without JS mediation.
        let helper = self.x87.expect("x87 import");
        self.op(Wasm::I32Const(a.op.encode() as i32));
        if let Some(address) = a.address {
            self.address(address, first.next_pc);
            let tail_segment = b.address.map_or(Segment::Cs, |tail| tail.segment);
            let pair_kind = match (paged, b.address.is_some()) {
                (true, true) => PAGED_STORE_PAIR_SEGMENT,
                (true, false) => PAGED_LOAD_PAIR_SEGMENT,
                (false, _) => MEMORY_FIRST_PAIR_SEGMENT,
            };
            self.op(Wasm::I32Const(
                (pair_kind | address.segment as u32 | ((tail_segment as u32) << 3)) as i32,
            ))
            .op(Wasm::I64Const(second.pc as i64));
            if let Some(tail) = b.address {
                self.address(tail, second.next_pc);
            } else if paged {
                self.system_bits();
            } else {
                self.op(Wasm::I64Const(0));
            }
            if paged && b.op.is_store() {
                self.system_bits();
                self.op(Wasm::I64Const(32)).op(Wasm::I64Shl);
            }
            self.op(Wasm::I64Const(i64::from(b.op.encode())));
            if paged && b.op.is_store() {
                self.op(Wasm::I64Or);
            }
        } else if let Some(address) = b.address {
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
        if a.address.is_none() {
            self.op(Wasm::I64Const(second.pc as i64));
        }
        self.op(Wasm::Call(helper));
        let stores = b.op.is_store();
        if stores {
            self.op(Wasm::LocalTee(STORE_STATUS))
                .op(Wasm::I32Const(2))
                .op(Wasm::I32LeU);
        } else {
            self.op(Wasm::I32Eqz);
        }
        self.op(Wasm::If(BlockType::Empty));
        if b.op.writes_flags() {
            self.reload_x87_flags();
        }
        self.op(Wasm::I64Const(second.next_pc as i64))
            .op(Wasm::LocalSet(RIP));
        self.retire_instructions(2);
        self.stores = stores;
        let was_guarded = self.guarded_paged_lap;
        if paged {
            // The pair helper checked both starts and excluded observers before committing.
            self.guarded_paged_lap = true;
        }
        self.route(second.next_pc, loop_depth + 2, exit_depth + 2);
        self.guarded_paged_lap = was_guarded;
        self.stores = false;
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let inlined = register_store_pop(operation) && self.can_inline_x87_pop();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if inlined {
            self.inline_x87_pop_bank(pc, 1, operation.index);
            self.op(Wasm::Else);
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let exit_depth = exit_depth + u32::from(inlined);
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if inlined {
            self.op(Wasm::End);
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
