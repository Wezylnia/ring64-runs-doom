use crate::dbt::wasm::Emitter;
use crate::dbt::wasm::PRIVATE_MEMORY;
use crate::x86::instruction::Operation;
use wasm_encoder::{BlockType, Instruction as Op};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::dbt::wasm) enum IndexedOperand {
    Read,
    Write,
    ReadModifyWrite,
}

pub(in crate::dbt::wasm) fn indexed_scalar_read(operation: Operation) -> bool {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{Location, MemoryAluOperands, Width};
    let address = match operation {
        Operation::Load { address, .. }
        | Operation::Extend {
            src: Location::Memory(address),
            ..
        } => address,
        Operation::MemoryAlu {
            address,
            kind,
            operands,
            ..
        } if !kind.writes_result() || matches!(operands, MemoryAluOperands::RegisterFirst(_)) => {
            address
        }
        Operation::Pop { .. }
        | Operation::Return {
            width: Width::W64, ..
        } => return true,
        _ => return false,
    };
    matches!(address.segment, Segment::Ds | Segment::Ss)
}

pub(in crate::dbt::wasm) fn indexed_scalar_write(operation: Operation, sampled: bool) -> bool {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{Operand, Width};

    match operation {
        Operation::Store { address, .. } => matches!(address.segment, Segment::Ds | Segment::Ss),
        Operation::Call {
            target: Operand::Register(_) | Operand::Immediate(_),
            width: Width::W64,
        } => true,
        Operation::Push {
            src: Operand::Register(_) | Operand::Immediate(_),
            width: Width::W64,
        } => sampled,
        _ => indexed_scalar_rmw(operation),
    }
}

pub(in crate::dbt::wasm) fn indexed_scalar_rmw(operation: Operation) -> bool {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{Location, MemoryAluOperands, Width};

    let address = match operation {
        Operation::MemoryAlu {
            address,
            kind,
            operands: MemoryAluOperands::MemoryFirst(_),
            width: Width::W32,
        } if kind.writes_result() => address,
        Operation::Shift {
            dst: Location::Memory(address),
            width: Width::W32,
            ..
        } => address,
        _ => return false,
    };
    matches!(address.segment, Segment::Ds | Segment::Ss)
}

impl Emitter {
    pub(in crate::dbt::wasm) fn uses_indexed_scalar_read(&self, operation: Operation) -> bool {
        self.indexed_scalar_stamp.is_some()
            && !self.guarded_paged_lap
            && indexed_scalar_read(operation)
    }

    pub(in crate::dbt::wasm) fn uses_indexed_scalar_write(&self, operation: Operation) -> bool {
        self.indexed_store_fill.is_some()
            && !self.guarded_paged_lap
            && indexed_scalar_write(operation, self.sampled_region)
    }

    pub(in crate::dbt::wasm) fn uses_indexed_scalar_rmw(&self) -> bool {
        self.indexed_store_fill.is_some()
            && !self.guarded_paged_lap
            && indexed_scalar_rmw(self.current_operation)
    }

