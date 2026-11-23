use crate::dbt::wasm::{ADDRESS, Emitter, GPR, LHS, RESULT, RHS, VALUE};
use crate::x86::context::Segment;
use crate::x86::instruction::{Address, AddressBase, Operand, RegisterView, Width};
use wasm_encoder::Instruction as Op;

const RSP: u32 = GPR + 4;

impl Emitter {
    pub(in crate::dbt::wasm) fn stack_word(&mut self, width: Width, exit_depth: u32) {
        self.load_value(
            Address {
                base: AddressBase::Register(4),
                index: None,
                displacement: 0,
                size: self.context.stack_width(),
                segment: Segment::Ss,
            },
            width,
            0,
            false,
            exit_depth,
        );
    }

    pub(in crate::dbt::wasm) fn update_stack_pointer(&mut self, count: u64) {
        self.op(Op::LocalGet(ADDRESS))
            .op(Op::I64Const(count as i64))
            .op(Op::I64Add);
        let stack_width = self.context.stack_width();
        self.narrow(stack_width);
        self.write_register(RegisterView::low(4), stack_width);
    }

    pub(in crate::dbt::wasm) fn push_result(&mut self, width: Width, exit_depth: u32) {
        let stack_width = self.context.stack_width();
        self.op(Op::LocalGet(RSP))
            .op(Op::I64Const(i64::from(width.bytes())))
            .op(Op::I64Sub);
        self.narrow(stack_width);
        self.op(Op::LocalSet(ADDRESS));
        self.segment = Segment::Ss;
        self.store_result(width, exit_depth);
        self.op(Op::LocalGet(ADDRESS));
        self.write_register(RegisterView::low(4), stack_width);
    }

    pub(in crate::dbt::wasm) fn push(
        &mut self,
        src: Operand,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        self.operand(src, width, next_pc, exit_depth);
        self.op(Op::LocalSet(RESULT));
        self.push_result(width, exit_depth);
    }

