use super::{Emitter, StructuredLoop, Unit, VALUE};
use crate::embedding::layout::INDEXED_WRITE_UNSUPPORTED_LEAF;
use crate::memory::IndexedDwordWrite;
use crate::x86::{
    context::Segment,
    instruction::{Address, AddressBase, Alu, Condition, DataOperand, Location, Operation, Width},
};
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

#[derive(Clone, Copy)]
pub(super) struct IndexedWrite {
    pub(super) index: u32,
    pub(super) lookup: u32,
    pub(super) store: u32,
    table: Address,
    destination: Address,
}

impl IndexedWrite {
    pub(super) fn recognize(body: StructuredLoop, unit: &Unit) -> Option<Self> {
        let instructions = &unit.instructions;
        let order = body.order();
        for position in 0..order.len().saturating_sub(4) {
            let first = order.at(position);
            if order.at(position + 4) != first + 4 {
                continue;
            }
            let [index, test, branch, lookup, store] =
                &instructions[first as usize..first as usize + 5]
            else {
                unreachable!("five consecutive operand instructions");
            };
            let Operation::Extend {
                dst: index_register,
                src: Location::Memory(index_address),
                width: Width::W32,
                source_width: Width::W8,
                signed: false,
            } = index.operation
            else {
                continue;
            };
            let Operation::Alu {
                kind: Alu::Test,
                dst,
                src: DataOperand::Register(src),
                width: Width::W32 | Width::W64,
            } = test.operation
            else {
                continue;
            };
            let Operation::Jcc {
                condition: Condition::Equal,
                target,
            } = branch.operation
            else {
                continue;
            };
            let Operation::Load {
                dst: value,
                address: table,
                width: Width::W32,
            } = lookup.operation
            else {
                continue;
            };
            let Operation::Store {
                src: DataOperand::Register(source),
                address: destination,
                width: Width::W32,
            } = store.operation
            else {
                continue;
            };
            let (AddressBase::Register(table_base), Some(table_index)) = (table.base, table.index)
            else {
                continue;
            };
            let AddressBase::Register(store_base) = destination.base else {
                continue;
            };
            let changes = |bank| bank == index_register.bank || bank == value.bank;
            if dst != index_register
                || src != index_register
                || source != value
                || table_index.register != index_register.bank
                || table_index.scale != 4
                || table_base == index_register.bank
                || changes(store_base)
                || destination.index.is_some_and(|index| changes(index.register))
                || [index_address, table, destination].iter().any(|address|
                    address.size != Width::W64 || address.segment != Segment::Ds
                )
                || (test.pc..=store.pc).contains(&target)
                || [index, test, branch, lookup].into_iter()
                    .zip([test, branch, lookup, store])
                    .any(|(before, after)| before.next_pc != after.pc)
                || instructions[order.earlier as usize..=order.tail as usize].iter().any(|i| {
                    matches!(i.operation, Operation::Jump { target } | Operation::Jcc { target, .. }
                        if (test.pc..=store.pc).contains(&target))
                })
            {
                continue;
            }
            return Some(Self {
                index: first,
                lookup: first + 3,
                store: first + 4,
                table: Address {
                    index: None,
                    ..table
                },
                destination,
            });
        }
        None
    }
}

#[derive(Clone, Copy)]
pub(super) struct GuardedIndexedWrite {
    pub(super) shape: IndexedWrite,
    pub(super) resolve: u32,
    pub(super) record: u32,
    pub(super) host: u32,
    pub(super) continuing: u32,
}

impl Emitter {
    pub(super) fn indexed_write(&self) -> Option<GuardedIndexedWrite> {
        self.paged_indexed_write.filter(|_| {
            self.guarded_paged_lap
                && self
                    .structured_loop
                    .is_some_and(|(body, _)| body.paged_scalar_writes())
        })
    }

    pub(super) fn load_indexed_write(
        &mut self,
        group: GuardedIndexedWrite,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        let first = self.current_index == group.shape.index;
        let field = |offset: usize| MemArg {
            offset: offset as u64,
            align: 2,
            memory_index: 0,
        };
        let unsupported = INDEXED_WRITE_UNSUPPORTED_LEAF as i32;
        if first {
            self.op(Op::I32Const(0))
                .op(Op::LocalSet(group.host))
                .op(Op::LocalGet(group.record))
                .op(Op::I32Const(unsupported))
                .op(Op::I32Ne)
                .op(Op::If(BlockType::Empty));
            self.address(address, next_pc);
            self.address(group.shape.table, 0);
            self.address(group.shape.destination, 0);
            self.op(Op::LocalGet(group.continuing))
                .op(Op::Call(group.resolve))
                .op(Op::LocalSet(group.record))
                .op(Op::I32Const(1))
                .op(Op::LocalSet(group.continuing))
                .op(Op::End);
        }
        // One unsuitable leaf disables grouping until the guarded clone ends.
        self.op(Op::LocalGet(group.record))
            .op(Op::I32Const(unsupported))
            .op(Op::I32GtU)
            .op(Op::If(BlockType::Empty));
        if first {
            self.op(Op::LocalGet(group.record))
                .op(Op::I32Load(field(std::mem::offset_of!(
                    IndexedDwordWrite,
                    host
                ))))
                .op(Op::LocalSet(group.host));
        }
        let offset = if first {
            std::mem::offset_of!(IndexedDwordWrite, index)
        } else {
            std::mem::offset_of!(IndexedDwordWrite, value)
        };
        self.op(Op::LocalGet(group.record))
            .op(Op::I64Load32U(field(offset)))
            .op(Op::LocalSet(VALUE))
            .op(Op::Else);
        self.paged_indexed_write = None;
        assert!(self.load_guarded_paged_value(address, width, next_pc, exit_depth + 1));
        self.paged_indexed_write = Some(group);
        self.op(Op::End);
    }
}
