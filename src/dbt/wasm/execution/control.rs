use crate::dbt::wasm::PRIVATE_MEMORY;
use crate::dbt::wasm::{
    ENTRY, Emitter, FLAGS, LHS, REASON, REMAINING, RESULT, RHS, RIP, STORE_STATUS, VALUE,
};
use crate::embedding::layout::{BUDGET, DISPATCH, RunState, SEGMENT, STOP, TARGET};
use crate::memory::canonical48;
use crate::x86::context::Segment;
use crate::x86::instruction::{Address, Condition, CountCondition, RegisterView, Width};
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) enum InstructionRetirement {
    One,
    Deferred,
    DeferredCommit,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    FlatPrefix {
        count: u32,
        terminal: bool,
    },
    AfterPrefix(u32),
}

impl InstructionRetirement {
    pub(in crate::dbt::wasm) fn prechecked_flat_entry(self) -> bool {
        match self {
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            Self::FlatPrefix { .. } => true,
            _ => false,
        }
    }
}

impl Emitter {
    pub(in crate::dbt::wasm) fn target_failure(&mut self, exit_depth: u32) {
        self.record_fault_segment(Segment::Cs);
        // Runtime targets cannot be recovered by decoding the published RIP.
        self.op(Op::LocalGet(0))
            .op(Op::LocalGet(RHS))
            .op(Op::I64Store(MemArg {
                offset: std::mem::offset_of!(RunState, target) as u64,
                align: 3,
                memory_index: PRIVATE_MEMORY,
            }));
        self.exit(TARGET, exit_depth);
    }

    pub(in crate::dbt::wasm) fn check_runtime_target(&mut self, exit_depth: u32) {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.linear_call_spans
            && self.context.code_limit() == u64::from(u32::MAX)
            && matches!(
                self.current_operation,
                crate::x86::instruction::Operation::Call {
                    target: crate::x86::instruction::Operand::Register(_)
                        | crate::x86::instruction::Operand::Memory(_),
                    width: Width::W16 | Width::W32,
                } | crate::x86::instruction::Operation::Return {
                    width: Width::W16 | Width::W32,
                    ..
                } | crate::x86::instruction::Operation::JumpRegister {
                    width: Width::W16 | Width::W32,
                    ..
                } | crate::x86::instruction::Operation::JumpMemory {
                    width: Width::W16 | Width::W32,
                    ..
                }
            )
        {
            // The captured flat CS admits every decoded 16/32-bit near offset.
            return;
        }
        if let Some(check) = self.target_check {
            self.op(Op::LocalGet(RHS))
                .op(Op::Call(check))
                .op(Op::I32Eqz)
                .op(Op::If(BlockType::Empty));
            self.exit(SEGMENT, exit_depth + 1);
            self.op(Op::End);
            return;
        }
        self.op(Op::LocalGet(RHS))
            .op(Op::I64Const(16))
            .op(Op::I64Shl)
            .op(Op::I64Const(16))
            .op(Op::I64ShrS)
            .op(Op::LocalGet(RHS))
            .op(Op::I64Ne)
            .op(Op::If(BlockType::Empty));
        self.target_failure(exit_depth + 1);
        self.op(Op::End);
    }

    pub(in crate::dbt::wasm) fn commit_runtime_target(&mut self, exit_depth: u32) {
        self.op(Op::LocalGet(RHS)).op(Op::LocalSet(RIP));
        let count = match self.instruction_retirement {
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            InstructionRetirement::FlatPrefix {
                count,
                terminal: true,
            } => count + 1,
            _ => 1,
        };
        self.retire_instructions(count);
        if self.finish_instruction(exit_depth) {
            return;
        }
        self.gates(exit_depth);
        self.code_write_boundary(exit_depth, None);
        if self.dense_dispatch {
            self.op(Op::Br(exit_depth - 1));
            return;
        }
        let lookup = self
            .entry_lookup
            .expect("dynamic transfer has an entry lookup");
        self.op(Op::LocalGet(RHS))
            .op(Op::Call(lookup))
            .op(Op::LocalTee(ENTRY))
            .op(Op::I32Const(-1))
            .op(Op::I32Ne)
            .op(Op::If(BlockType::Empty))
            .op(Op::Br(exit_depth))
            .op(Op::End);
        self.exit(DISPATCH, exit_depth);
    }

    pub(in crate::dbt::wasm) fn jump_register(
        &mut self,
        target: u8,
        width: Width,
        exit_depth: u32,
    ) {
        self.register(target, width);
        self.op(Op::LocalSet(RHS));
        self.check_runtime_target(exit_depth);
        self.commit_runtime_target(exit_depth);
    }

