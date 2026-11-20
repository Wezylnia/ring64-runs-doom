use crate::x86::{
    context::ExecutionContext,
    float::{Format, Kind},
    instruction::{
        Address, AddressBase, Alu, Condition, DataOperand, Instruction, Location, Operation,
        VectorInstruction, VectorOperand, Width,
    },
};

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

    fn displacement_width(self) -> Option<u32> {
        let distance = self
            .last_displacement
            .checked_sub(self.first.displacement)?;
        u32::try_from(distance).ok()?.checked_add(4)
    }
}

#[derive(Clone, Copy)]
pub(super) struct ReadLoop {
    index: u8,
    end: u8,
    count_width: Width,
    step: u64,
    streams: [Option<Stream>; 2],
}

impl ReadLoop {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) const BIAS_LOCALS: u32 = 2;

    pub(super) fn recognize(
        context: &ExecutionContext,
        instructions: &[Instruction],
    ) -> Option<Self> {
        let [body @ .., add, compare, branch] = instructions else {
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
        let Operation::Alu {
            kind: Alu::Add,
            dst: index,
            src: DataOperand::Immediate(step),
            width: count_width @ (Width::W32 | Width::W64),
        } = add.operation
        else {
            return None;
        };
        // A power-of-two step lets the guard prove the end is reached by the backedge.
        if index.high_byte || !step.is_power_of_two() || step > u64::from(u32::MAX) {
            return None;
        }
        let Operation::Alu {
            kind: Alu::Cmp,
            dst: left,
            src: DataOperand::Register(right),
            width,
        } = compare.operation
        else {
            return None;
        };
        if width != count_width || left.high_byte || right.high_byte {
            return None;
        }
        let end = if left == index {
            right
        } else if right == index {
            left
        } else {
            return None;
        };
        if end == index {
            return None;
        }
        let mut proof = Self {
            index: index.bank,
            end: end.bank,
            count_width,
            step,
            streams: [None; 2],
        };
        let mut reads = 0;
        for instruction in body {
            let Some(address) = Self::read_operand(instruction.operation)? else {
                continue;
            };
            let index = address.index?;
            // All other registers are stable in this restricted body. Address-sized
            // truncation of the induction register may not depend on a partial write.
            if index.register != proof.index
                || address.size.bits() > count_width.bits()
                || !matches!(address.size, Width::W32 | Width::W64)
                || address.base == AddressBase::Register(proof.index)
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
                stream.displacement_width()?;
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
        (reads >= 4).then_some(proof)
    }

    fn read_operand(operation: Operation) -> Option<Option<Address>> {
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

    pub(super) fn stream_for(self, operation: Operation) -> Option<u32> {
        let address = Self::read_operand(operation)??;
        self.streams
            .iter()
            .position(|stream| stream.is_some_and(|stream| stream.matches(address)))
            .map(|index| index as u32)
    }
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::{ADDRESS, Emitter, LHS, RHS, STORE_STATUS, VALUE, memory::SpanCache};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use wasm_encoder::{BlockType, Instruction as Op, MemArg, ValType};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
impl Emitter {
    pub(super) fn guard_read_loop(
        &mut self,
        proof: ReadLoop,
        span: SpanCache,
        first_bias: u32,
        guard_failed: u32,
    ) {
        use crate::memory::DirectSpan as Descriptor;

        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0));
        // A later LDMXCSR can change rounding within this artifact. Only a
        // mapping or address proof failure latches the checked placement.
        self.load_float_mxcsr();
        self.reject_non_nearest_fp32();
        self.op(Op::Drop)
            .op(Op::Block(BlockType::Empty))
            .op(Op::LocalGet(guard_failed))
            .op(Op::BrIf(0));
        self.register(proof.end, proof.count_width);
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

        for (slot, stream) in proof.streams.iter().flatten().enumerate() {
            let scale = u64::from(stream.first.index.unwrap().scale);
            let displacement_width = u64::from(stream.displacement_width().unwrap());
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
                .op(Op::Call(span.resolve))
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
                .op(Op::I32Sub)
                .op(Op::LocalSet(first_bias + slot as u32));
        }

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
            (1, 3, Width::W64, 2)
        );
        assert_eq!(
            proof
                .streams
                .map(|s| s.map(|s| (s.first.displacement, s.displacement_width().unwrap()))),
            [Some((-4, 12)), Some((8, 8))]
        );
        assert_eq!(
            instructions.map(|i| proof.stream_for(i.operation)),
            [Some(0), Some(1), None, Some(0), Some(1), None, None, None]
        );
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
