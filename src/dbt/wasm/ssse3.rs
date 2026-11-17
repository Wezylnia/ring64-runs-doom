use super::Emitter;
use crate::x86::instruction::Ssse3Op;
use wasm_encoder::Instruction as Op;

impl Emitter {
    /// Leaves a v128 result. The MMX caller consumes only its low eight bytes.
    pub(super) fn ssse3(&mut self, op: Ssse3Op, immediate: u8, bytes: u8, left: u32, right: u32) {
        use Ssse3Op::*;
        match op {
            Pshufb => {
                let mask = 0x80 | (bytes - 1);
                self.op(Op::LocalGet(left))
                    .op(Op::LocalGet(right))
                    .op(Op::V128Const(i128::from_le_bytes([mask; 16])))
                    .op(Op::V128And)
                    .op(Op::I8x16Swizzle);
            }
            Phaddw | Phaddd | Phaddsw | Phsubw | Phsubd | Phsubsw => {
                let (lane_bytes, arithmetic) = match op {
                    Phaddw => (2, Op::I16x8Add),
                    Phaddd => (4, Op::I32x4Add),
                    Phaddsw => (2, Op::I16x8AddSatS),
                    Phsubw => (2, Op::I16x8Sub),
                    Phsubd => (4, Op::I32x4Sub),
                    Phsubsw => (2, Op::I16x8SubSatS),
                    _ => unreachable!(),
                };
                for parity in [0, 1] {
                    let lanes = horizontal_lanes(bytes, lane_bytes, parity);
                    self.op(Op::LocalGet(left))
                        .op(Op::LocalGet(right))
                        .op(Op::I8x16Shuffle(lanes));
                }
                self.op(arithmetic);
            }
            Pmaddubsw => {
                self.op(Op::LocalGet(left))
                    .op(Op::I16x8ExtendLowI8x16U)
                    .op(Op::LocalGet(right))
                    .op(Op::I16x8ExtendLowI8x16S)
                    .op(Op::I32x4DotI16x8S);
                if bytes == 16 {
                    self.op(Op::LocalGet(left))
                        .op(Op::I16x8ExtendHighI8x16U)
                        .op(Op::LocalGet(right))
                        .op(Op::I16x8ExtendHighI8x16S)
                        .op(Op::I32x4DotI16x8S);
                } else {
                    self.op(Op::V128Const(0));
                }
                self.op(Op::I16x8NarrowI32x4S);
            }
            Psignb | Psignw | Psignd => {
                let (gt, lt, neg) = match op {
                    Psignb => (Op::I8x16GtS, Op::I8x16LtS, Op::I8x16Neg),
                    Psignw => (Op::I16x8GtS, Op::I16x8LtS, Op::I16x8Neg),
                    Psignd => (Op::I32x4GtS, Op::I32x4LtS, Op::I32x4Neg),
                    _ => unreachable!(),
                };
                self.op(Op::LocalGet(left))
                    .op(Op::LocalGet(right))
                    .op(Op::V128Const(0))
                    .op(gt)
                    .op(Op::V128And)
                    .op(Op::LocalGet(left))
                    .op(neg)
                    .op(Op::LocalGet(right))
                    .op(Op::V128Const(0))
                    .op(lt)
                    .op(Op::V128And)
                    .op(Op::V128Or);
            }
            Pmulhrsw => {
                // Q15 saturates MIN * MIN to 0x7fff. x86 keeps the low word, 0x8000.
                self.op(Op::LocalGet(left))
                    .op(Op::LocalGet(right))
                    .op(Op::I16x8Q15MulrSatS)
                    .op(Op::LocalGet(left))
                    .op(Op::LocalGet(right))
                    .op(Op::I16x8MaxS)
                    .op(Op::I32Const(i32::from(i16::MIN)))
                    .op(Op::I16x8Splat)
                    .op(Op::I16x8Eq)
                    .op(Op::V128Xor);
            }
            Pabsb | Pabsw | Pabsd => {
                self.op(Op::LocalGet(right)).op(match op {
                    Pabsb => Op::I8x16Abs,
                    Pabsw => Op::I16x8Abs,
                    Pabsd => Op::I32x4Abs,
                    _ => unreachable!(),
                });
            }
            Palignr => {
                if immediate >= 2 * bytes {
                    self.op(Op::V128Const(0));
                    return;
                }
                let start = if immediate < bytes {
                    self.op(Op::LocalGet(right)).op(Op::LocalGet(left));
                    immediate
                } else {
                    self.op(Op::LocalGet(left)).op(Op::V128Const(0));
                    immediate - bytes
                };
                self.op(Op::I8x16Shuffle(std::array::from_fn(|i| {
                    let offset = start + i as u8;
                    if offset < bytes {
                        offset
                    } else {
                        16 + offset - bytes
                    }
                })));
            }
        }
    }
}

fn horizontal_lanes(bytes: u8, lane_bytes: u8, parity: u8) -> [u8; 16] {
    std::array::from_fn(|i| {
        let byte = i as u8 % bytes;
        let half = bytes / 2;
        let input = if byte < half { 0 } else { 16 };
        let within_half = byte % half;
        let lane = 2 * (within_half / lane_bytes) + parity;
        input + lane * lane_bytes + within_half % lane_bytes
    })
}
