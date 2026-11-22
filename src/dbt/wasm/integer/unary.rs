use super::{Emitter, FLAGS, LHS, RESULT, RHS, VALUE};
use crate::x86::instruction::{Alu, Carry, Location, RegisterView, Unary, Width};
use wasm_encoder::Instruction as Op;

impl Emitter {
    pub(super) fn read_location(
        &mut self,
        location: Location,
        width: Width,
        next_pc: u64,
        for_write: bool,
        exit_depth: u32,
    ) {
        match location {
            Location::Register(register) => self.data_register(register, width),
            Location::Memory(address) => {
                self.load_value(address, width, next_pc, for_write, exit_depth);
                self.op(Op::LocalGet(VALUE));
                self.narrow(width);
            }
        }
        self.op(Op::LocalSet(LHS));
    }

    pub(super) fn write_location(&mut self, location: Location, width: Width, exit_depth: u32) {
        match location {
            Location::Register(register) => {
                self.op(Op::LocalGet(RESULT));
                self.write_register(register, width);
            }
            Location::Memory(_) => self.store_result(width, exit_depth),
        }
    }

    pub(super) fn unary(
        &mut self,
        kind: Unary,
        dst: Location,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let proof = match dst {
            Location::Memory(address) => self.load_cached_rmw(address, width, next_pc, exit_depth),
            _ => None,
        };
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let direct = proof.is_some();
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        let direct = false;
        if direct {
            self.op(Op::LocalGet(VALUE)).op(Op::LocalSet(LHS));
        } else {
            self.read_location(dst, width, next_pc, true, exit_depth);
        }
        match kind {
            Unary::Not => {
                self.op(Op::LocalGet(LHS))
                    .op(Op::I64Const(width.mask() as i64))
                    .op(Op::I64Xor)
                    .op(Op::LocalSet(RESULT));
            }
            Unary::Inc | Unary::Dec => {
                self.op(Op::I64Const(1)).op(Op::LocalSet(RHS));
                self.arithmetic_result(
                    if kind == Unary::Inc {
                        Alu::Add
                    } else {
                        Alu::Sub
                    },
                    width,
                );
            }
            Unary::Neg => {
                self.op(Op::LocalGet(LHS))
                    .op(Op::LocalSet(RHS))
                    .op(Op::I64Const(0))
                    .op(Op::LocalSet(LHS));
                self.arithmetic_result(Alu::Sub, width);
            }
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(proof) = proof {
            self.store_cached_rmw(proof, width, exit_depth);
        } else {
            self.write_location(dst, width, exit_depth);
        }
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        self.write_location(dst, width, exit_depth);
        match kind {
            Unary::Not => {}
            Unary::Inc if self.omit_result_status => {}
            Unary::Inc | Unary::Dec => {
                self.op(Op::LocalGet(FLAGS))
                    .op(Op::I64Const(1))
                    .op(Op::I64And)
                    .op(Op::LocalSet(VALUE));
                self.arithmetic_flags(
                    if kind == Unary::Inc {
                        Alu::Add
                    } else {
                        Alu::Sub
                    },
                    width,
                );
                self.op(Op::LocalGet(FLAGS))
                    .op(Op::I64Const(!1))
                    .op(Op::I64And)
                    .op(Op::LocalGet(VALUE))
                    .op(Op::I64Or)
                    .op(Op::LocalSet(FLAGS));
            }
            Unary::Neg if !self.omit_result_status => self.arithmetic_flags(Alu::Sub, width),
            Unary::Neg => {}
        }
    }

    pub(super) fn carry(&mut self, kind: Carry) {
        self.op(Op::LocalGet(FLAGS));
        match kind {
            Carry::Clear => {
                self.op(Op::I64Const(!1)).op(Op::I64And);
            }
            Carry::Set => {
                self.op(Op::I64Const(1)).op(Op::I64Or);
            }
            Carry::Complement => {
                self.op(Op::I64Const(1)).op(Op::I64Xor);
            }
        }
        self.op(Op::LocalSet(FLAGS));
    }

    pub(super) fn lahf(&mut self) {
        self.status_flags();
        self.op(Op::I64Const(0xd5))
            .op(Op::I64And)
            .op(Op::I64Const(2))
            .op(Op::I64Or);
        self.write_register(RegisterView::high_byte(0), Width::W8);
    }

    pub(super) fn sahf(&mut self) {
        self.data_register(RegisterView::high_byte(0), Width::W8);
        self.op(Op::I64Const(0xd5))
            .op(Op::I64And)
            .op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(0x802))
            .op(Op::I64And)
            .op(Op::I64Or)
            .op(Op::LocalSet(FLAGS));
    }
}
