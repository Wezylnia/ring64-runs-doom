use super::{DIRECTION, Emitter, REASON, REPEAT_REMAINING, RIP, SHADOW, SYSTEM};
use crate::embedding::layout::{DISPATCH, HALTED, RunState};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::x86::instruction::Location;
#[cfg(not(feature = "parallel"))]
use crate::x86::instruction::Width;
use crate::x86::instruction::{Operation, ProtectionKind, Repeat, SystemFlagsKind};
use std::mem::offset_of;
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

pub(super) fn ss_producer(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::PopSs { .. }
            | Operation::Protection(crate::x86::instruction::ProtectionInstruction {
                kind: ProtectionKind::LoadSegment(crate::x86::context::Segment::Ss),
                ..
            })
    )
}

impl Emitter {
    fn can_complete_paged_quietly(&self, operation: Operation) -> bool {
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        {
            let _ = operation;
            false
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        {
            self.paged_quiet_completion
                && (self.sampled_region || !self.context.is_long64())
                && !self.guarded_paged_lap
                && match operation {
                    Operation::Jump { target } | Operation::Jcc { target, .. } => {
                        crate::memory::canonical48(target)
                    }
                    _ => matches!(
                        operation,
                        Operation::Alu { .. }
                            | Operation::Lea { .. }
                            | Operation::Mov { .. }
                            | Operation::Shift {
                                dst: Location::Register(_),
                                ..
                            }
                            | Operation::Unary {
                                dst: Location::Register(_),
                                ..
                            }
                            | Operation::Extend {
                                src: Location::Register(_),
                                ..
                            }
                            | Operation::Cmov {
                                src: Location::Register(_),
                                ..
                            }
                            | Operation::Setcc {
                                dst: Location::Register(_),
                                ..
                            }
                    ),
                }
        }
    }

    pub(super) fn can_complete_paged_write_quietly(&self, operation: Operation) -> bool {
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        {
            let _ = operation;
            false
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        {
            self.paged_quiet_completion
                && self.context.is_long64()
                && !self.sampled_region
                && self.uses_indexed_scalar_write(operation)
                && match operation {
                    Operation::Call { .. } => true,
                    Operation::Store { .. } => {
                        self.proved_stack.is_none()
                            && self.paged_prefix_store.is_none()
                            && self.paged_read_rearm.is_none()
                    }
                    _ => false,
                }
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn require_checked_store_finish(&mut self) {
        if self.can_complete_paged_write_quietly(self.current_operation) {
            // Only a direct write can keep begin's quiet completion proof.
            let result = self.boundary_result.unwrap();
            self.op(Op::I32Const(0)).op(Op::LocalSet(result));
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn paged_return_uses_read_result(&self) -> bool {
        self.paged_quiet_completion
            && !self.guarded_paged_lap
            && matches!(
                self.current_operation,
                Operation::Return {
                    width: Width::W64,
                    ..
                }
            )
    }

    pub(super) fn clear_quiet_fetch(&mut self) {
        if let Some(page) = self.quiet_fetch_page {
            self.op(Op::I64Const(0)).op(Op::LocalSet(page));
        }
    }

    pub(super) fn external_boundary(&mut self, exit_depth: u32) {
        let Some(helper) = self.external_step else {
            return;
        };
        // Idle-only intake changes eligibility only at existing control/shadow boundaries.
        self.op(Op::LocalGet(0))
            .op(Op::I64Load(MemArg {
                offset: offset_of!(RunState, interrupts) as u64,
                align: 2,
                memory_index: 0,
            }))
            .op(Op::I64Eqz)
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.clear_quiet_fetch();
        self.op(Op::LocalGet(RIP));
        self.full_flags();
        self.op(Op::LocalGet(SHADOW))
            .op(Op::Call(helper))
            .op(Op::LocalTee(REASON))
            .op(Op::BrIf(exit_depth + 1))
            .op(Op::End);
    }

    pub(super) fn halt(&mut self, next_pc: u64, exit_depth: u32) {
        self.full_flags();
        self.op(Op::Call(self.halt.expect("HLT import")))
            .op(Op::LocalTee(REASON))
            .op(Op::BrIf(exit_depth));
        self.commit(next_pc);
        self.op(Op::I32Const(0)).op(Op::LocalSet(SHADOW));
        self.trap(true, exit_depth);
        self.external_boundary(exit_depth);
        // Observe the activity writer before the host applies budget/stop gates.
        self.exit(HALTED, exit_depth);
    }

    pub(super) fn full_flags(&mut self) {
        self.status_flags();
        self.op(Op::LocalGet(DIRECTION)).op(Op::I64Or);
        self.system_bits();
        self.op(Op::I64Or);
    }

    pub(super) fn system_bits(&mut self) {
        if self.observed || self.mutable_controls {
            self.op(Op::LocalGet(SYSTEM));
        } else {
            // Ordinary fast units do not change controls; do not keep them live across the loop.
            self.op(Op::LocalGet(0))
                .op(Op::I64Load(MemArg {
                    offset: offset_of!(RunState, flags) as u64,
                    align: 3,
                    memory_index: 0,
                }))
                .op(Op::I64Const(!0xcd7))
                .op(Op::I64And);
        }
    }

    pub(super) fn profile_guard(&mut self, exit_depth: u32) {
        if self.paged {
            return;
        }
        let observed = i32::from(self.observed);
        self.op(Op::LocalGet(SYSTEM))
            .op(Op::I64Const(0x10100))
            .op(Op::I64And)
            .op(Op::I64Eqz)
            .op(Op::I32Eqz)
            .op(Op::LocalGet(SHADOW))
            .op(Op::I32Or)
            .op(Op::I32Eqz)
            .op(Op::I32Eqz);
        if self.observed {
            self.op(Op::LocalGet(0))
                .op(Op::I64Load(MemArg {
                    offset: (offset_of!(RunState, debug)
                        + offset_of!(crate::embedding::layout::debug::DebugState, registers)
                        + offset_of!(crate::x86::debug::State, dr7))
                        as u64,
                    align: 3,
                    memory_index: 0,
                }))
                .op(Op::I64Const(255))
                .op(Op::I64And)
                .op(Op::I64Eqz)
                .op(Op::I32Eqz)
                .op(Op::I32Or);
        }
        self.op(Op::I32Const(observed))
            .op(Op::I32Ne)
            .op(Op::If(BlockType::Empty));
        self.exit(DISPATCH, exit_depth + 1);
        self.op(Op::End);
    }

    fn clear_resume(&mut self) {
        self.op(Op::LocalGet(SYSTEM))
            .op(Op::I64Const(!0x10000))
            .op(Op::I64And)
            .op(Op::LocalSet(SYSTEM));
    }

    pub(super) fn begin(&mut self, operation: Operation, next_pc: u64, exit_depth: u32) {
        if !self.observed || self.guarded_paged_lap {
            return;
        }
        use crate::embedding::layout::debug::CONTINUE_ACTIVE;
        let quiet_registers = self.can_complete_paged_quietly(operation);
        let quiet_completion = quiet_registers || self.can_complete_paged_write_quietly(operation);
        let page = self.current_pc & !4095;
        let retained = self.quiet_fetch_page.filter(|_| {
            quiet_registers && next_pc > self.current_pc && (next_pc - 1) & !4095 == page
        });
        let tag = (page | 1) as i64;
        if let Some(local) = retained {
            self.op(Op::LocalGet(local))
                .op(Op::I64Const(tag))
                .op(Op::I64Eq)
                .op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                .op(Op::I32Const(CONTINUE_ACTIVE))
                .op(Op::Else);
        } else {
            self.clear_quiet_fetch();
        }
        self.op(Op::I64Const(self.current_pc as i64));
        self.full_flags();
        let debug_begin = self.debug_begin.expect("debug begin import");
        self.op(Op::LocalGet(SHADOW));
        #[cfg(not(feature = "parallel"))]
        if quiet_completion {
            #[cfg(target_arch = "wasm32")]
            {
                self.used_paged_quiet_completion = true;
            }
            self.op(Op::I32Const(
                crate::embedding::layout::debug::PAGED_QUIET_COMPLETION as i32,
            ))
            .op(Op::I32Or);
        }
        #[cfg(not(feature = "parallel"))]
        if self.paged
            && self.context.is_long64()
            && matches!(
                operation,
                Operation::Call {
                    width: Width::W64,
                    ..
                } | Operation::Return {
                    width: Width::W64,
                    ..
                }
            )
        {
            self.op(Op::I32Const(
                crate::embedding::layout::debug::PAGED_NEAR_TRANSFER as i32,
            ))
            .op(Op::I32Or);
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.uses_indexed_scalar_read(operation) || self.uses_indexed_scalar_write(operation) {
            self.op(Op::I32Const(
                crate::embedding::layout::debug::PAGED_INDEXED_SCALAR_READ as i32,
            ))
            .op(Op::I32Or);
        }
        self.op(Op::Call(debug_begin));
        if let Some(local) = retained {
            // Only the existing observed begin can grant a quiet source page.
            self.op(Op::LocalTee(REASON))
                .op(Op::I64Const(tag))
                .op(Op::I64Const(0))
                .op(Op::LocalGet(REASON))
                .op(Op::I32Const(CONTINUE_ACTIVE))
                .op(Op::I32Eq)
                .op(Op::Select)
                .op(Op::LocalSet(local))
                .op(Op::End);
        }
        if quiet_completion {
            let result = self.boundary_result.unwrap();
            self.op(Op::LocalTee(REASON))
                .op(Op::LocalTee(result))
                .op(Op::I32Const(0))
                .op(Op::I32GtS);
        } else {
            self.op(Op::LocalTee(REASON));
        }
        self.op(Op::BrIf(exit_depth));
        let sampled_tf = self.sampled_tf.unwrap();
        let incoming_shadow = self.incoming_shadow.unwrap();
        let next_shadow = self.next_shadow.unwrap();
        self.op(Op::LocalGet(SYSTEM))
            .op(Op::I64Const(0x100))
            .op(Op::I64And)
            .op(Op::I64Eqz)
            .op(Op::I32Eqz)
            .op(Op::LocalSet(sampled_tf))
            .op(Op::LocalGet(SHADOW))
            .op(Op::LocalSet(incoming_shadow))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(next_shadow));
        if let Operation::String {
            repeat,
            address_width,
            ..
        } = operation
            && repeat != Repeat::None
        {
            // A private no-work suspension is before the instruction-begin effect.
            self.register(1, address_width);
            self.op(Op::I64Eqz)
                .op(Op::LocalGet(REPEAT_REMAINING))
                .op(Op::I64Eqz)
                .op(Op::I32Eqz)
                .op(Op::I32Or)
                .op(Op::If(BlockType::Empty));
            self.clear_resume();
            self.op(Op::End);
            return;
        }
        let waiting = match operation {
            Operation::X87(instruction) => instruction.op.is_waiting(),
            Operation::Mmx { op, .. } => op.checks_pending(),
            Operation::NumericImage { op, .. } => op.is_waiting(),
            Operation::MaskedStore { mmx, .. } => mmx,
            _ => false,
        };
        if waiting {
            self.mmx_pending(exit_depth);
        }
        self.clear_resume();
    }

    fn ss_producer(&self) -> bool {
        ss_producer(self.current_operation)
    }

    pub(super) fn trap(&mut self, completed: bool, exit_depth: u32) {
        if !self.observed {
            return;
        }
        let incoming = self.incoming_shadow.unwrap();
        let result = self.boundary_result.unwrap();
        let quiet_completion = completed
            && (self.can_complete_paged_quietly(self.current_operation)
                || self.can_complete_paged_write_quietly(self.current_operation));
        if quiet_completion {
            self.op(Op::LocalGet(result))
                .op(Op::I32Const(
                    crate::embedding::layout::debug::CONTINUE_ACTIVE,
                ))
                .op(Op::I32Ne)
                .op(Op::If(BlockType::Empty));
        }
        let ss = if self.ss_producer() { 4 } else { 0 };
        let helper = self.debug_step.expect("debug boundary import");
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let quiet_return = completed && self.paged_return_uses_read_result();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if quiet_return {
            use crate::embedding::layout::{QUIET_RETURN_READ, debug::CONTINUE_ACTIVE};
            let sampled_tf = self.sampled_tf.unwrap();
            self.op(Op::LocalGet(super::STORE_STATUS))
                .op(Op::I32Const(QUIET_RETURN_READ as i32))
                .op(Op::I32Eq)
                .op(Op::LocalGet(sampled_tf))
                .op(Op::I32Eqz)
                .op(Op::I32And)
                .op(Op::LocalGet(incoming))
                .op(Op::I32Const(2))
                .op(Op::I32Ne)
                .op(Op::I32And)
                .op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                .op(Op::I32Const(CONTINUE_ACTIVE))
                .op(Op::Else);
        }
        self.op(Op::I64Const(self.current_pc as i64))
            .op(Op::LocalGet(RIP))
            .op(Op::I32Const(i32::from(completed)));
        self.full_flags();
        self.op(Op::LocalGet(self.sampled_tf.unwrap()))
            .op(Op::LocalGet(incoming))
            .op(Op::I32Const(2))
            .op(Op::I32Eq)
            .op(Op::I32Const(1))
            .op(Op::I32Shl)
            .op(Op::I32Or)
            .op(Op::I32Const(ss))
            .op(Op::I32Or)
            .op(Op::Call(helper));
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if quiet_return {
            self.op(Op::End);
        }
        self.op(Op::LocalTee(REASON))
            .op(Op::LocalTee(result))
            .op(Op::I32Const(0))
            .op(Op::I32GtS)
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(MemArg {
                offset: offset_of!(RunState, delivery_enabled) as u64,
                align: 2,
                memory_index: 0,
            }))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(SHADOW))
            .op(Op::End)
            .op(Op::Br(exit_depth + 1 + u32::from(quiet_completion)))
            .op(Op::End);
        if quiet_completion {
            self.op(Op::End);
        }
        self.op(Op::I32Const(0)).op(Op::LocalSet(REASON));
    }

    pub(super) fn finish_instruction(&mut self, exit_depth: u32) -> bool {
        if self.guarded_paged_lap {
            return false;
        }
        let ss = self.ss_producer();
        if self.observed {
            if ss {
                self.op(Op::LocalGet(self.incoming_shadow.unwrap()))
                    .op(Op::I32Const(2))
                    .op(Op::I32Ne)
                    .op(Op::I32Const(1))
                    .op(Op::I32Shl)
                    .op(Op::LocalSet(SHADOW));
            } else if matches!(self.current_operation, Operation::SystemFlags(d) if d.kind == SystemFlagsKind::Sti)
            {
                self.op(Op::LocalGet(self.next_shadow.unwrap()))
                    .op(Op::LocalSet(SHADOW));
            } else {
                self.op(Op::I32Const(0)).op(Op::LocalSet(SHADOW));
            }
            self.trap(true, exit_depth);
            self.external_boundary(exit_depth);
            if ss
                || matches!(self.current_operation, Operation::SystemFlags(d)
                if matches!(d.kind, SystemFlagsKind::Pop | SystemFlagsKind::Sti))
            {
                self.gates(exit_depth);
                self.exit(DISPATCH, exit_depth);
                return true;
            }
            // Only an exact active comparator keeps this observed owner valid
            // after ordinary RF/SS work. Other profiles retain their redispatch.
            self.op(Op::LocalGet(self.boundary_result.unwrap()))
                .op(Op::I32Const(
                    crate::embedding::layout::debug::CONTINUE_ACTIVE,
                ))
                .op(Op::I32Ne)
                .op(Op::If(BlockType::Empty));
            self.gates(exit_depth + 1);
            self.exit(DISPATCH, exit_depth + 1);
            self.op(Op::End);
            return false;
        }
        if ss {
            self.op(Op::I32Const(2)).op(Op::LocalSet(SHADOW));
        }
        let terminal = ss
            || matches!(self.current_operation, Operation::SystemFlags(d)
                if matches!(d.kind, SystemFlagsKind::Pop | SystemFlagsKind::Sti));
        if terminal {
            self.external_boundary(exit_depth);
            self.gates(exit_depth);
            self.exit(DISPATCH, exit_depth);
        }
        terminal
    }
}