    pub(in crate::dbt::wasm) fn jump_memory(
        &mut self,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        self.load_value(address, width, next_pc, false, exit_depth);
        self.op(Op::LocalGet(VALUE)).op(Op::LocalSet(RHS));
        self.check_runtime_target(exit_depth);
        self.commit_runtime_target(exit_depth);
    }

    pub(in crate::dbt::wasm) fn count_branch(
        &mut self,
        condition: CountCondition,
        width: Width,
        target: u64,
        loop_depth: u32,
        exit_depth: u32,
    ) {
        self.register(1, width);
        if condition != CountCondition::Zero {
            self.op(Op::I64Const(1)).op(Op::I64Sub);
            self.narrow(width);
            self.op(Op::LocalTee(RESULT));
        }
        self.op(Op::I64Eqz);
        if condition != CountCondition::Zero {
            self.op(Op::I32Eqz);
        }
        if matches!(condition, CountCondition::Equal | CountCondition::NotEqual) {
            self.condition(if condition == CountCondition::Equal {
                Condition::Equal
            } else {
                Condition::NotEqual
            });
            self.op(Op::I32And);
        }
        self.op(Op::If(BlockType::Empty));
        if self.check_static_target(target, exit_depth + 1) {
            if condition != CountCondition::Zero {
                self.op(Op::LocalGet(RESULT));
                self.write_register(RegisterView::low(1), width);
            }
            self.commit(target);
            self.route(target, loop_depth + 1, exit_depth + 1);
        }
        self.op(Op::End);
        if condition != CountCondition::Zero {
            self.op(Op::LocalGet(RESULT));
            self.write_register(RegisterView::low(1), width);
        }
    }

    pub(in crate::dbt::wasm) fn condition(&mut self, condition: Condition) {
        use Condition::*;
        match condition {
            Overflow | NotOverflow => self.flag_bits_clear(0x800),
            Below | AboveOrEqual => self.flag_bits_clear(1),
            Equal | NotEqual => self.flag_bits_clear(0x40),
            BelowOrEqual | Above => self.flag_bits_clear(0x41),
            Sign | NotSign => self.flag_bits_clear(0x80),
            Parity | NotParity => self.parity_clear(),
            Less | GreaterOrEqual => self.signed_flags_equal(),
            LessOrEqual | Greater => {
                self.flag_bits_clear(0x40);
                self.signed_flags_equal();
                self.op(Op::I32And);
            }
        }
        if matches!(
            condition,
            Overflow | Below | Equal | BelowOrEqual | Sign | Parity | Less | LessOrEqual
        ) {
            self.op(Op::I32Eqz);
        }
    }

    pub(in crate::dbt::wasm) fn cloned_condition(
        &mut self,
        condition: Condition,
        source: Option<super::loops::loop_cfg::ConditionSource>,
    ) {
        use super::loops::loop_cfg::ConditionSource;
        match source {
            None => self.condition(condition),
            Some(
                ConditionSource::Operands(width) | ConditionSource::OperandsWithTakenFlags(width),
            ) => self.compare_condition(condition, width),
            Some(ConditionSource::TestZero { .. }) => {
                self.op(Op::LocalGet(RESULT)).op(Op::I64Eqz);
                if condition == Condition::NotEqual {
                    self.op(Op::I32Eqz);
                }
            }
            Some(ConditionSource::ResultSign(width)) => {
                self.op(Op::LocalGet(RESULT));
                if width == Width::W32 {
                    self.op(Op::I32WrapI64).op(Op::I32Const(0));
                    self.op(if condition == Condition::Sign {
                        Op::I32LtS
                    } else {
                        Op::I32GeS
                    });
                } else {
                    self.op(Op::I64Const(0));
                    self.op(if condition == Condition::Sign {
                        Op::I64LtS
                    } else {
                        Op::I64GeS
                    });
                }
            }
        }
    }

    fn compare_condition(&mut self, condition: Condition, width: Width) {
        use Condition::*;
        let predicate = match (width, condition) {
            (Width::W32, Below) => Op::I32LtU,
            (Width::W32, AboveOrEqual) => Op::I32GeU,
            (Width::W32, Equal) => Op::I32Eq,
            (Width::W32, NotEqual) => Op::I32Ne,
            (Width::W32, BelowOrEqual) => Op::I32LeU,
            (Width::W32, Above) => Op::I32GtU,
            (Width::W64, Below) => Op::I64LtU,
            (Width::W64, AboveOrEqual) => Op::I64GeU,
            (Width::W64, Equal) => Op::I64Eq,
            (Width::W64, NotEqual) => Op::I64Ne,
            (Width::W64, BelowOrEqual) => Op::I64LeU,
            (Width::W64, Above) => Op::I64GtU,
            (Width::W32, Less) => Op::I32LtS,
            (Width::W32, GreaterOrEqual) => Op::I32GeS,
            (Width::W64, Less) => Op::I64LtS,
            (Width::W64, GreaterOrEqual) => Op::I64GeS,
            _ => unreachable!("admitted direct comparison"),
        };
        for operand in [LHS, RHS] {
            self.op(Op::LocalGet(operand));
            if width == Width::W32 {
                self.op(Op::I32WrapI64);
            }
        }
        self.op(predicate);
    }

