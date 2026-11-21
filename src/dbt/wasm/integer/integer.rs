use super::{Emitter, FLAGS, GPR, LHS, REMAINING, RESULT, RHS, VALUE};
use crate::x86::instruction::{Alu, DataOperand, Operand, RegisterView, Width};
use wasm_encoder::{BlockType, Instruction as Op};

impl Emitter {
    pub(super) fn narrow(&mut self, width: Width) {
        match width {
            Width::W8 | Width::W16 => {
                self.op(Op::I64Const(width.mask() as i64)).op(Op::I64And);
            }
            Width::W32 => {
                self.op(Op::I32WrapI64).op(Op::I64ExtendI32U);
            }
            Width::W64 => {}
        }
    }

    pub(super) fn register(&mut self, register: u8, width: Width) {
        self.op(Op::LocalGet(GPR + u32::from(register)));
        self.narrow(width);
    }

    pub(super) fn operand(
        &mut self,
        operand: Operand,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        match operand {
            Operand::Register(register) => self.register(register, width),
            Operand::Immediate(value) => {
                self.op(Op::I64Const(value as i64));
                self.narrow(width);
            }
            Operand::Memory(address) => {
                self.load_value(address, width, next_pc, false, exit_depth);
                self.op(Op::LocalGet(VALUE));
            }
        }
    }

    pub(super) fn data_register(&mut self, register: RegisterView, width: Width) {
        self.op(Op::LocalGet(GPR + u32::from(register.bank)));
        if register.high_byte {
            self.op(Op::I64Const(i64::from(register.shift())))
                .op(Op::I64ShrU);
        }
        self.narrow(width);
    }

    pub(super) fn data_operand(&mut self, operand: DataOperand, width: Width) {
        match operand {
            DataOperand::Register(register) => self.data_register(register, width),
            DataOperand::Immediate(value) => {
                self.op(Op::I64Const(value as i64));
                self.narrow(width);
            }
        }
    }

    pub(super) fn write_register(&mut self, register: RegisterView, width: Width) {
        self.merge_register(register, width);
        self.op(Op::LocalSet(GPR + u32::from(register.bank)));
    }

    pub(super) fn merge_register(&mut self, register: RegisterView, width: Width) {
        if width == Width::W32
            && (self.narrow_gprs
                || (self.loop_gates_prechecked
                    && self.register_upper_dwords[usize::from(register.bank)].is_some()))
        {
            return;
        }
        if matches!(width, Width::W8 | Width::W16)
            || (width == Width::W32 && !self.context.is_long64())
        {
            let shift = register.shift();
            if shift != 0 {
                self.op(Op::I64Const(i64::from(shift))).op(Op::I64Shl);
            }
            self.op(Op::LocalGet(GPR + u32::from(register.bank)))
                .op(Op::I64Const(!(width.mask() << shift) as i64))
                .op(Op::I64And)
                .op(Op::I64Or);
        }
    }

    pub(super) fn integer(
        &mut self,
        kind: Alu,
        dst: RegisterView,
        src: DataOperand,
        width: Width,
        overwritten_at: Option<u64>,
    ) {
        match kind {
            Alu::And | Alu::Or | Alu::Xor | Alu::Test => self.logical(kind, dst, src, width),
            Alu::Add | Alu::Sub | Alu::Cmp | Alu::Adc | Alu::Sbb => {
                self.arithmetic(kind, dst, src, width, overwritten_at)
            }
        }
    }

    fn arithmetic(
        &mut self,
        kind: Alu,
        dst: RegisterView,
        src: DataOperand,
        width: Width,
        overwritten_at: Option<u64>,
    ) {
        self.data_register(dst, width);
        self.op(Op::LocalSet(LHS));
        self.data_operand(src, width);
        self.op(Op::LocalSet(RHS));
        if kind == Alu::Cmp && self.omit_result_status {
            return;
        }
        if matches!(kind, Alu::Adc | Alu::Sbb) {
            self.carry_result(kind, width);
            self.carry_flags(kind, width);
        } else {
            self.arithmetic_result(kind, width);
            if !self.omit_result_status
                && let Some(next) = overwritten_at
            {
                // A nonfaulting successor overwrites these flags unless its entry gate exits.
                self.op(Op::Block(BlockType::Empty))
                    .op(Op::LocalGet(REMAINING))
                    .op(Op::I64Const(1))
                    .op(Op::I64GtU)
                    .op(Op::LocalGet(3))
                    .op(Op::LocalGet(2))
                    .op(Op::I64Const(next as i64))
                    .op(Op::I64Eq)
                    .op(Op::I32And)
                    .op(Op::I32Eqz)
                    .op(Op::I32And)
                    .op(Op::BrIf(0));
            }
            if !self.omit_result_status {
                self.arithmetic_flags(kind, width);
                if overwritten_at.is_some() {
                    self.op(Op::End);
                }
            }
        }
        if kind != Alu::Cmp {
            self.op(Op::LocalGet(RESULT));
            self.write_register(dst, width);
        }
    }

