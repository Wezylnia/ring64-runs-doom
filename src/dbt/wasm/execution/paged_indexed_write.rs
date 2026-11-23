use super::control::InstructionRetirement;
use super::{ADDRESS, Emitter, RESULT, RIP, STORE_STATUS, StructuredLoop, Unit, VALUE};
use crate::embedding::layout::INDEXED_WRITE_UNSUPPORTED_LEAF;
use crate::x86::paging::cache::IndexedDwordWrite;
use crate::x86::paging::cache::IndexedReadTable;
use crate::x86::{
    context::Segment,
    instruction::{
        Address, AddressBase, Alu, Condition, DataOperand, Location, Operation, RegisterView, Width,
    },
};
use std::mem::offset_of;
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

#[derive(Clone, Copy)]
pub(super) struct IndexedWrite {
    pub(super) index: u32,
    pub(super) lookup: u32,
    pub(super) store: u32,
    index_register: RegisterView,
    value_register: RegisterView,
    next_pc: u64,
    zero_target: u64,
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
                index_register,
                value_register: value,
                next_pc: store.next_pc,
                zero_target: target,
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
    pub(super) value: u32,
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
        let unsupported = INDEXED_WRITE_UNSUPPORTED_LEAF as i32;
        if first {
            self.op(Op::I32Const(0))
                .op(Op::LocalSet(group.host))
                .op(Op::Block(BlockType::Empty));
            self.reuse_indexed_write(group, address, next_pc, exit_depth);
            self.op(Op::LocalGet(group.record))
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
        self.op(Op::LocalGet(group.record))
            .op(Op::I32Const(unsupported))
            .op(Op::I32GtU)
            .op(Op::If(BlockType::Empty));
        if first {
            self.op(Op::LocalGet(group.record))
                .op(Op::I32Load(field(offset_of!(IndexedDwordWrite, host), 2)))
                .op(Op::LocalSet(group.host))
                .op(Op::LocalGet(group.record))
                .op(Op::I32Load(field(offset_of!(IndexedDwordWrite, value), 2)))
                .op(Op::LocalSet(group.value))
                .op(Op::LocalGet(group.record))
                .op(Op::I64Load32U(field(
                    offset_of!(IndexedDwordWrite, index),
                    2,
                )));
        } else {
            self.op(Op::LocalGet(group.value)).op(Op::I64ExtendI32U);
        }
        self.op(Op::LocalSet(VALUE)).op(Op::Else);
        self.paged_indexed_write = None;
        assert!(self.load_guarded_paged_value(
            address,
            width,
            next_pc,
            exit_depth + 1 + u32::from(first)
        ));
        self.paged_indexed_write = Some(group);
        self.op(Op::End);
        if first {
            self.op(Op::End);
        }
    }

    fn finish_admitted_indexed_write(&mut self, group: GuardedIndexedWrite, exit_depth: u32) {
        // A host pointer proves a nonzero byte and both complete dwords. The
        // lap already excluded intermediate stops, and funded all five PCs.
        if group.shape.index_register != group.shape.value_register {
            self.op(Op::LocalGet(VALUE));
            self.write_register(group.shape.index_register, Width::W32);
        }
        // TEST on a zero-extended byte has the same status in 32 and 64 bits.
        self.op(Op::LocalGet(VALUE)).op(Op::LocalSet(RESULT));
        self.logical_flags(Width::W32);
        self.op(Op::LocalGet(group.value)).op(Op::I64ExtendI32U);
        self.write_register(group.shape.value_register, Width::W32);
        self.op(Op::LocalGet(group.host))
            .op(Op::LocalGet(group.value))
            .op(Op::I32Store(field(0, 2)))
            .op(Op::I64Const(group.shape.next_pc as i64))
            .op(Op::LocalSet(RIP));
        self.retire_instructions(5);
        self.route(group.shape.next_pc, exit_depth - 1, exit_depth);
    }