    fn flag_bits_clear(&mut self, mut mask: i64) {
        let zero = self.packed_result_status && mask & 0x40 != 0;
        if zero {
            self.op(Op::LocalGet(FLAGS))
                .op(Op::I64Const(super::state::PACKED_RESULT_MASK))
                .op(Op::I64And)
                .op(Op::I64Eqz)
                .op(Op::I32Eqz);
            mask &= !0x40;
            if mask == 0 {
                return;
            }
        }
        if self.packed_result_status && mask & 0x80 != 0 {
            mask |= super::state::PACKED_SIGN_INPUTS;
        }
        self.op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(mask))
            .op(Op::I64And)
            .op(Op::I64Eqz);
        if zero {
            self.op(Op::I32And);
        }
    }

    fn signed_flags_equal(&mut self) {
        if self.packed_result_status {
            self.flag_bits_clear(0x80);
            self.flag_bits_clear(0x800);
            self.op(Op::I32Eq);
            return;
        }
        // Align OF with SF before comparing their values.
        self.op(Op::LocalGet(FLAGS))
            .op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(4))
            .op(Op::I64ShrU)
            .op(Op::I64Xor)
            .op(Op::I64Const(0x80))
            .op(Op::I64And)
            .op(Op::I64Eqz);
    }

    pub(in crate::dbt::wasm) fn exit(&mut self, reason: i32, depth: u32) {
        self.op(Op::I32Const(reason))
            .op(Op::LocalSet(REASON))
            .op(Op::Br(depth));
    }

    pub(in crate::dbt::wasm) fn gates(&mut self, exit_depth: u32) {
        self.op(Op::LocalGet(REMAINING))
            .op(Op::I64Eqz)
            .unlikely(Op::If(BlockType::Empty));
        self.exit(BUDGET, exit_depth + 1);
        self.op(Op::End)
            .op(Op::LocalGet(3))
            .op(Op::LocalGet(RIP))
            .op(Op::LocalGet(2))
            .op(Op::I64Eq)
            .op(Op::I32And)
            .unlikely(Op::If(BlockType::Empty));
        self.exit(STOP, exit_depth + 1);
        self.op(Op::End);
    }

    pub(in crate::dbt::wasm) fn commit(&mut self, next: u64) {
        let retired = match self.instruction_retirement {
            InstructionRetirement::One => Some(1),
            InstructionRetirement::Deferred => None,
            InstructionRetirement::DeferredCommit => return,
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            InstructionRetirement::FlatPrefix { count, terminal } => {
                if !terminal {
                    return;
                }
                Some(count + 1)
            }
            InstructionRetirement::AfterPrefix(count) => Some(count + 1),
        };
        if self.loop_gates_prechecked
            && let crate::x86::instruction::Operation::Jcc { target, .. } = self.current_operation
            && next != target
        {
            self.restore_loop_registers();
        }
        self.op(Op::I64Const(next as i64)).op(Op::LocalSet(RIP));
        if let Some(count) = retired {
            self.retire_instructions(count);
        }
    }

    pub(in crate::dbt::wasm) fn retire_before_read_fault(&mut self) {
        match self.instruction_retirement {
            InstructionRetirement::AfterPrefix(count) => self.retire_instructions(count),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            InstructionRetirement::FlatPrefix { count, .. } => {
                self.op(Op::I64Const(self.current_pc as i64))
                    .op(Op::LocalSet(RIP));
                if count != 0 {
                    self.retire_instructions(count);
                }
            }
            _ => {}
        }
    }

    pub(in crate::dbt::wasm) fn retire_instructions(&mut self, count: u32) {
        // Bulk paths cannot report individual PCs. An incomplete capture is discarded.
        if count == 1
            && let Some(retire) = self.flat_retire
        {
            self.op(Op::I64Const(self.current_pc as i64))
                .op(Op::Call(retire));
        }
        self.op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(count)))
            .op(Op::I64Sub)
            .op(Op::LocalSet(REMAINING));
    }

    pub(in crate::dbt::wasm) fn route(&mut self, target: u64, loop_depth: u32, exit_depth: u32) {
        if self.finish_instruction(exit_depth) {
            return;
        }
        self.code_write_boundary(exit_depth, None);
        if let Some(&index) = self.entries.get(&target) {
            let placement = if let Some((body, inner_depth)) = self.structured_loop {
                // Count transient scopes (such as Jcc's If) on top of the
                // cloned forward labels that still enclose the source.
                let extra_depth = loop_depth - inner_depth;
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                let preamble = self
                    .paged_preamble
                    .filter(|(preamble, _, _)| preamble.body.head == body.head);
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if let Some((preamble, handoff, _)) = preamble {
                    if index == preamble.inner_head {
                        self.op(Op::I32Const(1)).op(Op::LocalSet(handoff));
                    } else if index == body.head {
                        self.op(Op::I32Const(2)).op(Op::LocalSet(handoff));
                    }
                } else if self.paged_preamble_latch
                    && self.guarded_paged_lap
                    && let Some((preamble, handoff, inherited)) = self.paged_preamble
                    && body.head == preamble.inner_head
                    && index == preamble.body.head
                {
                    // A direct edge from the guarded inner clone can reuse
                    // the full prefix+inner source proof it inherited. The
                    // ordinary JMP/checked paths never produce this handoff.
                    self.op(Op::LocalGet(inherited))
                        .op(Op::I32Const(
                            super::loops::structured_loop::CONTINUE_PAGED_PREAMBLE,
                        ))
                        .op(Op::I32Mul)
                        .op(Op::LocalSet(handoff));
                }
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                let finite_preamble = preamble.is_some();
                #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
                let finite_preamble = false;
                if !finite_preamble
                    && index == body.head
                    && (body.forward_edges() || self.current_index == body.tail)
                {
                    self.op(Op::Br(body.active_labels(self.current_index) + extra_depth));
                    return;
                }
                let order = body.order();
                let current = order.position(self.current_index).unwrap();
                if let Some(target) = order.position(index)
                    && target > current
                {
                    if target != current + 1 || extra_depth != 0 {
                        self.op(Op::Br(target - current - 1 + extra_depth));
                    }
                    return;
                }
                // The clone lives at its head's original label. Other edges
                // still route against the original entry table.
                body.head
            } else {
                self.current_index
            };
            if index > placement {
                // Later instructions retain enclosing labels. A conditional
                // target adds its active `if` to the ordinary loop depth.
                let extra_depth = loop_depth - (self.entries.len() as u32 - placement);
                self.op(Op::Br(index - placement - 1 + extra_depth));
            } else {
                if !self.dense_dispatch {
                    self.op(Op::I32Const(index as i32)).op(Op::LocalSet(ENTRY));
                }
                self.op(Op::Br(loop_depth));
            }
        } else {
            // Budget/stop still precede fetch even at an external frontier.
            self.gates(exit_depth);
            self.exit(DISPATCH, exit_depth);
        }
    }

    pub(in crate::dbt::wasm) fn code_write_boundary(
        &mut self,
        exit_depth: u32,
        prefix: Option<(u64, u32)>,
    ) {
        if self.stores {
            // Status two is successful storage that invalidated this owner.
            // All instruction effects and its retirement are committed first.
            self.op(Op::LocalGet(STORE_STATUS))
                .op(Op::I32Const(2))
                .op(Op::I32Eq)
                .op(Op::If(BlockType::Empty));
            if let Some((next, count)) = prefix {
                self.op(Op::I64Const(next as i64)).op(Op::LocalSet(RIP));
                self.retire_instructions(count);
            }
            self.gates(exit_depth + 1);
            self.exit(DISPATCH, exit_depth + 1);
            self.op(Op::End);
        }
    }

    pub(in crate::dbt::wasm) fn check_static_target(
        &mut self,
        target: u64,
        exit_depth: u32,
    ) -> bool {
        if self.context.is_long64() {
            if !canonical48(target) {
                self.op(Op::I64Const(target as i64)).op(Op::LocalSet(RHS));
                self.target_failure(exit_depth);
                return false;
            }
        } else if target > self.context.code_limit() {
            self.op(Op::I64Const(target as i64)).op(Op::LocalSet(RHS));
            self.check_runtime_target(exit_depth);
            return false;
        }
        true
    }

    pub(in crate::dbt::wasm) fn target(&mut self, target: u64, loop_depth: u32, exit_depth: u32) {
        if self.check_static_target(target, exit_depth) {
            self.commit(target);
            self.route(target, loop_depth, exit_depth);
        }
    }
}
