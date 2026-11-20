use super::{ADDRESS, Emitter, FLAGS, LHS, REASON, RESULT, RHS, STORE_STATUS};
use crate::embedding::layout::RunState;
use crate::x86::float::{Format, Kind};
use crate::x86::instruction::{Address, FloatInstruction, Location, RegisterView, Width};
use std::mem::offset_of;
use wasm_encoder::{BlockType, Instruction as Op, ValType};

const ROUNDING_CONTROL: i32 = 0x6000;
const PRECISION_STATUS: i32 = 1 << 5;
const PRECISION_MASK: i32 = 1 << 12;

impl Emitter {
    pub(super) fn float(&mut self, instruction: FloatInstruction, next_pc: u64, exit_depth: u32) {
        let [left, right, result] = self.vector_temps();
        if instruction.destination_is_gpr() {
            self.op(Op::V128Const(0));
        } else {
            self.op(Op::LocalGet(self.xmm(instruction.dst)));
        }
        self.op(Op::LocalSet(left));
        match instruction.src {
            Location::Register(src) if instruction.source_is_gpr() => {
                self.data_register(
                    src,
                    if instruction.op.integer64 {
                        Width::W64
                    } else {
                        Width::W32
                    },
                );
                self.op(Op::LocalSet(super::VALUE))
                    .op(Op::V128Const(0))
                    .op(Op::LocalGet(super::VALUE))
                    .op(Op::I64x2ReplaceLane(0));
            }
            Location::Register(src) => {
                self.op(Op::LocalGet(self.xmm(src.bank)));
            }
            Location::Memory(address) => {
                let bytes = instruction.source_bytes();
                self.vector_load_value(address, bytes, bytes == 16, next_pc, exit_depth);
            }
        }
        self.op(Op::LocalSet(right));
        if instruction.op.scalar
            && instruction.op.format == Format::F32
            && matches!(instruction.op.kind, Kind::Add | Kind::Mul)
        {
            self.op(Op::Block(BlockType::Result(ValType::V128)))
                .op(Op::Block(BlockType::Empty));
            self.float_f32(instruction.op.kind, left, right, result);
            self.op(Op::Br(1)).op(Op::End);
            self.float_helper(instruction, left, right, exit_depth + 1);
            self.op(Op::End);
        } else {
            self.float_helper(instruction, left, right, exit_depth);
        }
        self.op(Op::LocalSet(result));
        if instruction.writes_flags() {
            self.op(Op::LocalGet(FLAGS))
                .op(Op::I64Const(!0x8d5i64))
                .op(Op::I64And)
                .op(Op::LocalGet(result))
                .op(Op::I64x2ExtractLane(0))
                .op(Op::I64Or)
                .op(Op::LocalSet(FLAGS));
        } else if instruction.destination_is_gpr() {
            self.op(Op::LocalGet(result)).op(Op::I64x2ExtractLane(0));
            let width = if instruction.op.integer64 {
                Width::W64
            } else {
                Width::W32
            };
            self.narrow(width);
            self.write_register(RegisterView::low(instruction.dst), width);
        } else {
            let destination = self.xmm(instruction.dst);
            self.op(Op::LocalGet(result)).op(Op::LocalSet(destination));
        }
    }

    fn float_helper(
        &mut self,
        instruction: FloatInstruction,
        left: u32,
        right: u32,
        exit_depth: u32,
    ) {
        let helper = self.float.expect("numeric import");
        self.op(Op::I32Const(instruction.op.encode() as i32))
            .op(Op::LocalGet(left))
            .op(Op::I64x2ExtractLane(0))
            .op(Op::LocalGet(left))
            .op(Op::I64x2ExtractLane(1))
            .op(Op::LocalGet(right))
            .op(Op::I64x2ExtractLane(0))
            .op(Op::LocalGet(right))
            .op(Op::I64x2ExtractLane(1))
            .op(Op::Call(helper))
            .op(Op::LocalTee(REASON))
            .op(Op::BrIf(exit_depth));
        self.op(Op::LocalGet(0))
            .op(Op::V128Load(super::vector::memory(offset_of!(
                RunState,
                vector_scratch
            ))));
    }

    fn float_f32(&mut self, kind: Kind, left: u32, right: u32, result: u32) {
        let mxcsr = super::vector::memory(offset_of!(RunState, mxcsr));
        self.op(Op::LocalGet(0))
            .op(Op::I32Load(mxcsr))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Const(ROUNDING_CONTROL))
            .op(Op::I32And)
            .op(Op::BrIf(0));

