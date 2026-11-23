use crate::dbt::wasm::PRIVATE_MEMORY;
use crate::dbt::wasm::{
    ADDRESS, Emitter, RIP, STORE_STATUS, StructuredLoop, memory::read_loop::ReadLoop,
};
use crate::x86::{
    float::{Format, Kind},
    instruction::{Address, Instruction, Location, Operation, VectorInstruction, VectorOperand},
};
use wasm_encoder::{BlockType, Instruction as Op, MemArg, ValType};

// The initial proof produces 0 or 1. A completed lap adds pending XMM outputs.
const DYADIC_DISABLED: i32 = 0;
const DYADIC_PENDING: i32 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Incoming,
    Loaded(u8),
    Product(u8),
    Prefix(u8),
}

#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct DyadicReduction {
    inputs: [(u32, Address); 2],
    accumulator: u8,
    outputs: [u8; 4],
    first_local: u32,
}

impl DyadicReduction {
    pub(in crate::dbt::wasm) const LOCALS: [(u32, ValType); 2] =
        [(4, ValType::V128), (1, ValType::I32)];

    pub(in crate::dbt::wasm) fn recognize(
        proof: ReadLoop,
        body: StructuredLoop,
        instructions: &[Instruction],
        first_local: u32,
    ) -> Option<Self> {
        if body.forward_edges() {
            return None;
        }
        let [terms @ .., _, _, _] = &instructions[body.head as usize..=body.tail as usize] else {
            return None;
        };
        let mut inputs = [None; 2];
        let mut banks = [Lane::Incoming; 16];
        let mut accumulator = None;
        let mut next = 0;
        let mut loaded = 0u8;
        for instruction in terms {
            match instruction.operation {
                Operation::Nop => (),
                Operation::Vector(VectorInstruction::Move {
                    register,
                    operand: VectorOperand::Memory(address),
                    bytes: 4,
                    preserve_upper: false,
                    aligned: false,
                    store: false,
                }) => {
                    let lane = Self::input_lane(proof, &mut inputs[0], address)?;
                    if loaded & (1 << lane) != 0 {
                        return None;
                    }
                    loaded |= 1 << lane;
                    banks[register as usize] = Lane::Loaded(lane);
                }
                Operation::Float(op) if op.op.scalar && op.op.format == Format::F32 => {
                    let dst = op.dst as usize;
                    match (op.op.kind, op.src) {
                        (Kind::Mul, Location::Memory(address)) => {
                            let lane = Self::input_lane(proof, &mut inputs[1], address)?;
                            if banks[dst] != Lane::Loaded(lane) {
                                return None;
                            }
                            banks[dst] = Lane::Product(lane);
                        }
                        (Kind::Add, Location::Register(src))
                            if banks[dst] == Lane::Product(next) =>
                        {
                            let before = banks[src.bank as usize];
                            if next == 0 && before == Lane::Incoming {
                                accumulator = Some(src.bank);
                            } else if next == 0 || before != Lane::Prefix(next - 1) {
                                return None;
                            }
                            banks[dst] = Lane::Prefix(next);
                            next += 1;
                        }
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
        let accumulator = accumulator?;
        if next != 4 || banks[accumulator as usize] != Lane::Prefix(3) {
            return None;
        }
        let mut outputs = [None; 4];
        for (bank, value) in banks.into_iter().enumerate() {
            match value {
                Lane::Incoming => (),
                Lane::Prefix(lane) => outputs[lane as usize] = Some(bank as u8),
                _ => return None,
            }
        }
        Some(Self {
            inputs: [inputs[0]?, inputs[1]?],
            accumulator,
            outputs: [outputs[0]?, outputs[1]?, outputs[2]?, outputs[3]?],
            first_local,
        })
    }

    fn input_lane(
        proof: ReadLoop,
        stream: &mut Option<(u32, Address)>,
        addr: Address,
    ) -> Option<u8> {
        let (slot, index, displacement) = proof.vector_slot(addr)?;
        if *stream.get_or_insert((slot, index)) != (slot, index)
            || displacement > 12
            || displacement % 4 != 0
        {
            return None;
        }
        Some((displacement / 4) as u8)
    }

    fn pending(self) -> u32 {
        self.first_local + 3
    }

    fn state(self) -> u32 {
        self.first_local + 4
    }
}

impl Emitter {
    pub(in crate::dbt::wasm) fn admit_dyadic_accumulator(&mut self, reduction: DyadicReduction) {
        let (proof, _) = self
            .paged_proved_f32_reads
            .or(self.proved_reads)
            .expect("dyadic read proof");
        // Inputs are multiples of 1/16. Keep every product and partial sum
        // on the 1/256 grid within binary32's exact integer range.
        const MAX_COEFFICIENT: i32 = 1 << 24;
        const LAP_COEFFICIENT: i32 = 4 * 128 * 128;
        let accumulator = self.xmm(reduction.accumulator);
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0));
        if self.paged_proved_f32_reads.is_some() {
            self.load_float_mxcsr();
            self.reject_non_nearest_fp32();
        }
        self.proved_read_laps(proof);
        self.op(Op::LocalTee(ADDRESS))
            .op(Op::I64Const(i64::from(MAX_COEFFICIENT / LAP_COEFFICIENT)))
            .op(Op::I64GtU)
            .unlikely(Op::BrIf(0))
            .op(Op::Drop);
        self.op(Op::LocalGet(accumulator))
            .op(Op::F32x4ExtractLane(0))
            .op(Op::F32Abs)
            .op(Op::F32Const(256.0f32.into()))
            .op(Op::F32Mul)
            .op(Op::I32ReinterpretF32)
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::F32ReinterpretI32)
            .op(Op::F32Nearest)
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::F32ReinterpretI32)
            .op(Op::F32Eq)
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::F32ReinterpretI32)
            .op(Op::I32Const(MAX_COEFFICIENT))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Const(LAP_COEFFICIENT))
            .op(Op::I32Mul)
            .op(Op::I32Sub)
            .op(Op::F32ConvertI32U)
            .op(Op::F32Le)
            .op(Op::I32And)
            .op(Op::End)
            .op(Op::LocalSet(reduction.state()))
            // Keep the live accumulator in the final lane of the pending
            // prefixes. The original XMM bank still owns a first-lap rejection.
            .op(Op::LocalGet(accumulator))
            .op(Op::F32x4ExtractLane(0))
            .op(Op::F32x4Splat)
            .op(Op::LocalSet(reduction.pending()));
    }

    pub(in crate::dbt::wasm) fn try_dyadic_prefix(
        &mut self,
        reduction: DyadicReduction,
        paged_prefix: Option<(u32, u64)>,
    ) {
        let (_, bias) = self
            .paged_proved_f32_reads
            .or(self.proved_reads)
            .expect("dyadic read proof");
        let [left, right, prefixes] = [0, 1, 2].map(|i| reduction.first_local + i);
        self.op(Op::LocalGet(reduction.state()))
            .op(Op::If(BlockType::Empty))
            .op(Op::Block(BlockType::Empty));
        for ((slot, index), input) in reduction.inputs.into_iter().zip([left, right]) {
            self.address(index, 0);
            self.op(Op::I32WrapI64)
                .op(Op::LocalGet(bias + slot))
                .op(Op::I32Add)
                .ram_load(
                    Op::V128Load,
                    MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: PRIVATE_MEMORY,
                    },
                )
                .op(Op::LocalSet(input));
        }
        const fn splat(bits: i32) -> i128 {
            let lane = bits as u32 as u128;
            (lane | (lane << 32) | (lane << 64) | (lane << 96)) as i128
        }
        // Scaling up by a power of two is exact until overflow. The range
        // test rejects overflow and nonfinite values. Nonzero subnormals
        // remain below one after scaling and fail the integrality test.
        for input in [left, right] {
            self.op(Op::LocalGet(input))
                .op(Op::F32x4Abs)
                .op(Op::V128Const(splat(16.0f32.to_bits() as i32)))
                .op(Op::F32x4Mul)
                .op(Op::LocalTee(prefixes))
                .op(Op::F32x4Nearest)
                .op(Op::LocalGet(prefixes))
                .op(Op::F32x4Eq)
                .op(Op::LocalGet(prefixes))
                .op(Op::V128Const(splat(128.0f32.to_bits() as i32)))
                .op(Op::F32x4Le)
                .op(Op::V128And);
        }
        self.op(Op::V128And)
            .op(Op::I32x4AllTrue)
            .op(Op::I32Eqz)
            .unlikely(Op::BrIf(0))
            .op(Op::LocalGet(left))
            .op(Op::LocalGet(right))
            .op(Op::F32x4Mul)
            .op(Op::LocalSet(prefixes));
        // Negative zero preserves the sign of every all-negative-zero prefix.
        for shift in [4, 8] {
            self.op(Op::LocalGet(prefixes))
                .op(Op::LocalGet(prefixes))
                .op(Op::V128Const(splat(i32::MIN)))
                .op(Op::I8x16Shuffle(std::array::from_fn(|byte| {
                    if byte < shift {
                        (byte + 16) as u8
                    } else {
                        (byte - shift) as u8
                    }
                })))
                .op(Op::F32x4Add)
                .op(Op::LocalSet(prefixes));
        }
        self.op(Op::LocalGet(reduction.pending()))
            .op(Op::F32x4ExtractLane(3))
            .op(Op::F32x4Splat)
            .op(Op::LocalGet(prefixes))
            .op(Op::F32x4Add)
            .op(Op::LocalSet(reduction.pending()));
        if let Some((retired, next_pc)) = paged_prefix {
            // The integer latch still retires normally. Its exit or a partial
            // following lap can immediately observe these XMM banks.
            self.publish_dyadic_prefixes(reduction);
            self.op(Op::I64Const(next_pc as i64)).op(Op::LocalSet(RIP));
            self.retire_instructions(retired);
        } else {
            self.op(Op::I32Const(DYADIC_PENDING))
                .op(Op::LocalSet(reduction.state()));
        }
        self.op(Op::Br(2)).op(Op::End);
        // The existing lap can expose its incoming banks through checked replay.
        if paged_prefix.is_none() {
            self.flush_dyadic_prefixes(reduction);
        }
        self.op(Op::I32Const(DYADIC_DISABLED))
            .op(Op::LocalSet(reduction.state()))
            .op(Op::End);
    }

    pub(in crate::dbt::wasm) fn flush_dyadic_prefixes(&mut self, reduction: DyadicReduction) {
        self.op(Op::LocalGet(reduction.state()))
            .op(Op::I32Const(DYADIC_PENDING))
            .op(Op::I32Eq)
            .op(Op::If(BlockType::Empty));
        self.publish_dyadic_prefixes(reduction);
        self.op(Op::End);
    }

    fn publish_dyadic_prefixes(&mut self, reduction: DyadicReduction) {
        for (lane, bank) in reduction.outputs.into_iter().enumerate() {
            let destination = self.xmm(bank);
            self.op(Op::V128Const(0))
                .op(Op::LocalGet(reduction.pending()))
                .op(Op::I32x4ExtractLane(lane as u8))
                .op(Op::I32x4ReplaceLane(0))
                .op(Op::LocalSet(destination));
        }
    }
}
