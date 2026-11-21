use crate::x86::{
    context::{ExecutionContext, Segment},
    instruction::{Address, AddressBase, Alu, Instruction, Location, Operation, Width},
};

#[derive(Clone, Copy)]
pub(super) struct StackSlots {
    first: Address,
    bytes: u32,
    pub(super) writes: bool,
}

impl StackSlots {
    pub(super) fn recognize(context: &ExecutionContext, body: &[Instruction]) -> Option<Self> {
        if context.is_long64() || context.segment_base(Segment::Ss) != 0 {
            return None;
        }
        let mut first = None::<Address>;
        let mut end = i64::MIN;
        let mut reads = 0;
        let mut writes = 0;
        for instruction in body {
            let operation = instruction.operation;
            if !preserves_stack_pointer(operation) {
                return None;
            }
            let (address, width, write) = match operation {
                Operation::Load { address, width, .. }
                | Operation::MemoryAlu {
                    kind: Alu::Cmp | Alu::Test,
                    address,
                    width,
                    ..
                } => (address, width, false),
                Operation::Store { address, width, .. } => (address, width, true),
                _ => continue,
            };
            if !stack_slot(address) {
                continue;
            }
            if first.is_none_or(|previous| address.displacement < previous.displacement) {
                first = Some(address);
            }
            end = end.max(address.displacement.checked_add(i64::from(width.bytes()))?);
            reads += u32::from(!write);
            writes += u32::from(write);
        }
        if reads == 0 || reads + writes < 3 {
            return None;
        }
        let first = first?;
        let bytes = u32::try_from(end.checked_sub(first.displacement)?).ok()?;
        Some(Self {
            first,
            bytes,
            writes: writes != 0,
        })
    }

    pub(super) fn offset(self, address: Address, width: Width) -> Option<u64> {
        if !stack_slot(address) {
            return None;
        }
        let offset =
            u32::try_from(address.displacement.checked_sub(self.first.displacement)?).ok()?;
        (offset.checked_add(u32::from(width.bytes()))? <= self.bytes).then_some(u64::from(offset))
    }
}

fn stack_slot(address: Address) -> bool {
    address.segment == Segment::Ss
        && address.size == Width::W32
        && address.base == AddressBase::Register(4)
        && address.index.is_none()
}

fn preserves_stack_pointer(operation: Operation) -> bool {
    match operation {
        Operation::Alu { kind, dst, .. } => !kind.writes_result() || dst.bank != 4,
        Operation::Cmov { dst, .. }
        | Operation::Extend { dst, .. }
        | Operation::Lea { dst, .. }
        | Operation::Load { dst, .. }
        | Operation::Mov { dst, .. }
        | Operation::Shift {
            dst: Location::Register(dst),
            ..
        }
        | Operation::Unary {
            dst: Location::Register(dst),
            ..
        } => dst.bank != 4,
        Operation::Jump { .. }
        | Operation::Jcc { .. }
        | Operation::MemoryAlu {
            kind: Alu::Cmp | Alu::Test,
            ..
        }
        | Operation::Nop
        | Operation::Store { .. } => true,
        _ => false,
    }
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(super) struct StackGuard {
    pub(super) slots: StackSlots,
    pub(super) read: u32,
    pub(super) write: Option<u32>,
    pub(super) host: u32,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
impl super::Emitter {
    pub(super) fn guard_stack_slots(&mut self, guard: StackGuard) {
        use super::ADDRESS;
        use crate::memory::DirectSpan;
        use wasm_encoder::{BlockType, Instruction as Op, MemArg, ValType};

        let StackGuard {
            slots: proof,
            read,
            write,
            host,
        } = guard;
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0));
        self.address(proof.first, 0);
        self.op(Op::LocalSet(ADDRESS));
        // SS translates this whole interval, including the last byte, with
        // its actual address-size arithmetic and segment permissions.
        for resolver in [Some(read), write].into_iter().flatten() {
            self.op(Op::LocalGet(ADDRESS))
                .op(Op::I32Const(proof.bytes as i32))
                .op(Op::I32Const(Segment::Ss as i32))
                .op(Op::Call(resolver))
                .op(Op::LocalTee(host))
                .op(Op::I32Eqz)
                .op(Op::BrIf(0));
        }
        let field = |offset, align| MemArg {
            offset,
            align,
            memory_index: 0,
        };
        // Both successful resolvers refer to this same uniquely mapped range.
        self.op(Op::Drop)
            .op(Op::LocalGet(host))
            .op(Op::I32Load(field(
                std::mem::offset_of!(DirectSpan, host) as u64,
                2,
            )))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::LocalGet(host))
            .op(Op::I64Load(field(
                std::mem::offset_of!(DirectSpan, base) as u64,
                3,
            )))
            .op(Op::I64Sub)
            .op(Op::I32WrapI64)
            .op(Op::I32Add)
            .op(Op::LocalSet(host))
            .op(Op::I32Const(1))
            .op(Op::End);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86::instruction::{DataOperand, RegisterView};

    #[test]
    fn fixed_stack_slots_keep_widths_and_reject_stack_pointer_writes() {
        let slot = |displacement| Address {
            size: Width::W32,
            base: AddressBase::Register(4),
            segment: Segment::Ss,
            displacement,
            ..Address::default()
        };
        let load = |address| Operation::Load {
            dst: RegisterView::low(0),
            address,
            width: Width::W32,
        };
        let instructions = [
            load(slot(8)),
            Operation::Store {
                src: DataOperand::Immediate(1),
                address: slot(-4),
                width: Width::W32,
            },
            load(slot(12)),
        ];
        let mut body: Vec<_> = instructions
            .into_iter()
            .enumerate()
            .map(|(i, operation)| Instruction {
                pc: 0x1000 + i as u64,
                next_pc: 0x1001 + i as u64,
                operation,
            })
            .collect();
        let mut context = ExecutionContext::reset();
        let proof = StackSlots::recognize(&context, &body).unwrap();
        assert!(proof.writes);
        assert_eq!(proof.offset(slot(-4), Width::W32), Some(0));
        assert_eq!(proof.offset(slot(12), Width::W32), Some(16));
        assert_eq!(proof.offset(slot(12), Width::W64), None);
        context.set_segment_base(Segment::Ss, 0x1000);
        assert!(StackSlots::recognize(&context, &body).is_none());
        context.set_segment_base(Segment::Ss, 0);
        body.push(Instruction {
            pc: 0x1003,
            next_pc: 0x1004,
            operation: Operation::Mov {
                dst: RegisterView::low(4),
                src: DataOperand::Immediate(0),
                width: Width::W16,
            },
        });
        assert!(StackSlots::recognize(&context, &body).is_none());
    }
}