    pub(in crate::dbt::wasm) fn pop(&mut self, dst: u8, width: Width, exit_depth: u32) {
        self.stack_word(width, exit_depth);
        self.update_stack_pointer(u64::from(width.bytes()));
        self.op(Op::LocalGet(VALUE));
        self.write_register(RegisterView::low(dst), width);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(in crate::dbt::wasm) fn emit_paged_immediate_stack_pair(
        &mut self,
        first: crate::x86::instruction::Instruction,
        second: crate::x86::instruction::Instruction,
        loop_depth: u32,
    ) {
        use crate::dbt::wasm::{RIP, STORE_STATUS, memory::indexed::IndexedOperand};
        use crate::x86::instruction::Operation;
        use wasm_encoder::{BlockType, MemArg};

        let Operation::Push {
            src: Operand::Immediate(value),
            width: Width::W64,
        } = first.operation
        else {
            return;
        };
        let Operation::Pop {
            dst,
            width: Width::W64,
        } = second.operation
        else {
            return;
        };
        if !self.sampled_region
            || !self.paged_quiet_completion
            || !self.uses_indexed_scalar_write(first.operation)
            || !self.uses_indexed_scalar_read(second.operation)
            || self.context.stack_width() != Width::W64
            || dst == 4
            || first.next_pc != second.pc
            || second.next_pc <= second.pc
            || (second.next_pc - 1) >> 12 != first.pc >> 12
        {
            return;
        }

        self.guard_straight_prefix(&first, &second, 2);
        self.newest_prefix_fetch(first.pc, true, false);
        self.op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(RSP))
            .op(Op::I64Const(8))
            .op(Op::I64Sub)
            .op(Op::LocalSet(ADDRESS));
        self.indexed_read_host(
            self.indexed_paged_read.unwrap(),
            8,
            true,
            IndexedOperand::ReadModifyWrite,
        );
        self.op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Empty));
        // The POP reads the exact RAM bytes written by the PUSH. Both
        // instructions leave the same stack and fetch leaves newest.
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Const(value as i64));
        self.direct_scalar_store(
            Width::W64,
            MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            },
        );
        self.op(Op::I64Const(value as i64));
        self.write_register(RegisterView::low(dst), Width::W64);
        self.remember_quiet_fetch(first.pc);
        self.op(Op::I64Const(second.next_pc as i64))
            .op(Op::LocalSet(RIP));
        self.retire_instructions(2);
        self.guarded_paged_lap = true;
        self.route(second.next_pc, loop_depth + 3, loop_depth + 4);
        self.guarded_paged_lap = false;
        self.op(Op::End).op(Op::End).op(Op::End);
    }

    pub(in crate::dbt::wasm) fn pop_memory(
        &mut self,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        self.stack_word(width, exit_depth);
        self.op(Op::LocalGet(VALUE)).op(Op::LocalSet(RESULT));
        self.op(Op::LocalGet(ADDRESS))
            .op(Op::I64Const(i64::from(width.bytes())))
            .op(Op::I64Add);
        let stack_width = self.context.stack_width();
        self.narrow(stack_width);
        self.merge_register(RegisterView::low(4), stack_width);
        self.op(Op::LocalSet(LHS));
        self.address_with_stack(address, next_pc, Some(LHS));
        self.op(Op::LocalSet(ADDRESS));
        self.store_result(width, exit_depth);
        self.op(Op::LocalGet(LHS)).op(Op::LocalSet(RSP));
    }

    pub(in crate::dbt::wasm) fn leave(&mut self, width: Width, exit_depth: u32) {
        self.register(5, self.context.stack_width());
        self.op(Op::LocalSet(ADDRESS));
        self.segment = Segment::Ss;
        self.load_address_value(width, false, exit_depth);
        self.update_stack_pointer(u64::from(width.bytes()));
        self.op(Op::LocalGet(VALUE));
        self.write_register(RegisterView::low(5), width);
    }

    pub(in crate::dbt::wasm) fn call(
        &mut self,
        target: Operand,
        width: Width,
        next_pc: u64,
        loop_depth: u32,
        exit_depth: u32,
    ) {
        match target {
            Operand::Immediate(target) => {
                if !self.check_static_target(target, exit_depth) {
                    return;
                }
                self.op(Op::I64Const(next_pc as i64))
                    .op(Op::LocalSet(RESULT));
                self.push_result(width, exit_depth);
                self.commit(target);
                self.route(target, loop_depth, exit_depth);
            }
            Operand::Register(_) | Operand::Memory(_) => {
                self.operand(target, width, next_pc, exit_depth);
                self.op(Op::LocalSet(RHS));
                self.push_runtime_call(width, next_pc, exit_depth);
                self.commit_runtime_target(exit_depth);
            }
        }
    }

    fn push_runtime_call(&mut self, width: Width, next_pc: u64, exit_depth: u32) {
        self.check_runtime_target(exit_depth);
        self.op(Op::I64Const(next_pc as i64))
            .op(Op::LocalSet(RESULT));
        self.push_result(width, exit_depth);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(in crate::dbt::wasm) fn guarded_paged_register_call(
        &mut self,
        instruction: crate::x86::instruction::Instruction,
        exit_depth: u32,
    ) -> bool {
        use crate::dbt::wasm::{STORE_STATUS, memory::indexed::IndexedOperand};
        use crate::embedding::layout::debug::CONTINUE_ACTIVE;
        use crate::x86::instruction::Operation;
        use wasm_encoder::{BlockType, MemArg, ValType};

        let Operation::Call {
            target: Operand::Register(register),
            width: Width::W64,
        } = instruction.operation
        else {
            return false;
        };
        let page = instruction.pc & !4095;
        if !self.retouch_prefix_fetch
            || !self.paged_quiet_completion
            || !self.context.is_long64()
            || self.context.cpl() != 0
            || self.context.stack_width() != Width::W64
            || !self.uses_indexed_scalar_write(instruction.operation)
            || instruction.next_pc <= instruction.pc
            || (instruction.next_pc - 1) & !4095 != page
        {
            return false;
        }

        self.operand(
            Operand::Register(register),
            Width::W64,
            instruction.next_pc,
            exit_depth,
        );
        self.op(Op::LocalSet(RHS))
            .op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0))
            .op(Op::Block(BlockType::Empty))
            // An in-page target of this admitted long-mode instruction is canonical.
            .op(Op::LocalGet(RHS))
            .op(Op::I64Const(page as i64))
            .op(Op::I64Xor)
            .op(Op::I64Const(4095))
            .op(Op::I64GtU)
            .op(Op::BrIf(0));
        self.newest_prefix_fetch(instruction.pc, true, false);
        self.op(Op::I32Eqz)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(RSP))
            .op(Op::I64Const(8))
            .op(Op::I64Sub)
            .op(Op::LocalSet(ADDRESS));
        self.indexed_read_host(
            self.indexed_paged_read.unwrap(),
            8,
            false,
            IndexedOperand::Write,
        );
        let boundary_result = self.boundary_result.unwrap();
        self.op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Const(instruction.next_pc as i64))
            .op(Op::I64Store(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::LocalSet(RSP))
            .op(Op::I32Const(CONTINUE_ACTIVE))
            .op(Op::LocalSet(boundary_result))
            .op(Op::I32Const(1))
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::I32Const(1))
            .op(Op::Br(1))
            .op(Op::End)
            .op(Op::End)
            .op(Op::I32Eqz)
            .unlikely(Op::If(BlockType::Empty));
        // Any refusal precedes the stack store and uses the ordinary begin/fault order.
        self.begin(instruction.operation, instruction.next_pc, exit_depth + 1);
        self.push_runtime_call(Width::W64, instruction.next_pc, exit_depth + 1);
        self.op(Op::End);
        self.commit_runtime_target(exit_depth);
        true
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(in crate::dbt::wasm) fn try_guarded_return_read(
        &mut self,
        instruction: crate::x86::instruction::Instruction,
        exit_depth: u32,
    ) -> bool {
        use crate::dbt::wasm::{STORE_STATUS, memory::indexed::IndexedOperand};
        use crate::embedding::layout::QUIET_RETURN_READ;
        use wasm_encoder::{BlockType, MemArg, ValType};

        if !self.retouch_prefix_fetch
            || !self.paged_return_uses_read_result()
            || !self.uses_indexed_scalar_read(instruction.operation)
            || instruction.next_pc <= instruction.pc
            || (instruction.next_pc - 1) >> 12 != instruction.pc >> 12
        {
            return false;
        }

        self.newest_prefix_fetch(instruction.pc, true, false);
        self.op(Op::If(BlockType::Result(ValType::I32)))
            .op(Op::LocalGet(RSP))
            .op(Op::LocalSet(ADDRESS));
        self.indexed_read_host(
            self.indexed_paged_read.unwrap(),
            Width::W64.bytes(),
            false,
            IndexedOperand::Read,
        );
        self.op(Op::Else)
            .op(Op::I32Const(0))
            .op(Op::End)
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(STORE_STATUS));
        self.direct_scalar_load(
            Width::W64,
            MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            },
        );
        let sampled_tf = self.sampled_tf.unwrap();
        let incoming_shadow = self.incoming_shadow.unwrap();
        // The retained fetch proves a quiet boundary. Leave a missed stack
        // operand and all observable boundaries to the usual begin and read.
        self.op(Op::I32Const(QUIET_RETURN_READ as i32))
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(sampled_tf))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(incoming_shadow))
            .op(Op::Else);
        self.begin(instruction.operation, instruction.next_pc, exit_depth + 1);
        self.stack_word(Width::W64, exit_depth + 1);
        self.op(Op::End);
        true
    }

    pub(in crate::dbt::wasm) fn return_stack(
        &mut self,
        cleanup: u16,
        width: Width,
        exit_depth: u32,
        already_read: bool,
    ) {
        if !already_read {
            self.stack_word(width, exit_depth);
        }
        self.op(Op::LocalGet(VALUE)).op(Op::LocalSet(RHS));
        self.check_runtime_target(exit_depth);
        self.update_stack_pointer(u64::from(width.bytes()) + u64::from(cleanup));
        self.commit_runtime_target(exit_depth);
    }
}