    fn reuse_indexed_write(
        &mut self,
        group: GuardedIndexedWrite,
        address: Address,
        next_pc: u64,
        exit_depth: u32,
    ) {
        let funded = self.loop_gates_prechecked
            && matches!(self.instruction_retirement, InstructionRetirement::One);
        self.op(Op::Block(BlockType::Empty))
            .op(Op::LocalGet(group.record))
            .op(Op::I32Const(INDEXED_WRITE_UNSUPPORTED_LEAF as i32))
            .op(Op::I32LeU)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(group.record))
            .op(Op::I32Load(field(
                offset_of!(IndexedDwordWrite, recency),
                2,
            )))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load(field(
                offset_of!(IndexedReadTable, generation),
                3,
            )))
            .op(Op::LocalGet(group.record))
            .op(Op::I64Load(field(
                offset_of!(IndexedDwordWrite, generation),
                3,
            )))
            .op(Op::I64Ne)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(field(offset_of!(IndexedReadTable, hint), 2)))
            .op(Op::LocalGet(group.record))
            .op(Op::I32Load(field(offset_of!(IndexedDwordWrite, fetch), 2)))
            .op(Op::I32Ne)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load(field(offset_of!(IndexedReadTable, clock), 3)))
            .op(Op::I64Const(-5))
            .op(Op::I64GtU)
            .op(Op::BrIf(0));
        self.address(address, next_pc);
        self.indexed_page_host(group, 0, 1);
        self.op(Op::I64Load8U(field(0, 0)))
            .op(Op::LocalTee(VALUE))
            .op(Op::I64Eqz)
            .op(Op::If(BlockType::Empty));
        self.touch_indexed_slots(group, &[0, 3]);
        if funded
            && crate::memory::canonical48(group.shape.zero_target)
            && self.entries.contains_key(&group.shape.zero_target)
        {
            self.op(Op::I64Const(0));
            self.write_register(group.shape.index_register, Width::W32);
            self.op(Op::I64Const(0x46))
                .op(Op::LocalSet(super::FLAGS))
                .op(Op::I64Const(group.shape.zero_target as i64))
                .op(Op::LocalSet(RIP));
            self.retire_instructions(3);
            self.route(group.shape.zero_target, exit_depth + 2, exit_depth + 3);
        } else {
            self.op(Op::Br(2));
        }
        self.op(Op::End);
        self.address(group.shape.table, 0);
        self.op(Op::LocalGet(VALUE))
            .op(Op::I64Const(2))
            .op(Op::I64Shl)
            .op(Op::I64Add);
        self.indexed_page_host(group, 1, 4);
        self.op(Op::I32Load(field(0, 2)))
            .op(Op::LocalSet(group.value));
        self.address(group.shape.destination, 0);
        self.indexed_page_host(group, 2, 4);
        self.op(Op::LocalSet(group.host));
        self.touch_indexed_slots(group, &[0, 1, 2, 3]);
        if funded {
            self.finish_admitted_indexed_write(group, exit_depth + 2);
        } else {
            self.op(Op::Br(1));
        }
        self.op(Op::End);
    }

    // One tag checks the entire canonical page and the dword alignment.
    fn indexed_page_host(&mut self, group: GuardedIndexedWrite, index: usize, width: i64) {
        self.op(Op::LocalTee(ADDRESS))
            .op(Op::I64Const(!4095 | (width - 1)))
            .op(Op::I64And)
            .op(Op::I64Const(4))
            .op(Op::I64Or)
            .op(Op::LocalGet(group.record))
            .op(Op::I64Load(field(
                offset_of!(IndexedDwordWrite, tags) + 8 * index,
                3,
            )))
            .op(Op::I64Ne)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(group.record))
            .op(Op::I32Load(field(
                offset_of!(IndexedDwordWrite, biases) + 4 * index,
                2,
            )))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Add);
    }

    fn touch_indexed_slots(&mut self, group: GuardedIndexedWrite, slots: &[usize]) {
        let clock = field(offset_of!(IndexedReadTable, clock), 3);
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load(clock))
            .op(Op::LocalSet(ADDRESS));
        for &slot in slots {
            self.op(Op::LocalGet(group.record))
                .op(Op::I32Load(field(
                    offset_of!(IndexedDwordWrite, ages) + 4 * slot,
                    2,
                )))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::I64Const(1))
                .op(Op::I64Add)
                .op(Op::LocalTee(ADDRESS))
                .op(Op::I64Store(field(0, 3)));
        }
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I64Store(clock));
    }
}

fn field(offset: usize, align: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align,
        memory_index: 0,
    }
}
