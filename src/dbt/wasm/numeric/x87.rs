#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::dbt::wasm::{ADDRESS, LHS, REMAINING, RESULT, RHS, RIP, SHADOW, VALUE};
use crate::dbt::wasm::{Emitter, FLAGS, GPR, REASON, STORE_STATUS};
use crate::embedding::layout::RunState;
use crate::x86::context::Segment;
use crate::x86::instruction::X87Instruction;
use crate::x86::x87::Kind;
use std::mem::offset_of;
use wasm_encoder::{Instruction as Wasm, MemArg};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::x86::{
    instruction::{Instruction, Operation},
    x87::{
        Format, MEMORY_FIRST_PAIR_SEGMENT, MEMORY_PAIR_SEGMENT, Op, PAGED_LOAD_PAIR_SEGMENT,
        PAGED_STORE_PAIR_SEGMENT, REGISTER_PAIR_SEGMENT, memory_first_pair,
        register_exchange_compare, register_load_arithmetic, register_load_memory_arithmetic,
        register_store_pop,
    },
};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use wasm_encoder::BlockType;

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) fn x87_pair(
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
pub(in crate::dbt::wasm) fn paged_x87_memory_pair(first: Instruction, second: Instruction) -> bool {
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

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) fn inline_x87_bank_operation(op: Op) -> bool {
    register_store_pop(op)
        || op.kind == Kind::Constant && op.index == 6
        || op.kind == Kind::Sub && op.format == Format::Register && !op.dest_index && op.pop == 0
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn x87_mem(offset: usize, align: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align,
        memory_index: 0,
    }
}

impl Emitter {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn can_inline_x87_bank_operation(&self) -> bool {
        use crate::x86::context::Mode;
        self.x87_register_pairs
            && !self.observed
            && !self.paged
            && self.structured_loop.is_none()
            && (self.context.is_long64()
                || self.context.mode() == Mode::Protected && self.context.code32())
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn inline_x87_stack_bank(&mut self, pc: u64, delta: i32, destination: u8) {
        use crate::x86::x87::State;
        let x87 = offset_of!(RunState, x87);
        let mem = x87_mem;
        let status = mem(x87 + offset_of!(State, status_word), 1);
        let control = mem(x87 + offset_of!(State, control_word), 1);
        let tags = mem(x87 + offset_of!(State, tag_word), 1);
        let push_zero = delta == -1;
        // The ordinary helper handles pending exceptions and stack faults.
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
            .op(Wasm::LocalGet(REASON));
        if push_zero {
            self.op(Wasm::I32Const(0x800)).op(Wasm::I32Sub);
        }
        self.op(Wasm::I32Const(11))
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
            .op(if push_zero { Wasm::I32Eq } else { Wasm::I32Ne })
            .op(Wasm::I32And)
            .op(Wasm::If(BlockType::Empty));

        self.op(Wasm::LocalGet(0))
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::I32Const(!(0x8080 | 0x3800 | 0x200)))
            .op(Wasm::I32And)
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::I32Const(delta << 11))
            .op(Wasm::I32Add)
            .op(Wasm::I32Const(0x3800))
            .op(Wasm::I32And)
            .op(Wasm::I32Or)
            .op(Wasm::I32Store16(status));

        let reclassify = destination != 0 && delta == 1;
        if push_zero {
            let registers = x87 + offset_of!(State, registers);
            self.op(Wasm::LocalGet(0))
                .op(Wasm::LocalGet(STORE_STATUS))
                .op(Wasm::I32Const(10))
                .op(Wasm::I32Mul)
                .op(Wasm::I32Add)
                .op(Wasm::LocalTee(REASON))
                .op(Wasm::I64Const(0))
                .op(Wasm::I64Store(mem(registers, 0)))
                .op(Wasm::LocalGet(REASON))
                .op(Wasm::I32Const(0))
                .op(Wasm::I32Store16(mem(registers + 8, 0)));
        } else if destination != 0 {
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
        if push_zero {
            // The proven empty tag (3) becomes the zero tag (1).
            self.op(Wasm::I32Const(2))
                .op(Wasm::LocalGet(STORE_STATUS))
                .op(Wasm::I32Const(1))
                .op(Wasm::I32Shl)
                .op(Wasm::I32Shl)
                .op(Wasm::I32Xor);
        } else {
            for top in [STORE_STATUS, REASON].into_iter().take(delta as usize) {
                self.op(Wasm::I32Const(3))
                    .op(Wasm::LocalGet(top))
                    .op(Wasm::I32Const(1))
                    .op(Wasm::I32Shl)
                    .op(Wasm::I32Shl)
                    .op(Wasm::I32Or);
            }
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
        self.op(Wasm::I32Store16(tags));
        self.inline_x87_environment(pc);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn inline_x87_operands(&mut self, index: u8, pc64: bool) {
        use crate::x86::x87::State;
        let bank = offset_of!(RunState, x87);
        let status = x87_mem(bank + offset_of!(State, status_word), 1);
        let control = x87_mem(bank + offset_of!(State, control_word), 1);
        let tags = x87_mem(bank + offset_of!(State, tag_word), 1);
        let significand = x87_mem(bank + offset_of!(State, registers), 0);
        // Waiting instructions cannot bypass pending exceptions or empty tags.
        self.op(Wasm::LocalGet(0))
            .op(Wasm::I32Load16U(status))
            .op(Wasm::LocalTee(REASON))
            .op(Wasm::LocalGet(0));
        if pc64 {
            self.op(Wasm::I64Load16U(control))
                .op(Wasm::LocalTee(VALUE))
                .op(Wasm::I32WrapI64);
        } else {
            self.op(Wasm::I32Load16U(control));
        }
        self.op(Wasm::I32Const(-1))
            .op(Wasm::I32Xor)
            .op(Wasm::I32And)
            .op(Wasm::I32Const(63))
            .op(Wasm::I32And)
            .op(Wasm::I32Eqz);
        if pc64 {
            // Both precision encodings with PC bit 8 set use 64 significand bits.
            self.op(Wasm::LocalGet(VALUE))
                .op(Wasm::I32WrapI64)
                .op(Wasm::I32Const(0x100))
                .op(Wasm::I32And)
                .op(Wasm::I32Eqz)
                .op(Wasm::I32Eqz)
                .op(Wasm::I32And);
        }
        self.op(Wasm::LocalGet(REASON))
            .op(Wasm::I32Const(11))
            .op(Wasm::I32ShrU)
            .op(Wasm::I32Const(7))
            .op(Wasm::I32And)
            .op(Wasm::LocalSet(STORE_STATUS));
        for (logical, pointer, raw) in [(0, ADDRESS, LHS), (index, VALUE, RHS)] {
            self.op(Wasm::LocalGet(0))
                .op(Wasm::I32Load16U(tags))
                .op(Wasm::LocalGet(STORE_STATUS));
            if logical != 0 {
                self.op(Wasm::I32Const(i32::from(logical)))
                    .op(Wasm::I32Add)
                    .op(Wasm::I32Const(7))
                    .op(Wasm::I32And);
            }
            self.op(Wasm::I64ExtendI32U)
                .op(Wasm::LocalTee(pointer))
                .op(Wasm::I32WrapI64)
                .op(Wasm::I32Const(1))
                .op(Wasm::I32Shl)
                .op(Wasm::I32ShrU)
                .op(Wasm::I32Const(3))
                .op(Wasm::I32And)
                .op(Wasm::I32Const(3))
                .op(Wasm::I32Ne)
                .op(Wasm::I32And)
                .op(Wasm::LocalGet(0))
                .op(Wasm::LocalGet(pointer))
                .op(Wasm::I32WrapI64)
                .op(Wasm::I32Const(10))
                .op(Wasm::I32Mul)
                .op(Wasm::I32Add)
                .op(Wasm::I64ExtendI32U)
                .op(Wasm::LocalTee(pointer))
                .op(Wasm::I32WrapI64)
                .op(Wasm::I64Load(significand))
                .op(Wasm::LocalSet(raw));
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn inline_x87_subtract(&mut self, pc: u64, index: u8) {
        use crate::x86::x87::State;
        let bank = offset_of!(RunState, x87);
        let status = x87_mem(bank + offset_of!(State, status_word), 1);
        let control = x87_mem(bank + offset_of!(State, control_word), 1);
        let tags = x87_mem(bank + offset_of!(State, tag_word), 1);
        let registers = bank + offset_of!(State, registers);
        let significand = x87_mem(registers, 0);
        let exponent = x87_mem(registers + 8, 0);

        self.inline_x87_operands(index, true);
        // Canonical short significands with equal signed exponents subtract
        // exactly. The normalization is the equal-exponent case of the Rust kernel.
        self.op(Wasm::LocalGet(LHS))
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::I64Or)
            .op(Wasm::I32WrapI64)
            .op(Wasm::I32Eqz)
            .op(Wasm::I32And)
            .op(Wasm::LocalGet(LHS))
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::I64And)
            .op(Wasm::I64Const(0))
            .op(Wasm::I64LtS)
            .op(Wasm::I32And)
            .op(Wasm::LocalGet(LHS))
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::I64LtU)
            .op(Wasm::LocalSet(REASON))
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::LocalGet(LHS))
            .op(Wasm::I64Sub)
            .op(Wasm::LocalGet(LHS))
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::I64Sub)
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::Select)
            .op(Wasm::LocalSet(RESULT))
            .op(Wasm::LocalGet(ADDRESS))
            .op(Wasm::I32WrapI64)
            .op(Wasm::I64Load16U(exponent))
            .op(Wasm::LocalTee(LHS))
            .op(Wasm::LocalGet(VALUE))
            .op(Wasm::I32WrapI64)
            .op(Wasm::I64Load16U(exponent))
            .op(Wasm::I64Eq)
            .op(Wasm::I32And)
            .op(Wasm::LocalGet(LHS))
            .op(Wasm::I64Const(0x7fff))
            .op(Wasm::I64And)
            .op(Wasm::LocalTee(RHS))
            .op(Wasm::I64Const(1))
            .op(Wasm::I64Sub)
            .op(Wasm::I64Const(0x7ffe))
            .op(Wasm::I64LtU)
            .op(Wasm::I32And)
            .op(Wasm::LocalGet(RESULT))
            .op(Wasm::I64Clz)
            .op(Wasm::LocalTee(VALUE))
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::I64LtU)
            .op(Wasm::LocalGet(RESULT))
            .op(Wasm::I64Eqz)
            .op(Wasm::I32Or)
            .op(Wasm::I32And)
            .op(Wasm::If(BlockType::Empty));

        self.op(Wasm::LocalGet(ADDRESS))
            .op(Wasm::I32WrapI64)
            .op(Wasm::LocalGet(RESULT))
            .op(Wasm::LocalGet(VALUE))
            .op(Wasm::I64Shl)
            .op(Wasm::I64Store(significand))
            .op(Wasm::LocalGet(ADDRESS))
            .op(Wasm::I32WrapI64)
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::LocalGet(VALUE))
            .op(Wasm::I64Sub)
            .op(Wasm::LocalGet(LHS))
            .op(Wasm::I64Const(0x8000))
            .op(Wasm::I64And)
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::I64ExtendI32U)
            .op(Wasm::I64Const(15))
            .op(Wasm::I64Shl)
            .op(Wasm::I64Xor)
            .op(Wasm::I64Or)
            .op(Wasm::LocalGet(0))
            .op(Wasm::I64Load16U(control))
            .op(Wasm::I64Const(0xc00))
            .op(Wasm::I64And)
            .op(Wasm::I64Const(0x400))
            .op(Wasm::I64Eq)
            .op(Wasm::I64ExtendI32U)
            .op(Wasm::I64Const(15))
            .op(Wasm::I64Shl)
            .op(Wasm::LocalGet(RESULT))
            .op(Wasm::I64Eqz)
            .op(Wasm::I32Eqz)
            .op(Wasm::Select)
            .op(Wasm::I64Store16(exponent))
            .op(Wasm::LocalGet(0))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I32Load16U(tags))
            .op(Wasm::I32Const(3))
            .op(Wasm::LocalGet(STORE_STATUS))
            .op(Wasm::I32Const(1))
            .op(Wasm::I32Shl)
            .op(Wasm::LocalTee(REASON))
            .op(Wasm::I32Shl)
            .op(Wasm::I32Const(-1))
            .op(Wasm::I32Xor)
            .op(Wasm::I32And)
            .op(Wasm::LocalGet(RESULT))
            .op(Wasm::I64Eqz)
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::I32Shl)
            .op(Wasm::I32Or)
            .op(Wasm::I32Store16(tags))
            .op(Wasm::LocalGet(0))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I32Load16U(status))
            .op(Wasm::I32Const(!(0x8080 | 0x200)))
            .op(Wasm::I32And)
            .op(Wasm::I32Store16(status));
        self.inline_x87_environment(pc);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn inline_x87_environment(&mut self, pc: u64) {
        use crate::x86::x87::State;
        let saved_pc = if self.context.is_long64() {
            pc
        } else {
            u64::from(pc as u32)
        };
        self.op(Wasm::LocalGet(0))
            .op(Wasm::I64Const(saved_pc as i64))
            .op(Wasm::I64Store(x87_mem(
                offset_of!(RunState, x87) + offset_of!(State, instruction_pointer),
                3,
            )))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I64Const(0))
            .op(Wasm::I64Store(x87_mem(offset_of!(RunState, target), 3)))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I32Const(Segment::Cs as i32))
            .op(Wasm::I32Store(x87_mem(
                offset_of!(RunState, fault_segment),
                2,
            )))
            .op(Wasm::I32Const(0))
            .op(Wasm::LocalSet(REASON));
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn inline_x87_exchange_compare(&mut self, pc: u64, index: u8, pop: bool) {
        use crate::x86::x87::State;
        let bank = offset_of!(RunState, x87);
        let status = x87_mem(bank + offset_of!(State, status_word), 1);
        let tags = x87_mem(bank + offset_of!(State, tag_word), 1);
        let registers = bank + offset_of!(State, registers);
        let significand = x87_mem(registers, 0);
        let exponent = x87_mem(registers + 8, 0);
        self.inline_x87_operands(index, false);
        self.op(Wasm::LocalGet(LHS))
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::I64And)
            .op(Wasm::I64Const(0))
            .op(Wasm::I64LtS)
            .op(Wasm::I32And)
            .op(Wasm::LocalGet(ADDRESS))
            .op(Wasm::I32WrapI64)
            .op(Wasm::I64Load16U(exponent))
            .op(Wasm::LocalTee(RESULT))
            .op(Wasm::I32WrapI64);
        for pointer in [ADDRESS, VALUE] {
            if pointer == VALUE {
                self.op(Wasm::LocalGet(VALUE))
                    .op(Wasm::I32WrapI64)
                    .op(Wasm::I32Load16U(exponent))
                    .op(Wasm::LocalTee(REASON));
            }
            self.op(Wasm::I32Const(0x7fff))
                .op(Wasm::I32And)
                .op(Wasm::I32Const(1))
                .op(Wasm::I32Sub)
                .op(Wasm::I32Const(0x7ffe))
                .op(Wasm::I32LtU)
                .op(Wasm::I32And);
        }
        self.op(Wasm::If(BlockType::Empty));
        for (pointer, raw, signed_exp, store) in [
            (ADDRESS, RHS, REASON, Wasm::I32Store16(exponent)),
            (VALUE, LHS, RESULT, Wasm::I64Store16(exponent)),
        ] {
            self.op(Wasm::LocalGet(pointer))
                .op(Wasm::I32WrapI64)
                .op(Wasm::LocalGet(raw))
                .op(Wasm::I64Store(significand))
                .op(Wasm::LocalGet(pointer))
                .op(Wasm::I32WrapI64)
                .op(Wasm::LocalGet(signed_exp))
                .op(store);
        }

        // The exchange makes the original ST(i) the left comparison operand.
        // For equal signs, (exponent, significand) orders the magnitudes.
        self.op(Wasm::LocalGet(REASON))
            .op(Wasm::I64ExtendI32U)
            .op(Wasm::LocalGet(RESULT))
            .op(Wasm::I64Eq)
            .op(Wasm::I64ExtendI32U)
            .op(Wasm::LocalSet(VALUE))
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::LocalGet(LHS))
            .op(Wasm::I64Eq)
            .op(Wasm::LocalGet(VALUE))
            .op(Wasm::I32WrapI64)
            .op(Wasm::I32And)
            .op(Wasm::I64ExtendI32U)
            .op(Wasm::LocalSet(ADDRESS))
            .op(Wasm::LocalGet(0));
        self.full_flags();
        self.op(Wasm::I64Const(!0x8d5))
            .op(Wasm::I64And)
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::I64ExtendI32U)
            .op(Wasm::LocalGet(RESULT))
            .op(Wasm::I64LtU)
            .op(Wasm::LocalGet(VALUE))
            .op(Wasm::I32WrapI64)
            .op(Wasm::LocalGet(RHS))
            .op(Wasm::LocalGet(LHS))
            .op(Wasm::I64LtU)
            .op(Wasm::I32And)
            .op(Wasm::I32Or)
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::LocalGet(RESULT))
            .op(Wasm::I32WrapI64)
            .op(Wasm::I32Xor)
            .op(Wasm::I32Const(15))
            .op(Wasm::I32ShrU)
            .op(Wasm::I32Eqz)
            .op(Wasm::I32And)
            .op(Wasm::LocalGet(REASON))
            .op(Wasm::I32Const(15))
            .op(Wasm::I32ShrU)
            .op(Wasm::I32Xor)
            .op(Wasm::LocalGet(ADDRESS))
            .op(Wasm::I64Eqz)
            .op(Wasm::I32And)
            .op(Wasm::I64ExtendI32U)
            .op(Wasm::I64Or)
            .op(Wasm::LocalGet(ADDRESS))
            .op(Wasm::I64Const(6))
            .op(Wasm::I64Shl)
            .op(Wasm::I64Or)
            .op(Wasm::I64Store(x87_mem(offset_of!(RunState, flags), 3)));
        self.reload_x87_flags();

        self.op(Wasm::LocalGet(0))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I32Load16U(tags));
        for (logical, empty) in [(0, pop), (index, false)] {
            self.op(Wasm::I32Const(3)).op(Wasm::LocalGet(STORE_STATUS));
            if logical != 0 {
                self.op(Wasm::I32Const(i32::from(logical)))
                    .op(Wasm::I32Add)
                    .op(Wasm::I32Const(7))
                    .op(Wasm::I32And);
            }
            self.op(Wasm::I32Const(1)).op(Wasm::I32Shl).op(Wasm::I32Shl);
            if empty {
                self.op(Wasm::I32Or);
            } else {
                self.op(Wasm::I32Const(-1))
                    .op(Wasm::I32Xor)
                    .op(Wasm::I32And);
            }
        }
        self.op(Wasm::I32Store16(tags))
            .op(Wasm::LocalGet(0))
            .op(Wasm::LocalGet(0))
            .op(Wasm::I32Load16U(status));
        if pop {
            self.op(Wasm::LocalTee(REASON))
                .op(Wasm::I32Const(!(0x8080 | 0x200 | 0x3800)))
                .op(Wasm::I32And)
                .op(Wasm::LocalGet(REASON))
                .op(Wasm::I32Const(0x800))
                .op(Wasm::I32Add)
                .op(Wasm::I32Const(0x3800))
                .op(Wasm::I32And)
                .op(Wasm::I32Or);
        } else {
            self.op(Wasm::I32Const(!(0x8080 | 0x200))).op(Wasm::I32And);
        }
        self.op(Wasm::I32Store16(status));
        self.inline_x87_environment(pc);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(in crate::dbt::wasm) fn emit_x87_pair(
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
        let pop_pair = register_store_pop(a.op)
            && a.op.index == 1
            && register_store_pop(b.op)
            && b.op.index == 0;
        let exchange_compare =
            register_exchange_compare(a.op, b.op) && a.op.index != 0 && a.op.index == b.op.index;
        if (pop_pair || exchange_compare) && self.can_inline_x87_bank_operation() {
            if pop_pair {
                self.inline_x87_stack_bank(second.pc, 2, 1);
            } else {
                self.inline_x87_exchange_compare(second.pc, a.op.index, b.op.pop != 0);
            }
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

    pub(in crate::dbt::wasm) fn x87(
        &mut self,
        instruction: X87Instruction,
        pc: u64,
        next_pc: u64,
        exit_depth: u32,
    ) {
        let operation = instruction.op;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let inlined = inline_x87_bank_operation(operation) && self.can_inline_x87_bank_operation();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if inlined {
            match operation.kind {
                Kind::Constant => self.inline_x87_stack_bank(pc, -1, 0),
                Kind::Sub => self.inline_x87_subtract(pc, operation.index),
                _ => self.inline_x87_stack_bank(pc, 1, operation.index),
            }
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