    pub(super) fn carry_result(&mut self, kind: Alu, width: Width) {
        self.op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(1))
            .op(Op::I64And)
            .op(Op::LocalSet(VALUE))
            .op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS));
        let add = kind == Alu::Adc;
        self.op(if add { Op::I64Add } else { Op::I64Sub })
            .op(Op::LocalGet(VALUE))
            .op(if add { Op::I64Add } else { Op::I64Sub });
        self.narrow(width);
        self.op(Op::LocalSet(RESULT));
    }

    pub(super) fn carry_flags(&mut self, kind: Alu, width: Width) {
        let add = kind == Alu::Adc;
        // Original operands plus the final result also determine OF and AF
        // with carry. CF additionally covers the equality boundary.
        self.arithmetic_flags(if add { Alu::Add } else { Alu::Sub }, width);
        self.op(Op::LocalGet(if add { RESULT } else { LHS }))
            .op(Op::LocalGet(if add { LHS } else { RHS }))
            .op(Op::I64Eq)
            .op(Op::I64ExtendI32U)
            .op(Op::LocalGet(VALUE))
            .op(Op::I64And)
            .op(Op::LocalGet(FLAGS))
            .op(Op::I64Or)
            .op(Op::LocalSet(FLAGS));
    }

    pub(super) fn arithmetic_result(&mut self, kind: Alu, width: Width) {
        let add = kind == Alu::Add;
        if width == Width::W32 {
            self.op(Op::LocalGet(LHS))
                .op(Op::I32WrapI64)
                .op(Op::LocalGet(RHS))
                .op(Op::I32WrapI64)
                .op(if add { Op::I32Add } else { Op::I32Sub })
                .op(Op::I64ExtendI32U);
        } else {
            self.op(Op::LocalGet(LHS)).op(Op::LocalGet(RHS)).op(if add {
                Op::I64Add
            } else {
                Op::I64Sub
            });
            self.narrow(width);
        }
        self.op(Op::LocalSet(RESULT));
    }

    pub(super) fn arithmetic_flags(&mut self, kind: Alu, width: Width) {
        self.op(Op::I64Const(2));
        if kind == Alu::Add {
            self.op(Op::LocalGet(RESULT)).op(Op::LocalGet(LHS));
        } else {
            self.op(Op::LocalGet(LHS)).op(Op::LocalGet(RHS));
        }
        self.op(Op::I64LtU).op(Op::I64ExtendI32U).op(Op::I64Or);
        self.defer_result_parity();
        self.op(Op::I64Or);
        self.op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::I64Xor)
            .op(Op::LocalGet(RESULT))
            .op(Op::I64Xor)
            .op(Op::I64Const(16))
            .op(Op::I64And)
            .op(Op::I64Or);
        self.op(Op::LocalGet(RESULT));
        if !cfg!(feature = "parallel")
            && self.loop_gates_prechecked
            && self
                .structured_loop
                .is_some_and(|(body, _)| body.forward_edges() && body.order().earlier == body.head)
        {
            // Only zero has 64 leading zeros, the packed ZF bit.
            self.op(Op::I64Clz).op(Op::I64Const(0x40)).op(Op::I64And);
        } else {
            self.op(Op::I64Eqz)
                .op(Op::I64ExtendI32U)
                .op(Op::I64Const(6))
                .op(Op::I64Shl);
        }
        self.op(Op::I64Or);
        self.op(Op::LocalGet(RESULT))
            .op(Op::I64Const(i64::from(width.bits() - 1)))
            .op(Op::I64ShrU)
            .op(Op::I64Const(7))
            .op(Op::I64Shl)
            .op(Op::I64Or);
        self.op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::I64Xor);
        if kind == Alu::Add {
            self.op(Op::I64Const(-1)).op(Op::I64Xor);
        }
        self.op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RESULT))
            .op(Op::I64Xor)
            .op(Op::I64And)
            .op(Op::I64Const(i64::from(width.bits() - 1)))
            .op(Op::I64ShrU)
            .op(Op::I64Const(11))
            .op(Op::I64Shl)
            .op(Op::I64Or)
            .op(Op::LocalSet(FLAGS));
    }

    fn logical(&mut self, kind: Alu, dst: RegisterView, src: DataOperand, width: Width) {
        if width == Width::W32 {
            self.data_register(dst, Width::W64);
            self.op(Op::I32WrapI64);
            match src {
                DataOperand::Register(register) => {
                    self.data_register(register, Width::W64);
                    self.op(Op::I32WrapI64);
                }
                DataOperand::Immediate(value) => {
                    self.op(Op::I32Const(value as i32));
                }
            }
            self.op(match kind {
                Alu::And | Alu::Test => Op::I32And,
                Alu::Or => Op::I32Or,
                Alu::Xor => Op::I32Xor,
                _ => unreachable!(),
            })
            .op(Op::I64ExtendI32U)
            .op(Op::LocalSet(RESULT));
        } else {
            self.data_register(dst, width);
            self.data_operand(src, width);
            self.logical_result(kind);
        }
        if !self.omit_result_status {
            self.logical_flags(width);
        }
        if kind != Alu::Test {
            self.op(Op::LocalGet(RESULT));
            self.write_register(dst, width);
        }
    }

    pub(super) fn logical_result(&mut self, kind: Alu) {
        self.op(match kind {
            Alu::And | Alu::Test => Op::I64And,
            Alu::Or => Op::I64Or,
            Alu::Xor => Op::I64Xor,
            _ => unreachable!(),
        })
        .op(Op::LocalSet(RESULT));
    }

    pub(super) fn logical_flags(&mut self, width: Width) {
        self.op(Op::I64Const(2));
        self.defer_result_parity();
        self.op(Op::I64Or)
            .op(Op::LocalGet(RESULT))
            .op(Op::I64Eqz)
            .op(Op::I64ExtendI32U)
            .op(Op::I64Const(6))
            .op(Op::I64Shl)
            .op(Op::I64Or)
            .op(Op::LocalGet(RESULT))
            .op(Op::I64Const(i64::from(width.bits() - 1)))
            .op(Op::I64ShrU)
            .op(Op::I64Const(7))
            .op(Op::I64Shl)
            .op(Op::I64Or)
            .op(Op::LocalSet(FLAGS));
    }
}