    pub(in crate::dbt::wasm) fn indexed_prefix_operand_host(&mut self, read: u32, width: u8) {
        use crate::dbt::wasm::{ADDRESS, STORE_STATUS};
        use wasm_encoder::{MemArg, ValType};

        self.op(Op::LocalSet(ADDRESS));
        self.indexed_read_host(
            self.indexed_paged_read.expect("indexed prefix locals"),
            width,
            true,
            IndexedOperand::Read,
        );
        self.op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Result(ValType::I32)))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::Else)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32Const(i32::from(width)))
            .op(Op::I32Const(
                crate::embedding::layout::paging::PREFIX_INDEXED_READ_SEGMENT as i32,
            ))
            .op(Op::Call(read))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Result(ValType::I32)))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(MemArg {
                offset: std::mem::offset_of!(crate::memory::DirectSpan, host) as u64,
                align: 0,
                memory_index: PRIVATE_MEMORY,
            }))
            .op(Op::Else)
            .op(Op::I32Const(0))
            .op(Op::End)
            .op(Op::End);
    }

    pub(in crate::dbt::wasm) fn indexed_read_host(
        &mut self,
        [table, entry, data, fetch]: [u32; 4],
        width: u8,
        preserve_fetch: bool,
        operand: IndexedOperand,
    ) {
        use crate::dbt::wasm::{ADDRESS, VALUE};
        use crate::embedding::layout::StateStorage;
        use crate::x86::paging::cache::{
            CAPACITY, INDEXED_READ_ENTRIES, INDEXED_VALID, INDEXED_WRITE_READABLE,
            IndexedReadEntry, IndexedReadTable,
        };
        use std::mem::{offset_of, size_of};
        use wasm_encoder::{MemArg, ValType};

        let mem = |offset, align| MemArg {
            offset: offset as u64,
            align,
            memory_index: PRIVATE_MEMORY,
        };
        let clock = mem(offset_of!(IndexedReadTable, clock), 3);
        let ages = mem(offset_of!(IndexedReadTable, ages), 3);
        let epoch = mem(offset_of!(IndexedReadTable, epoch), 3);
        let records = if operand != IndexedOperand::Read {
            offset_of!(IndexedReadTable, writes)
        } else {
            offset_of!(IndexedReadTable, entries)
        };
        let stamp = if preserve_fetch {
            VALUE
        } else {
            self.indexed_scalar_stamp.unwrap()
        };
        let data_address = if preserve_fetch { data } else { fetch };
        // A record is never indexed by the final byte's page if the access
        // crosses a boundary. Its tag also distinguishes a default record.
        self.op(Op::Block(BlockType::Result(ValType::I32)));
        // A proved data/fetch pair stays ordered until another indexed operand
        // changes it or control enters Core. The host bias covers one whole page.
        if let Some((page, bias, rights)) = self.ordered_operand {
            if preserve_fetch {
                let needed = match operand {
                    IndexedOperand::Read => 1,
                    IndexedOperand::Write => 2,
                    IndexedOperand::ReadModifyWrite => 3,
                };
                self.op(Op::LocalGet(ADDRESS))
                    .op(Op::LocalGet(page))
                    .op(Op::I64Sub)
                    .op(Op::I64Const(4096 - i64::from(width)))
                    .op(Op::I64LeU)
                    .op(Op::LocalGet(rights))
                    .op(Op::I32Const(needed))
                    .op(Op::I32And)
                    .op(Op::I32Const(needed))
                    .op(Op::I32Eq)
                    .op(Op::I32And)
                    .op(Op::If(BlockType::Empty))
                    .op(Op::LocalGet(bias))
                    .op(Op::LocalGet(ADDRESS))
                    .op(Op::I32WrapI64)
                    .op(Op::I32Add)
                    .op(Op::Br(1))
                    .op(Op::End);
            }
            self.op(Op::I32Const(0)).op(Op::LocalSet(rights));
        }
        self.op(Op::I32Const(0))
            .op(Op::Block(BlockType::Empty))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(mem(
                offset_of!(StateStorage, indexed_paged_reads),
                2,
            )))
            .op(Op::LocalTee(table))
            .op(Op::I32Eqz)
            .indexed_miss(Op::BrIf(0))
            .op(Op::LocalGet(table))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Const(12))
            .op(Op::I32ShrU)
            .op(Op::I32Const(INDEXED_READ_ENTRIES as i32 - 1))
            .op(Op::I32And)
            .op(Op::I32Const(size_of::<IndexedReadEntry>() as i32))
            .op(Op::I32Mul)
            .op(Op::I32Add)
            .op(Op::LocalTee(entry))
            .op(Op::I64Load(mem(
                records + offset_of!(IndexedReadEntry, tag),
                3,
            )));
        if operand == IndexedOperand::Write {
            self.op(Op::I64Const(!INDEXED_WRITE_READABLE as i64))
                .op(Op::I64And);
        }
        self.op(Op::LocalGet(ADDRESS));
        if width > 1 {
            self.op(Op::I64Const(i64::from(width - 1))).op(Op::I64Add);
        }
        let required = INDEXED_VALID
            | if operand == IndexedOperand::ReadModifyWrite {
                INDEXED_WRITE_READABLE
            } else {
                0
            };
        self.op(Op::I64Const(!4095))
            .op(Op::I64And)
            .op(Op::I64Const(required as i64))
            .op(Op::I64Or)
            .op(Op::LocalGet(table))
            .op(Op::I64Load(epoch))
            .op(Op::I64Or)
            .op(Op::I64Ne)
            .indexed_miss(Op::BrIf(0))
            .op(Op::LocalGet(table))
            .op(Op::LocalGet(entry))
            .op(Op::I32Load(mem(
                records + offset_of!(IndexedReadEntry, slot_offset),
                2,
            )));
        if !preserve_fetch {
            self.op(Op::LocalTee(data));
        }
        self.op(Op::I32Add).op(Op::LocalSet(data_address));
        let hint = mem(offset_of!(IndexedReadTable, hint), 2);
        if preserve_fetch {
            self.op(Op::LocalGet(table))
                .op(Op::I32Load(hint))
                .op(Op::LocalTee(fetch))
                .op(Op::I32Const(CAPACITY as i32))
                .op(Op::I32GeU)
                .indexed_miss(Op::BrIf(0))
                .op(Op::LocalGet(table))
                .op(Op::LocalGet(fetch))
                .op(Op::I32Const(3))
                .op(Op::I32Shl)
                .op(Op::I32Add)
                .op(Op::LocalSet(fetch))
                .op(Op::LocalGet(table))
                .op(Op::I64Load(clock))
                .op(Op::LocalTee(stamp))
                .op(Op::I64Const(-3))
                .op(Op::I64GtU)
                .indexed_miss(Op::BrIf(0))
                // Retiring a slot clears its age. A live published operand
                // implies a positive clock, so age also proves fetch liveness.
                .op(Op::LocalGet(fetch))
                .op(Op::I64Load(ages))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Ne)
                .indexed_miss(Op::BrIf(0));

            // Match data_translation_then_fetch. Leave the hot data/fetch order
            // untouched if it already matches and otherwise install both ages.
            self.op(Op::LocalGet(data))
                .op(Op::LocalGet(fetch))
                .op(Op::I32Ne)
                .op(Op::LocalGet(data))
                .op(Op::I64Load(ages))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Const(1))
                .op(Op::I64Sub)
                .op(Op::I64Ne)
                .op(Op::I32And)
                .op(Op::If(BlockType::Empty));
            for (slot, advance) in [(data, 1), (fetch, 2)] {
                self.op(Op::LocalGet(slot))
                    .op(Op::LocalGet(stamp))
                    .op(Op::I64Const(advance))
                    .op(Op::I64Add)
                    .op(Op::I64Store(ages));
            }
            self.op(Op::LocalGet(table))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Const(2))
                .op(Op::I64Add)
                .op(Op::I64Store(clock))
                .op(Op::End);
        } else {
            // A checked scalar operand touches only the data leaf.
            self.op(Op::LocalGet(table))
                .op(Op::I64Load(clock))
                .op(Op::LocalTee(stamp))
                .op(Op::I64Const(-1))
                .op(Op::I64Eq)
                .indexed_miss(Op::BrIf(0))
                .op(Op::LocalGet(table))
                .op(Op::LocalGet(data))
                .op(Op::I32Const(3))
                .op(Op::I32ShrU)
                .op(Op::I32Store(hint))
                .op(Op::LocalGet(fetch))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Const(1))
                .op(Op::I64Add)
                .op(Op::LocalTee(stamp))
                .op(Op::I64Store(ages))
                .op(Op::LocalGet(table))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Store(clock));
        }
        if let Some((page, _, rights)) = self.ordered_operand.filter(|_| preserve_fetch) {
            self.op(Op::LocalGet(ADDRESS))
                .op(Op::I64Const(!4095))
                .op(Op::I64And)
                .op(Op::LocalSet(page));
            // Reads grant no write access. A write grants read access only if
            // the write record certified the identical readable backing.
            if operand == IndexedOperand::Read {
                self.op(Op::I32Const(1));
            } else {
                self.op(Op::LocalGet(entry))
                    .op(Op::I32Load(mem(
                        records + offset_of!(IndexedReadEntry, tag),
                        2,
                    )))
                    .op(Op::I32Const(1))
                    .op(Op::I32ShrU)
                    .op(Op::I32Const(1))
                    .op(Op::I32And)
                    .op(Op::I32Const(2))
                    .op(Op::I32Or);
            }
            // A pair may just have advanced the clock twice. Leave the next
            // operation to the checked path when rebasing may be necessary.
            self.op(Op::I32Const(0))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Const(-5))
                .op(Op::I64LeU)
                .op(Op::Select)
                .op(Op::LocalSet(rights));
        }
        self.op(Op::LocalGet(entry)).op(Op::I32Load(mem(
            records + offset_of!(IndexedReadEntry, host_bias),
            2,
        )));
        if let Some((_, bias, _)) = self.ordered_operand.filter(|_| preserve_fetch) {
            self.op(Op::LocalTee(bias));
        }
        self.op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Add)
            .op(Op::Br(1))
            .op(Op::End)
            .op(Op::End);
    }
}
