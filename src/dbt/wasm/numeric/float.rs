use super::{ADDRESS, Emitter, FLAGS, LHS, REASON, RESULT, RHS, STORE_STATUS};
use crate::embedding::layout::RunState;
use crate::x86::float::{Format, Kind};
use crate::x86::instruction::{Address, FloatInstruction, Location, RegisterView, Width};
use std::mem::offset_of;
use wasm_encoder::{BlockType, Instruction as Op, ValType};

const ROUNDING_CONTROL: i32 = 0x6000;
const PRECISION_STATUS: i32 = 1 << 5;
const PRECISION_MASK: i32 = 1 << 12;
pub(super) const MASKED_STICKY_PRECISION: i32 = PRECISION_MASK | PRECISION_STATUS;

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
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            let wide = (self.zero_upper_xmm & (1 << instruction.dst) != 0
                && match instruction.src {
                    Location::Memory(_) => true,
                    Location::Register(src) => self.zero_upper_xmm & (1 << src.bank) != 0,
                })
            .then_some(result);
            #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
            let wide = None;
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            let normal = [
                self.normal_low_f32 & (1 << instruction.dst) != 0,
                matches!(instruction.src, Location::Register(src) if self.normal_low_f32 & (1 << src.bank) != 0),
            ];
            #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
            let normal = [false; 2];
            self.float_f32(instruction.op.kind, left, right, wide, normal);
            self.op(Op::Br(1)).op(Op::End);
            self.float_f32_fallback(instruction, left, right, exit_depth);
            self.op(Op::End);
        } else {
            self.float_helper(instruction, left, right, exit_depth);
        }
        self.op(Op::LocalSet(result));
        if instruction.writes_flags() {
            self.op(Op::LocalGet(FLAGS))
                .op(Op::I64Const(2))
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

    fn float_f32_fallback(
        &mut self,
        instruction: FloatInstruction,
        left: u32,
        right: u32,
        exit_depth: u32,
    ) {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.proved_reads.is_some() {
            let (body, _) = self.structured_loop.expect("proved read loop placement");
            debug_assert!(self.loop_gates_prechecked);
            // The rejected op has published neither its result nor retirement.
            // A header retry skips the guarded copy and reaches the checked loop.
            if self.float_outline_params != 0 {
                let continuation = if self.current_index == body.head {
                    super::float_loop::HEADER_REJECTION
                } else {
                    self.current_index as i32
                };
                self.op(Op::I32Const(continuation))
                    .op(Op::LocalSet(super::ENTRY))
                    .op(Op::Br(2));
            } else if self.current_index == body.head {
                self.op(Op::Br(2));
            } else {
                let (_, retry) = self.proved_read_retry.expect("proved read retry local");
                self.op(Op::I32Const(1)).op(Op::LocalSet(retry));
                self.op(Op::I32Const(self.current_index as i32))
                    .op(Op::LocalSet(super::ENTRY))
                    .op(Op::Br(exit_depth));
            }
            return;
        }
        self.float_helper(instruction, left, right, exit_depth + 1);
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

    fn float_f32(
        &mut self,
        kind: Kind,
        left: u32,
        right: u32,
        wide: Option<u32>,
        normal: [bool; 2],
    ) {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.float_raw_sticky {
            self.float_f32_sticky(kind, left, right, wide, normal);
            return;
        }
        let mxcsr = super::vector::memory(offset_of!(RunState, mxcsr));
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let rounding_prechecked = self.proved_reads.is_some();
        #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
        let rounding_prechecked = false;
        if !rounding_prechecked {
            self.load_float_mxcsr();
            self.op(Op::LocalTee(STORE_STATUS));
            self.reject_non_nearest_fp32();
        }

        for ((vector, bits), known_normal) in [(left, LHS), (right, RHS)].into_iter().zip(normal) {
            self.op(Op::LocalGet(vector));
            if let Some(wide) = wide {
                self.op(Op::F64x2PromoteLowF32x4)
                    .op(Op::LocalTee(wide))
                    .op(Op::F64x2ExtractLane(0));
            } else {
                self.op(Op::F32x4ExtractLane(0)).op(Op::F64PromoteF32);
            }
            self.op(Op::I64ReinterpretF64).op(Op::LocalSet(bits));
            if !known_normal {
                self.reject_non_normal_f32(bits);
            }
            if let Some(wide) = wide {
                // Retain the widened operand beneath the next operand's check.
                self.op(Op::LocalGet(wide));
            }
        }

        if let Some(wide) = wide {
            // Keep the rounded destination independent of the exactness
            // calculation. Both vector operands have proved zero upper lanes.
            self.op(Op::LocalGet(left))
                .op(Op::LocalGet(right))
                .op(if kind == Kind::Add {
                    Op::F32x4Add
                } else {
                    Op::F32x4Mul
                })
                .op(Op::LocalSet(wide));
            self.op(if kind == Kind::Add {
                Op::F64x2Add
            } else {
                Op::F64x2Mul
            })
            .op(Op::F64x2ExtractLane(0));
        } else {
            self.float_f64(LHS);
            self.float_f64(RHS);
            self.op(if kind == Kind::Add {
                Op::F64Add
            } else {
                Op::F64Mul
            });
        }
        self.op(Op::I64ReinterpretF64).op(Op::LocalSet(RESULT));
        // Check before rounding so a tiny product that rounds to a normal f32 still falls back.
        self.reject_non_normal_f32(RESULT);

        if !rounding_prechecked {
            self.if_precision_unsettled();
        }
        // A normal f64 result is representable in f32 exactly when the
        // fractional bits below the f32 significand are all zero.
        self.op(Op::LocalGet(RESULT))
            .op(Op::I64Const(
                (1 << (f64::MANTISSA_DIGITS - f32::MANTISSA_DIGITS)) - 1,
            ))
            .op(Op::I64And)
            .op(Op::I64Eqz)
            .op(Op::I32Eqz);
        if kind == Kind::Add {
            // A widened sum that loses a nonzero addend can look exact
            // only when it has the larger addend's bit pattern.
            self.op(Op::LocalGet(RESULT))
                .op(Op::LocalGet(LHS))
                .op(Op::I64Eq)
                .op(Op::LocalGet(RESULT))
                .op(Op::LocalGet(RHS))
                .op(Op::I64Eq)
                .op(Op::I32Or);
            for summand in [LHS, RHS] {
                self.op(Op::LocalGet(summand))
                    .op(Op::I64Const(1))
                    .op(Op::I64Shl)
                    .op(Op::I64Eqz)
                    .op(Op::I32Eqz)
                    .op(Op::I32And);
            }
            self.op(Op::I32Or);
        }
        self.op(Op::If(BlockType::Empty));
        if rounding_prechecked {
            // The proved body keeps nearest rounding. An exact result never
            // consults the precision mask or needs to publish a sticky bit.
            self.load_float_mxcsr();
            self.op(Op::LocalSet(STORE_STATUS));
            self.if_precision_unsettled();
        }
        self.op(Op::LocalGet(STORE_STATUS))
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
            .op(Op::End);
        if let Some(wide) = wide {
            self.op(Op::LocalGet(wide));
        } else {
            self.op(Op::LocalGet(left));
            self.float_f64(RESULT);
            self.op(Op::F32DemoteF64).op(Op::F32x4ReplaceLane(0));
        }
    }

    fn if_precision_unsettled(&mut self) {
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Const(MASKED_STICKY_PRECISION))
            .op(Op::I32And)
            .op(Op::I32Const(MASKED_STICKY_PRECISION))
            .op(Op::I32Ne)
            .op(Op::If(BlockType::Empty));
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn float_f32_sticky(
        &mut self,
        kind: Kind,
        left: u32,
        right: u32,
        wide: Option<u32>,
        normal: [bool; 2],
    ) {
        for (vector, known_normal) in [left, right].into_iter().zip(normal) {
            if !known_normal {
                // Signed zero is admissible. Nonfinite operands cannot
                // produce an interior result and are rejected below.
                self.op(Op::LocalGet(vector))
                    .op(Op::I32x4ExtractLane(0))
                    .op(Op::I32Const(0x7fff_ffff))
                    .op(Op::I32And)
                    .op(Op::I32Const(1))
                    .op(Op::I32Sub)
                    .op(Op::I32Const(0x007f_ffff))
                    .op(Op::I32LtU)
                    .unlikely(Op::BrIf(0));
            }
        }
        if let Some(result) = wide {
            self.op(Op::LocalGet(left))
                .op(Op::LocalGet(right))
                .op(if kind == Kind::Add {
                    Op::F32x4Add
                } else {
                    Op::F32x4Mul
                })
                .op(Op::LocalTee(result))
                .op(Op::I32x4ExtractLane(0));
            self.reject_fp32_noninterior();
            self.op(Op::LocalGet(result));
        } else {
            self.op(Op::LocalGet(left))
                .op(Op::LocalGet(left))
                .op(Op::F32x4ExtractLane(0))
                .op(Op::LocalGet(right))
                .op(Op::F32x4ExtractLane(0))
                .op(if kind == Kind::Add {
                    Op::F32Add
                } else {
                    Op::F32Mul
                })
                .op(Op::I32ReinterpretF32)
                .op(Op::LocalTee(STORE_STATUS));
            self.reject_fp32_noninterior();
            self.op(Op::LocalGet(STORE_STATUS))
                .op(Op::I32x4ReplaceLane(0));
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn reject_fp32_noninterior(&mut self) {
        // An interior rounded result cannot have crossed either exponent
        // boundary. Precision is already masked and sticky in this outline.
        self.op(Op::I32Const(23))
            .op(Op::I32ShrU)
            .op(Op::I32Const(255))
            .op(Op::I32And)
            .op(Op::I32Const(2))
            .op(Op::I32Sub)
            .op(Op::I32Const(251))
            .op(Op::I32GtU)
            .unlikely(Op::BrIf(0));
    }

    pub(super) fn load_float_mxcsr(&mut self) {
        self.op(Op::LocalGet(0))
            .op(Op::I32Load(super::vector::memory(offset_of!(
                RunState, mxcsr
            ))));
    }

    pub(super) fn reject_non_nearest_fp32(&mut self) {
        self.op(Op::I32Const(ROUNDING_CONTROL))
            .op(Op::I32And)
            .unlikely(Op::BrIf(0));
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
            .unlikely(Op::BrIf(0));
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