        for (vector, bits) in [(left, LHS), (right, RHS)] {
            self.op(Op::LocalGet(vector))
                .op(Op::F32x4ExtractLane(0))
                .op(Op::F64PromoteF32)
                .op(Op::I64ReinterpretF64)
                .op(Op::LocalSet(bits));
            self.reject_non_normal_f32(bits);
        }

        self.float_f64(LHS);
        self.float_f64(RHS);
        self.op(if kind == Kind::Add {
            Op::F64Add
        } else {
            Op::F64Mul
        })
        .op(Op::I64ReinterpretF64)
        .op(Op::LocalSet(RESULT));
        // Check before rounding so a tiny product that rounds to a normal f32 still falls back.
        self.reject_non_normal_f32(RESULT);

        self.op(Op::LocalGet(left));
        self.float_f64(RESULT);
        self.op(Op::F32DemoteF64)
            .op(Op::F32x4ReplaceLane(0))
            .op(Op::LocalSet(result));

        let masked_sticky_precision = PRECISION_MASK | PRECISION_STATUS;
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Const(masked_sticky_precision))
            .op(Op::I32And)
            .op(Op::I32Const(masked_sticky_precision))
            .op(Op::I32Ne)
            .op(Op::If(BlockType::Empty));
        self.op(Op::LocalGet(result))
            .op(Op::F32x4ExtractLane(0))
            .op(Op::F64PromoteF32);
        self.float_f64(RESULT);
        self.op(Op::F64Ne);
        if kind == Kind::Add {
            // A product of two f32 significands fits in f64. An f32 sum may lose low bits
            // in f64 only when the exponents are so far apart that it rounds back to the
            // larger operand in f32. If both operands are nonzero that sum is inexact.
            self.float_f64(RESULT);
            self.float_f64(LHS);
            self.op(Op::F64Eq);
            self.float_f64(RHS);
            self.op(Op::F64Const(0.0.into()))
                .op(Op::F64Ne)
                .op(Op::I32And);
            self.float_f64(RESULT);
            self.float_f64(RHS);
            self.op(Op::F64Eq);
            self.float_f64(LHS);
            self.op(Op::F64Const(0.0.into()))
                .op(Op::F64Ne)
                .op(Op::I32And)
                .op(Op::I32Or)
                .op(Op::I32Or);
        }
        self.op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Const(PRECISION_MASK))
            .op(Op::I32And)
            .op(Op::I32Eqz)
            .op(Op::BrIf(2))
            .op(Op::LocalGet(0))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Const(PRECISION_STATUS))
            .op(Op::I32Or)
            .op(Op::I32Store(mxcsr))
            .op(Op::End)
            .op(Op::End)
            .op(Op::LocalGet(result));
    }

    fn float_f64(&mut self, bits: u32) {
        self.op(Op::LocalGet(bits)).op(Op::F64ReinterpretI64);
    }

    fn reject_non_normal_f32(&mut self, bits: u32) {
        const MIN: i64 = (f32::MIN_POSITIVE as f64).to_bits() as i64;
        const MAX: i64 = (f32::MAX as f64).to_bits() as i64;
        self.op(Op::LocalGet(bits))
            .op(Op::I64Const(i64::MAX))
            .op(Op::I64And)
            .op(Op::LocalTee(ADDRESS))
            .op(Op::I64Const(MIN))
            .op(Op::I64Sub)
            .op(Op::I64Const(MAX - MIN))
            .op(Op::I64GtU)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I64Eqz)
            .op(Op::I32Eqz)
            .op(Op::I32And)
            .op(Op::BrIf(0));
    }

    pub(super) fn mxcsr(&mut self, address: Address, store: bool, next_pc: u64, exit_depth: u32) {
        let helper = if store {
            self.store_mxcsr.expect("MXCSR store import")
        } else {
            self.load_mxcsr.expect("MXCSR load import")
        };
        self.address(address, next_pc);
        self.op(Op::I32Const(address.segment as i32))
            .op(Op::Call(helper));
        if store {
            self.op(Op::LocalTee(STORE_STATUS))
                .op(Op::I32Eqz)
                .op(Op::If(BlockType::Empty));
            self.vector_failure(exit_depth + 1);
            self.op(Op::End);
            self.stores = true;
        } else {
            self.op(Op::LocalTee(REASON))
                .op(Op::I32Eqz)
                .op(Op::I32Eqz)
                .op(Op::BrIf(exit_depth));
        }
    }
}
