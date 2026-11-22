use crate::x86::{
    context::ExecutionContext,
    float::{Format, Kind},
    instruction::{
        Address, AddressBase, Alu, Condition, DataOperand, Instruction, Location, Operation, Unary,
        VectorInstruction, VectorOperand, Width,
    },
};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(super) struct IndexedReadWindow {
    pub(super) address: Address,
    pub(super) span: super::memory::SpanCache,
    pub(super) host: u32,
    pub(super) limit: u32,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
impl IndexedReadWindow {
    pub(super) fn recognize(context: &ExecutionContext, body: &[Instruction]) -> Option<Address> {
        use crate::{
            memory::Access,
            x86::context::{Mode, Segment},
        };
        let [loads @ .., counter, branch] = body else {
            return None;
        };
        if !matches!(branch.operation, Operation::Jcc {
            condition: Condition::NotEqual, target
        } if target == body.first()?.pc)
        {
            return None;
        }
        let Operation::Alu {
            kind: Alu::Add | Alu::Sub,
            dst: counter,
            src: DataOperand::Immediate(_),
            ..
        } = counter.operation
        else {
            return None;
        };
        let first = loads.first()?.operation;
        let (normalize, loads) = if matches!(first, Operation::Mov { .. }) {
            (Some(first), &loads[1..])
        } else {
            (None, loads)
        };
        let Operation::Load {
            dst,
            address,
            width: Width::W32,
        } = loads.first()?.operation
        else {
            return None;
        };
        let AddressBase::Register(base) = address.base else {
            return None;
        };
        let index = address.index?;
        if dst.high_byte
            || index.register != dst.bank
            || base == index.register
            || counter.bank == base
            || counter.bank == index.register
            || address.segment != Segment::Ds
            || !(context.is_long64() && address.size == Width::W64
                || context.mode() == Mode::Protected
                    && context.code32()
                    && address.size == Width::W32)
            || context
                .translate(0, 4, address.segment, Access::Read, false)
                .ok()
                != Some(0)
        {
            return None;
        }
        let load = Operation::Load {
            dst,
            address,
            width: Width::W32,
        };
        let self_mov = Operation::Mov {
            dst,
            src: DataOperand::Register(dst),
            width: Width::W32,
        };
        (normalize.is_none_or(|operation| operation == self_mov)
            && loads.len() >= 4
            && loads
                .iter()
                .all(|instruction| instruction.operation == load))
        .then_some(address)
    }
}

#[derive(Clone, Copy)]
struct Stream {
    first: Address,
    last_displacement: i64,
}

impl Stream {
    fn matches(self, address: Address) -> bool {
        Address {
            displacement: address.displacement,
            ..self.first
        } == address
    }

    fn displacement_width(self, bytes: u32) -> Option<u32> {
        let distance = self
            .last_displacement
            .checked_sub(self.first.displacement)?;
        u32::try_from(distance).ok()?.checked_add(bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadLimit {
    Register(u8),
    Immediate(u64),
}

#[derive(Clone, Copy)]
pub(super) struct ReadLoop {
    index: u8,
    end: ReadLimit,
    count_width: Width,
    step: u64,
    streams: [Option<Stream>; 2],
    byte_reads: bool,
}

impl ReadLoop {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) const BIAS_LOCALS: u32 = 2;

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn outline_registers(self) -> Option<(u16, u8, usize)> {
        let ReadLimit::Register(end) = self.end else {
            return None;
        };
        if self.byte_reads {
            return None;
        }
        let inputs = (1 << self.index) | (1 << end);
        let biases = self.streams.into_iter().flatten().count();
        Some((inputs, self.index, biases))
    }

    pub(super) fn recognize(
        context: &ExecutionContext,
        instructions: &[Instruction],
    ) -> Option<Self> {
        Self::recognize_body(context, instructions, false)
    }

    pub(super) fn recognize_integer(
        context: &ExecutionContext,
        instructions: &[Instruction],
    ) -> Option<Self> {
        Self::recognize_body(context, instructions, true)
    }

    #[cfg(all(not(feature = "parallel"), any(test, target_arch = "wasm32")))]
    pub(super) fn recognize_paged_f32(
        context: &ExecutionContext,
        instructions: &[Instruction],
    ) -> Option<Self> {
        use crate::x86::context::Segment;

        let address_size = if context.is_long64() {
            Width::W64
        } else if context.flat_paged_data32() {
            Width::W32
        } else {
            return None;
        };
        let proof = Self::recognize(context, instructions)?;
        let [body @ .., _, _, _] = instructions else {
            return None;
        };
        proof.streams[1]?;
        for instruction in body {
            if let Some(address) = Self::read_operand(instruction.operation, false)?
                && (address.segment != Segment::Ds
                    || address.size != address_size
                    || address.index?.scale != 4
                    || address.displacement % 4 != 0)
            {
                return None;
            }
        }
        Some(proof)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn one_stream(self) -> bool {
        self.streams[1].is_none()
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn last_memory_stream(self, instructions: &[Instruction]) -> Option<u32> {
        instructions.iter().rev().find_map(|instruction| {
            let address = Self::read_operand(instruction.operation, self.byte_reads)??;
            self.stream_for_address(address)
        })
    }

    fn recognize_body(
        context: &ExecutionContext,
        instructions: &[Instruction],
        byte_reads: bool,
    ) -> Option<Self> {
        let [body @ .., update, compare, branch] = instructions else {
            return None;
        };
        if instructions
            .windows(2)
            .any(|pair| pair[0].next_pc != pair[1].pc)
        {
            return None;
        }
        let Operation::Jcc {
            condition: Condition::NotEqual,
            target,
        } = branch.operation
        else {
            return None;
        };
        if target != body.first()?.pc {
            return None;
        }
        let (index, step, count_width, minimum_reads) = match update.operation {
            Operation::Alu {
                kind: Alu::Add,
                dst,
                src: DataOperand::Immediate(step),
                width: count_width @ (Width::W32 | Width::W64),
            } => (dst, step, count_width, 4),
            Operation::Unary {
                kind: Unary::Inc,
                dst: Location::Register(dst),
                width: count_width @ (Width::W32 | Width::W64),
            } if !byte_reads => (dst, 1, count_width, 2),
            _ => return None,
        };
        // A power-of-two step lets the guard prove the end is reached by the backedge.
        if index.high_byte || !step.is_power_of_two() || step > u64::from(u32::MAX) {
            return None;
        }
        let end = match compare.operation {
            Operation::Alu {
                kind: Alu::Cmp,
                dst: left,
                src: DataOperand::Register(right),
                width,
            } if width == count_width && !left.high_byte && !right.high_byte && left != right => {
                if left == index {
                    ReadLimit::Register(right.bank)
                } else if right == index {
                    ReadLimit::Register(left.bank)
                } else {
                    return None;
                }
            }
            Operation::Alu {
                kind: Alu::Cmp,
                dst,
                src: DataOperand::Immediate(end),
                width,
            } if byte_reads
                && dst == index
                && (width == count_width || (count_width == Width::W64 && width == Width::W32)) =>
            {
                // The guard compares the full-width start against this bound.
                // A W32 compare on a W64 counter cannot reach a wrapped value.
                ReadLimit::Immediate(end & width.mask())
            }
            _ => return None,
        };
        let written = if byte_reads {
            body.iter().try_fold(0u16, |written, instruction| {
                Some(written | Self::integer_writes(instruction.operation)?)
            })?
        } else {
            0
        };
        if written & (1 << index.bank) != 0
            || matches!(end, ReadLimit::Register(bank) if written & (1 << bank) != 0)
        {
            return None;
        }
        let mut proof = Self {
            index: index.bank,
            end,
            count_width,
            step,
            streams: [None; 2],
            byte_reads,
        };
        let bytes = if byte_reads { 1 } else { 4 };
        let mut reads = 0;
        for instruction in body {
            let Some(address) = Self::read_operand(instruction.operation, byte_reads)? else {
                continue;
            };
            let index = address.index?;
            // All other registers are stable in this restricted body. Address-sized
            // truncation of the induction register may not depend on a partial write.
            if index.register != proof.index
                || address.size.bits() > count_width.bits()
                || !matches!(address.size, Width::W32 | Width::W64)
                || address.base == AddressBase::Register(proof.index)
                || matches!(address.base, AddressBase::Register(bank) if written & (1 << bank) != 0)
                || address.base == AddressBase::NextPc
                || context.segment_base(address.segment) != 0
            {
                return None;
            }
            if let Some(stream) = proof
                .streams
                .iter_mut()
                .flatten()
                .find(|stream| stream.matches(address))
            {
                stream.first.displacement = stream.first.displacement.min(address.displacement);
                stream.last_displacement = stream.last_displacement.max(address.displacement);
                stream.displacement_width(bytes)?;
            } else {
                let next = proof.streams.iter_mut().find(|stream| stream.is_none())?;
                *next = Some(Stream {
                    first: address,
                    last_displacement: address.displacement,
                });
            }
            reads += 1;
        }
        // A guarded copy pays for two paths in the artifact. Require a body with
        // repeated reads that can actually share its guard.
        (reads >= minimum_reads).then_some(proof)
    }

    fn integer_writes(operation: Operation) -> Option<u16> {
        match operation {
            Operation::Alu { kind, dst, .. } => Some(if kind.writes_result() {
                1 << dst.bank
            } else {
                0
            }),
            Operation::Load {
                dst,
                width: Width::W8,
                ..
            }
            | Operation::Extend {
                dst,
                source_width: Width::W8,
                ..
            }
            | Operation::Mov { dst, .. }
            | Operation::Lea { dst, .. } => Some(1 << dst.bank),
            Operation::Nop => Some(0),
            _ => None,
        }
    }

    fn read_operand(operation: Operation, byte_reads: bool) -> Option<Option<Address>> {
        if byte_reads {
            return Some(match operation {
                Operation::Load {
                    address,
                    width: Width::W8,
                    ..
                }
                | Operation::Extend {
                    src: Location::Memory(address),
                    source_width: Width::W8,
                    ..
                } => Some(address),
                _ => None,
            });
        }
        match operation {
            Operation::Nop => Some(None),
            Operation::Vector(VectorInstruction::Move {
                operand,
                bytes: 4,
                aligned: false,
                store: false,
                ..
            }) => Some(match operand {
                VectorOperand::Memory(address) => Some(address),
                VectorOperand::Register(_) => None,
            }),
            Operation::Float(instruction)
                if instruction.op.scalar
                    && instruction.op.format == Format::F32
                    && matches!(instruction.op.kind, Kind::Add | Kind::Mul) =>
            {
                Some(match instruction.src {
                    Location::Memory(address) => Some(address),
                    Location::Register(_) => None,
                })
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn stream_for(self, operation: Operation) -> Option<u32> {
        let address = Self::read_operand(operation, self.byte_reads)??;
        self.stream_for_address(address)
    }

    #[cfg(any(test, all(not(feature = "parallel"), target_arch = "wasm32")))]
    pub(super) fn byte_slot(self, address: Address) -> Option<(u32, Address, u64)> {
        if !self.byte_reads {
            return None;
        }
        self.slot(address)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn vector_slot(self, address: Address) -> Option<(u32, Address, u64)> {
        if self.byte_reads {
            return None;
        }
        let (slot, first, offset) = self.slot(address)?;
        Some((
            slot,
            Address {
                base: AddressBase::None,
                displacement: 0,
                ..first
            },
            offset,
        ))
    }

    #[cfg(any(test, all(not(feature = "parallel"), target_arch = "wasm32")))]
    fn slot(self, address: Address) -> Option<(u32, Address, u64)> {
        let slot = self.stream_for_address(address)?;
        let stream = self.streams[slot as usize]?;
        if address.displacement > stream.last_displacement {
            return None;
        }
        let offset = address
            .displacement
            .checked_sub(stream.first.displacement)?;
        Some((slot, stream.first, u64::try_from(offset).ok()?))
    }

    #[cfg(any(test, all(not(feature = "parallel"), target_arch = "wasm32")))]
    fn stream_for_address(self, address: Address) -> Option<u32> {
        self.streams
            .iter()
            .position(|stream| stream.is_some_and(|stream| stream.matches(address)))
            .map(|index| index as u32)
    }
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::{ADDRESS, Emitter, LHS, RESULT, RHS, STORE_STATUS, VALUE, memory::SpanCache};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use wasm_encoder::{BlockType, Instruction as Op, MemArg, ValType};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
impl Emitter {
    pub(super) fn proved_read_laps(&mut self, proof: ReadLoop) {
        let ReadLimit::Register(end) = proof.end else {
            unreachable!("outlined Float loop register bound");
        };
        self.register(end, proof.count_width);
        self.register(proof.index, proof.count_width);
        self.op(Op::I64Sub)
            .op(Op::I64Const(i64::from(proof.step.trailing_zeros())))
            .op(Op::I64ShrU);
    }

    pub(super) fn multiple_read_laps(&mut self, proof: ReadLoop) {
        // The existing span proof establishes a positive aligned trip count.
        // Reload both bounds because a checked retry can reuse the span biases.
        let ReadLimit::Register(end) = proof.end else {
            unreachable!("outlined Float loop register bound");
        };
        self.register(end, proof.count_width);
        self.register(proof.index, proof.count_width);
        self.op(Op::I64Sub)
            .op(Op::I64Const(proof.step as i64))
            .op(Op::I64Ne);
    }

    pub(super) fn guard_read_loop(
        &mut self,
        proof: ReadLoop,
        span: SpanCache,
        first_bias: u32,
        guard_failed: u32,
        retry: u32,
    ) {
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            // A checked retry reaches this header only through the same
            // induction step, with its proved stream bindings still live.
            .op(Op::I32Const(1))
            .op(Op::LocalGet(retry))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(retry))
            .op(Op::BrIf(0))
            .op(Op::Drop)
            .op(Op::I32Const(0));
        // A later LDMXCSR can change rounding within this artifact. Only a
        // mapping or address proof failure latches the checked placement.
        self.load_float_mxcsr();
        self.reject_non_nearest_fp32();
        self.op(Op::Drop)
            .op(Op::Block(BlockType::Empty))
            .op(Op::LocalGet(guard_failed))
            .op(Op::BrIf(0));
        self.guard_read_span(proof, span.resolve, first_bias);
        self.op(Op::I32Const(1))
            .op(Op::Br(1))
            .op(Op::End)
            // Split or unsuitable mappings use the checked loop until the
            // next activation, without reprobing on each checked backedge.
            .op(Op::I32Const(1))
            .op(Op::LocalSet(guard_failed))
            .op(Op::I32Const(0))
            .op(Op::End);
    }

    pub(super) fn guard_integer_read_loop(
        &mut self,
        proof: ReadLoop,
        resolve: u32,
        first_bias: u32,
    ) {
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0));
        self.guard_read_span(proof, resolve, first_bias);
        self.op(Op::Drop).op(Op::I32Const(1)).op(Op::End);
    }

    fn guard_read_trip_count(&mut self, proof: ReadLoop) {
        match proof.end {
            ReadLimit::Register(end) => self.register(end, proof.count_width),
            ReadLimit::Immediate(end) => {
                self.op(Op::I64Const(end as i64));
            }
        }
        self.op(Op::LocalTee(RHS));
        self.register(proof.index, proof.count_width);
        self.op(Op::LocalTee(LHS))
            .op(Op::I64LeU)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(RHS))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Sub)
            .op(Op::LocalTee(LHS))
            .op(Op::I64Const((proof.step - 1) as i64))
            .op(Op::I64And)
            .op(Op::I64Eqz)
            .op(Op::I32Eqz)
            .op(Op::BrIf(0));
    }

    fn guard_read_stream_length(&mut self, proof: ReadLoop, stream: Stream) {
        let scale = u64::from(stream.first.index.unwrap().scale);
        let bytes = if proof.byte_reads { 1 } else { 4 };
        let displacement_width = u64::from(stream.displacement_width(bytes).unwrap());
        // Length = (end - start - step) * scale + displacement width.
        let max_count = (u64::from(u32::MAX) - displacement_width) / scale + proof.step;
        self.op(Op::LocalGet(LHS))
            .op(Op::I64Const(max_count as i64))
            .op(Op::I64GtU)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Const(proof.step as i64))
            .op(Op::I64Sub)
            .op(Op::I64Const(scale as i64))
            .op(Op::I64Mul)
            .op(Op::I64Const(displacement_width as i64))
            .op(Op::I64Add)
            .op(Op::LocalSet(VALUE));
    }

    pub(super) fn guard_paged_f32_read_loop(
        &mut self,
        proof: ReadLoop,
        resolve: u32,
        first_bias: u32,
    ) {
        use crate::{memory::DirectSpan, x86::context::Segment};

        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0));
        self.guard_read_trip_count(proof);
        for (slot, stream) in proof.streams.iter().flatten().copied().enumerate() {
            self.guard_read_stream_length(proof, stream);
            self.address(stream.first, 0);
            // Scale and constant offsets are multiples of four in both streams.
            self.op(Op::LocalTee(ADDRESS))
                .op(Op::I64Const(3))
                .op(Op::I64And)
                .op(Op::I64Eqz)
                .op(Op::I32Eqz)
                .op(Op::BrIf(0))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::LocalGet(VALUE))
                .op(Op::I64Const(1))
                .op(Op::I64Sub)
                .op(Op::I64Add)
                .op(Op::LocalTee(VALUE));
            if stream.first.size == Width::W64 {
                self.op(Op::LocalGet(ADDRESS)).op(Op::I64LtU);
            } else {
                self.op(Op::I64Const(i64::from(u32::MAX))).op(Op::I64GtU);
            }
            self.op(Op::BrIf(0));
            if slot == 0 {
                self.op(Op::LocalGet(ADDRESS))
                    .op(Op::LocalSet(RESULT))
                    .op(Op::LocalGet(VALUE))
                    .op(Op::LocalSet(RHS));
            } else {
                self.op(Op::LocalGet(ADDRESS))
                    .op(Op::LocalGet(RESULT))
                    .op(Op::LocalGet(ADDRESS))
                    .op(Op::LocalGet(RESULT))
                    .op(Op::I64LtU)
                    .op(Op::Select)
                    .op(Op::LocalSet(RESULT))
                    .op(Op::LocalGet(VALUE))
                    .op(Op::LocalGet(RHS))
                    .op(Op::LocalGet(VALUE))
                    .op(Op::LocalGet(RHS))
                    .op(Op::I64GtU)
                    .op(Op::Select)
                    .op(Op::LocalSet(RHS));
            }
        }
        // Resolve the bounding interval once. Both streams then touch the
        // same local translation and backing, with the fetched code still MRU.
        self.op(Op::LocalGet(RHS))
            .op(Op::LocalGet(RESULT))
            .op(Op::I64Sub)
            .op(Op::LocalTee(VALUE))
            .op(Op::I64Const(i64::from(u32::MAX - 1)))
            .op(Op::I64GtU)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(RESULT))
            .op(Op::LocalGet(VALUE))
            .op(Op::I64Const(1))
            .op(Op::I64Add)
            .op(Op::I32WrapI64)
            .op(Op::I32Const(Segment::Ds as i32))
            .op(Op::Call(resolve))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::BrIf(0));
        for (slot, stream) in proof.streams.iter().flatten().enumerate() {
            self.op(Op::LocalGet(STORE_STATUS))
                .op(Op::I32Load(MemArg {
                    offset: std::mem::offset_of!(DirectSpan, host) as u64,
                    align: 2,
                    memory_index: 0,
                }))
                .op(Op::LocalGet(RESULT))
                .op(Op::I32WrapI64)
                .op(Op::I32Sub);
            self.address(
                Address {
                    index: None,
                    ..stream.first
                },
                0,
            );
            self.op(Op::I32WrapI64)
                .op(Op::I32Add)
                .op(Op::LocalSet(first_bias + slot as u32));
        }
        self.op(Op::Drop).op(Op::I32Const(1)).op(Op::End);
    }

    pub(super) fn guard_paged_f32_pair(
        &mut self,
        proof: ReadLoop,
        resolve: u32,
        last: u32,
        first_bias: u32,
    ) {
        let addresses = [RESULT, RHS];
        let order = [1 - last, last];
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0));
        self.guard_read_trip_count(proof);
        for (slot, (stream, address)) in proof.streams.into_iter().zip(addresses).enumerate() {
            let stream = stream.unwrap();
            self.guard_read_stream_length(proof, stream);
            self.address(stream.first, 0);
            self.op(Op::LocalSet(address))
                .op(Op::LocalGet(VALUE))
                .op(Op::I32WrapI64)
                .op(Op::LocalSet(first_bias + slot as u32));
        }
        for slot in order {
            self.op(Op::LocalGet(addresses[slot as usize]))
                .op(Op::LocalGet(first_bias + slot));
        }
        self.op(Op::Call(resolve))
            .op(Op::LocalTee(VALUE))
            .op(Op::I64Eqz)
            .op(Op::BrIf(0));
        for (word, slot) in order.into_iter().enumerate() {
            let stream = proof.streams[slot as usize].unwrap();
            self.op(Op::LocalGet(VALUE));
            if word != 0 {
                self.op(Op::I64Const(32)).op(Op::I64ShrU);
            }
            self.op(Op::I32WrapI64)
                .op(Op::LocalGet(addresses[slot as usize]))
                .op(Op::I32WrapI64)
                .op(Op::I32Sub);
            self.address(
                Address {
                    index: None,
                    ..stream.first
                },
                0,
            );
            self.op(Op::I32WrapI64)
                .op(Op::I32Add)
                .op(Op::LocalSet(first_bias + slot));
        }
        self.op(Op::Drop).op(Op::I32Const(1)).op(Op::End);
    }

    fn guard_read_span(&mut self, proof: ReadLoop, resolve: u32, first_bias: u32) {
        use crate::memory::DirectSpan as Descriptor;

        self.guard_read_trip_count(proof);

        for (slot, stream) in proof.streams.iter().flatten().enumerate() {
            self.guard_read_stream_length(proof, *stream);
            self.address(stream.first, 0);
            self.op(Op::LocalTee(ADDRESS))
                .op(Op::LocalGet(VALUE))
                .op(Op::I64Const(1))
                .op(Op::I64Sub)
                .op(Op::I64Add);
            if stream.first.size == Width::W64 {
                self.op(Op::LocalGet(ADDRESS)).op(Op::I64LtU);
            } else {
                self.op(Op::I64Const(i64::from(u32::MAX))).op(Op::I64GtU);
            }
            self.op(Op::BrIf(0))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::LocalGet(VALUE))
                .op(Op::I32WrapI64)
                .op(Op::I32Const(stream.first.segment as i32))
                .op(Op::Call(resolve))
                .op(Op::LocalTee(STORE_STATUS))
                .op(Op::I32Eqz)
                .op(Op::BrIf(0));

            // The resolver certifies the complete canonical/segmented interval
            // in one readable backing. The i32 bias may wrap, but the final
            // Wasm pointer is the backing address plus an in-bounds byte offset.
            let field = |offset: usize, align| MemArg {
                offset: offset as u64,
                align,
                memory_index: 0,
            };
            self.op(Op::LocalGet(STORE_STATUS))
                .op(Op::I32Load(field(
                    std::mem::offset_of!(Descriptor, host),
                    2,
                )))
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::I64Load(field(
                    std::mem::offset_of!(Descriptor, base),
                    3,
                )))
                .op(Op::I32WrapI64)
                .op(Op::I32Sub);
            if !proof.byte_reads {
                // Fold the invariant guest base and first displacement into
                // the bias. Each vector load adds just its scaled index and offset.
                self.address(
                    Address {
                        index: None,
                        ..stream.first
                    },
                    0,
                );
                self.op(Op::I32WrapI64).op(Op::I32Add);
            }
            self.op(Op::LocalSet(first_bias + slot as u32));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86::{
        context::Segment,
        instruction::{AddressIndex, RegisterView},
    };

    fn read(base: u8, displacement: i64) -> Operation {
        Operation::Vector(VectorInstruction::Move {
            register: 1,
            operand: VectorOperand::Memory(Address {
                base: AddressBase::Register(base),
                index: Some(AddressIndex {
                    register: 1,
                    scale: 4,
                }),
                displacement,
                size: Width::W64,
                segment: Segment::Ds,
            }),
            bytes: 4,
            preserve_upper: false,
            aligned: false,
            store: false,
        })
    }

    fn fixture() -> [Instruction; 8] {
        let index = RegisterView::low(1);
        let end = RegisterView::low(3);
        let head = 0x4000;
        let operations = [
            read(6, 4),
            read(7, 12),
            Operation::Nop,
            read(6, -4),
            read(7, 8),
            Operation::Alu {
                kind: Alu::Add,
                dst: index,
                src: DataOperand::Immediate(2),
                width: Width::W64,
            },
            Operation::Alu {
                kind: Alu::Cmp,
                dst: end,
                src: DataOperand::Register(index),
                width: Width::W64,
            },
            Operation::Jcc {
                condition: Condition::NotEqual,
                target: head,
            },
        ];
        std::array::from_fn(|index| Instruction {
            pc: head + index as u64,
            next_pc: head + index as u64 + 1,
            operation: operations[index],
            #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
            explicit_lock: false,
        })
    }

    #[test]
    fn read_loop_bounds_displacements_per_independent_affine_stream() {
        let instructions = fixture();
        let proof = ReadLoop::recognize(&ExecutionContext::default(), &instructions).unwrap();
        assert_eq!(
            (proof.index, proof.end, proof.count_width, proof.step),
            (1, ReadLimit::Register(3), Width::W64, 2)
        );
        assert_eq!(
            proof
                .streams
                .map(|s| s.map(|s| (s.first.displacement, s.displacement_width(4).unwrap()))),
            [Some((-4, 12)), Some((8, 8))]
        );
        assert_eq!(
            instructions.map(|i| proof.stream_for(i.operation)),
            [Some(0), Some(1), None, Some(0), Some(1), None, None, None]
        );
    }

    #[cfg(not(feature = "parallel"))]
    #[test]
    fn paged_f32_read_loop_proves_only_two_aligned_ds_streams() {
        let context = ExecutionContext::default();
        let candidate = fixture();
        assert!(ReadLoop::recognize_paged_f32(&context, &candidate).is_some());
        let Operation::Vector(VectorInstruction::Move {
            operand: VectorOperand::Memory(address),
            ..
        }) = candidate[0].operation
        else {
            unreachable!();
        };
        for modified in [
            Address {
                segment: Segment::Fs,
                ..address
            },
            Address {
                size: Width::W32,
                ..address
            },
            Address {
                displacement: 5,
                ..address
            },
        ] {
            let mut instructions = candidate;
            let Operation::Vector(VectorInstruction::Move { operand, .. }) =
                &mut instructions[0].operation
            else {
                unreachable!();
            };
            *operand = VectorOperand::Memory(modified);
            assert!(ReadLoop::recognize_paged_f32(&context, &instructions).is_none());
        }
    }

    #[cfg(not(feature = "parallel"))]
    #[test]
    fn paged_f32_read_loop_requires_flat_32_bit_ds_for_dword_addressing() {
        use crate::x86::context::Mode;

        let mut context = ExecutionContext::default();
        context.set_mode_and_cpl(Mode::Protected, 0);
        let mut cs = context.segment(Segment::Cs);
        cs.default32 = true;
        let mut attributes = context.attributes(Segment::Cs);
        attributes.long = false;
        context.set_segment(Segment::Cs, 8, cs, attributes);
        let mut instructions = fixture();
        for instruction in &mut instructions {
            match &mut instruction.operation {
                Operation::Vector(VectorInstruction::Move {
                    operand: VectorOperand::Memory(address),
                    ..
                }) => address.size = Width::W32,
                Operation::Alu { width, .. } => *width = Width::W32,
                _ => (),
            }
        }
        let proof = ReadLoop::recognize_paged_f32(&context, &instructions).unwrap();
        assert_eq!(proof.count_width, Width::W32);
        let mut ds = context.segment(Segment::Ds);
        ds.limit = 0xffff;
        context.set_segment(Segment::Ds, 24, ds, context.attributes(Segment::Ds));
        assert!(ReadLoop::recognize_paged_f32(&context, &instructions).is_none());
    }

    #[test]
    fn scalar_read_loops_prove_two_streams_with_a_full_width_inc() {
        let context = ExecutionContext::default();
        let fixture = fixture();
        let mut instructions = [fixture[0], fixture[1], fixture[5], fixture[6], fixture[7]];
        for (index, instruction) in instructions.iter_mut().enumerate() {
            instruction.pc = fixture[0].pc + index as u64;
            instruction.next_pc = instruction.pc + 1;
        }
        assert!(ReadLoop::recognize(&context, &instructions).is_none());

        let inc = |width| Operation::Unary {
            kind: Unary::Inc,
            dst: Location::Register(RegisterView::low(1)),
            width,
        };
        instructions[2].operation = inc(Width::W64);
        let proof = ReadLoop::recognize(&context, &instructions).unwrap();
        assert_eq!(
            (proof.index, proof.step, proof.count_width),
            (1, 1, Width::W64)
        );

        instructions[2].operation = inc(Width::W32);
        assert!(ReadLoop::recognize(&context, &instructions).is_none());
        instructions[2].operation = inc(Width::W64);
        instructions[1].operation = Operation::Nop;
        assert!(ReadLoop::recognize(&context, &instructions).is_none());
    }

    #[test]
    fn affine_byte_reads_use_the_decoded_bound_and_reject_mutated_bases() {
        let mut instructions = fixture();
        for instruction in &mut instructions[..5] {
            if let Operation::Vector(VectorInstruction::Move {
                operand: VectorOperand::Memory(address),
                ..
            }) = instruction.operation
            {
                instruction.operation = Operation::Extend {
                    dst: RegisterView::low(2),
                    src: Location::Memory(address),
                    source_width: Width::W8,
                    width: Width::W32,
                    signed: false,
                };
            }
        }
        instructions[6].operation = Operation::Alu {
            kind: Alu::Cmp,
            dst: RegisterView::low(1),
            src: DataOperand::Immediate(73),
            width: Width::W32,
        };
        let context = ExecutionContext::default();
        let proof = ReadLoop::recognize_integer(&context, &instructions).unwrap();
        assert_eq!(proof.end, ReadLimit::Immediate(73));
        assert_eq!(proof.count_width, Width::W64);
        assert_eq!(proof.streams[0].unwrap().displacement_width(1), Some(9));
        let address = ReadLoop::read_operand(instructions[0].operation, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            proof.byte_slot(address),
            Some((
                0,
                Address {
                    displacement: -4,
                    ..address
                },
                8
            ))
        );
        assert!(
            proof
                .byte_slot(Address {
                    displacement: 5,
                    ..address
                })
                .is_none()
        );
        instructions[2].operation = Operation::Mov {
            dst: RegisterView::low(6),
            src: DataOperand::Immediate(0),
            width: Width::W32,
        };
        assert!(ReadLoop::recognize_integer(&context, &instructions).is_none());
    }

    #[test]
    fn read_loop_rejects_unproved_control_address_and_register_effects() {
        let context = ExecutionContext::default();
        let candidate = fixture();
        let reg = RegisterView::low(1);
        let add = |step, width| Operation::Alu {
            kind: Alu::Add,
            dst: reg,
            src: DataOperand::Immediate(step),
            width,
        };
        for (index, replacement) in [
            (2, add(1, Width::W64)),
            (2, read(5, 0)),
            (3, read(1, 0)),
            (3, read(6, i64::MIN)),
            (5, add(3, Width::W64)),
            (5, add(2, Width::W32)),
            (5, add(0, Width::W64)),
            (
                7,
                Operation::Jcc {
                    condition: Condition::Equal,
                    target: candidate[0].pc,
                },
            ),
            (
                7,
                Operation::Jcc {
                    condition: Condition::NotEqual,
                    target: candidate[1].pc,
                },
            ),
        ] {
            let mut instructions = candidate;
            instructions[index].operation = replacement;
            assert!(
                ReadLoop::recognize(&context, &instructions).is_none(),
                "{replacement:?}"
            );
        }
        let mut instructions = candidate;
        instructions[2].next_pc += 1;
        assert!(ReadLoop::recognize(&context, &instructions).is_none());
    }
}
