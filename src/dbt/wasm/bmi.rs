use super::{Emitter, FLAGS, LHS, RESULT, RHS};
use crate::x86::instruction::{Bmi, Location, RegisterView, Width};
use wasm_encoder::{BlockType, Instruction as Op, ValType};

impl Emitter {
    pub(super) fn bmi(
        &mut self,
        kind: Bmi,
        dst: RegisterView,
        src: Location,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        self.read_location(src, width, next_pc, false, exit_depth);
        match kind {
            Bmi::AndNot(first) => {
                self.data_register(first, width);
                self.op(Op::I64Const(-1))
                    .op(Op::I64Xor)
                    .op(Op::LocalGet(LHS))
                    .op(Op::I64And);
            }
            Bmi::Extract(control) => self.bextr(control, width),
            Bmi::Isolate => {
                self.op(Op::I64Const(0))
                    .op(Op::LocalGet(LHS))
                    .op(Op::I64Sub)
                    .op(Op::LocalGet(LHS))
                    .op(Op::I64And);
            }
            Bmi::Mask | Bmi::Reset => {
                self.op(Op::LocalGet(LHS))
                    .op(Op::LocalGet(LHS))
                    .op(Op::I64Const(1))
                    .op(Op::I64Sub);
                if kind == Bmi::Mask {
                    self.op(Op::I64Xor);
                    self.narrow(width);
                } else {
                    self.op(Op::I64And);
                }
            }
            Bmi::TrailingZeros => {
                self.op(Op::LocalGet(LHS));
                // A sentinel above a narrow operand makes ctz(0) its x86 width.
                if width != Width::W64 {
                    self.op(Op::I64Const(1i64 << width.bits())).op(Op::I64Or);
                }
                self.op(Op::I64Ctz);
            }
        }
        self.op(Op::LocalTee(RESULT));
        self.write_register(dst, width);

        self.op(Op::I64Const(2));
        match kind {
            Bmi::Isolate | Bmi::Mask | Bmi::Reset | Bmi::TrailingZeros => {
                self.op(Op::LocalGet(LHS)).op(Op::I64Eqz);
                if kind == Bmi::Isolate {
                    self.op(Op::I32Eqz);
                }
                self.op(Op::I64ExtendI32U).op(Op::I64Or);
            }
            _ => {}
        }
        if kind != Bmi::Mask {
            self.op(Op::LocalGet(RESULT))
                .op(Op::I64Eqz)
                .op(Op::I64ExtendI32U)
                .op(Op::I64Const(6))
                .op(Op::I64Shl)
                .op(Op::I64Or);
        }
        if matches!(kind, Bmi::AndNot(_) | Bmi::Isolate | Bmi::Mask | Bmi::Reset) {
            self.op(Op::LocalGet(RESULT))
                .op(Op::I64Const(i64::from(width.bits() - 1)))
                .op(Op::I64ShrU)
                .op(Op::I64Const(7))
                .op(Op::I64Shl)
                .op(Op::I64Or);
        }
        self.op(Op::LocalSet(FLAGS));
    }

    fn bextr(&mut self, control: RegisterView, width: Width) {
        self.data_register(control, Width::W8);
        self.op(Op::LocalTee(RHS))
            .op(Op::I64Const(i64::from(width.bits())))
            .op(Op::I64LtU)
            .op(Op::If(BlockType::Result(ValType::I64)))
            .op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::I64ShrU);
        // The source is zero-extended. Lengths >= 64 keep every remaining bit.
        self.data_register(control, Width::W16);
        self.op(Op::I64Const(8))
            .op(Op::I64ShrU)
            .op(Op::LocalSet(RHS))
            .op(Op::I64Const(1))
            .op(Op::LocalGet(RHS))
            .op(Op::I64Shl)
            .op(Op::I64Const(1))
            .op(Op::I64Sub)
            .op(Op::I64Const(-1))
            .op(Op::LocalGet(RHS))
            .op(Op::I64Const(64))
            .op(Op::I64LtU)
            .op(Op::Select)
            .op(Op::I64And)
            .op(Op::Else)
            .op(Op::I64Const(0))
            .op(Op::End);
    }
}
