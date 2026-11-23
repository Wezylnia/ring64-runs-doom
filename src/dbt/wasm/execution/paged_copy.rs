use super::paged_prefix::PagedPrefixFetch;
use super::{
    ADDRESS, Emitter, LHS, REMAINING, RHS, RIP, STORE_STATUS, StructuredLoop, Unit, VALUE,
    memory::SpanCache,
};
use crate::x86::context::Segment;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::x86::instruction::MemoryAluOperands;
use crate::x86::instruction::{
    Address, AddressBase, Alu, Condition, DataOperand, Instruction, Location, Operation,
    RegisterView, VectorInstruction, VectorOperand, Width,
};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use std::mem::offset_of;
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn emit_copy_access_cache(resolve: u32) -> wasm_encoder::Function {
    use crate::embedding::layout::{COPY_FILL_INDEX, RunState, StateStorage};
    use crate::x86::paging::cache::{
        CAPACITY, INDEXED_READ_ENTRIES, INDEXED_VALID, INDEXED_WRITE_READABLE, IndexedReadEntry,
        IndexedReadTable,
    };
    use std::mem::{offset_of, size_of};
    use wasm_encoder::{Function, ValType};

    let mut body = Function::new([(10, ValType::I32), (3, ValType::I64)]);
    let mut op = |op| {
        body.instruction(&op);
    };
    let (table, width, offset, record, source, destination) = (4, 5, 6, 7, 8, 9);
    let (read_slot, write_slot, fetch, scratch) = (10, 11, 12, 13);
    let (epoch, clock, initial_clock) = (14, 15, 16);
    let field = |offset: usize| memory(offset as u64);
    let ages = field(offset_of!(IndexedReadTable, ages));
    let clock_field = field(offset_of!(IndexedReadTable, clock));
    op(Op::Block(BlockType::Empty));
    op(Op::LocalGet(0));
    op(Op::I32Load(field(offset_of!(
        StateStorage,
        indexed_paged_reads
    ))));
    op(Op::LocalTee(table));
    op(Op::I32Eqz);
    op(Op::BrIf(0));
    op(Op::LocalGet(table));
    op(Op::I64Load(field(offset_of!(IndexedReadTable, epoch))));
    op(Op::LocalSet(epoch));
    op(Op::LocalGet(3));
    op(Op::I32Const(255));
    op(Op::I32And);
    op(Op::LocalSet(width));
    for (address, records, host, slot, write) in [
        (
            1,
            offset_of!(IndexedReadTable, entries),
            source,
            read_slot,
            false,
        ),
        (
            2,
            offset_of!(IndexedReadTable, writes),
            destination,
            write_slot,
            true,
        ),
    ] {
        op(Op::LocalGet(table));
        op(Op::LocalGet(address));
        op(Op::I32WrapI64);
        op(Op::I32Const(12));
        op(Op::I32ShrU);
        op(Op::I32Const(INDEXED_READ_ENTRIES as i32 - 1));
        op(Op::I32And);
        op(Op::I32Const(size_of::<IndexedReadEntry>() as i32));
        op(Op::I32Mul);
        op(Op::I32Add);
        op(Op::LocalTee(record));
        op(Op::I64Load(field(
            records + offset_of!(IndexedReadEntry, tag),
        )));
        if write {
            op(Op::I64Const(!(INDEXED_WRITE_READABLE as i64)));
            op(Op::I64And);
        }
        // Tagging the last byte checks width and page identity together.
        op(Op::LocalGet(address));
        op(Op::LocalGet(width));
        op(Op::I64ExtendI32U);
        op(Op::I64Add);
        op(Op::I64Const(1));
        op(Op::I64Sub);
        op(Op::I64Const(!4095));
        op(Op::I64And);
        op(Op::I64Const(INDEXED_VALID as i64));
        op(Op::I64Or);
        op(Op::LocalGet(epoch));
        op(Op::I64Or);
        op(Op::I64Ne);
        op(Op::BrIf(0));
        op(Op::LocalGet(record));
        op(Op::I32Load(field(
            records + offset_of!(IndexedReadEntry, host_bias),
        )));
        op(Op::LocalGet(address));
        op(Op::I32WrapI64);
        op(Op::I32Add);
        op(Op::LocalSet(host));
        op(Op::LocalGet(table));
        op(Op::LocalGet(record));
        op(Op::I32Load(field(
            records + offset_of!(IndexedReadEntry, slot_offset),
        )));
        op(Op::I32Add);
        op(Op::LocalSet(slot));
    }
    op(Op::LocalGet(table));
    op(Op::I32Load(field(offset_of!(IndexedReadTable, hint))));
    op(Op::LocalTee(fetch));
    op(Op::I32Const(CAPACITY as i32));
    op(Op::I32GeU);
    op(Op::BrIf(0));
    op(Op::LocalGet(table));
    op(Op::LocalGet(fetch));
    op(Op::I32Const(3));
    op(Op::I32Shl);
    op(Op::I32Add);
    op(Op::LocalSet(fetch));
    op(Op::LocalGet(table));
    op(Op::I64Load(clock_field));
    op(Op::LocalTee(clock));
    op(Op::LocalTee(initial_clock));
    op(Op::I64Const(-5));
    op(Op::I64GtU);
    op(Op::BrIf(0));
    op(Op::LocalGet(fetch));
    op(Op::I64Load(ages));
    op(Op::LocalGet(clock));
    op(Op::I64Ne);
    op(Op::BrIf(0));
    // Preserve the checked resolver's read, write, fetch order.
    for slot in [read_slot, write_slot] {
        op(Op::LocalGet(slot));
        op(Op::LocalGet(fetch));
        op(Op::I32Ne);
        op(Op::LocalGet(slot));
        op(Op::I64Load(ages));
        op(Op::LocalGet(clock));
        op(Op::I64Const(1));
        op(Op::I64Sub);
        op(Op::I64Ne);
        op(Op::I32And);
        op(Op::If(BlockType::Empty));
        op(Op::LocalGet(slot));
        op(Op::LocalGet(clock));
        op(Op::I64Const(1));
        op(Op::I64Add);
        op(Op::I64Store(ages));
        op(Op::LocalGet(fetch));
        op(Op::LocalGet(clock));
        op(Op::I64Const(2));
        op(Op::I64Add);
        op(Op::LocalTee(clock));
        op(Op::I64Store(ages));
        op(Op::End);
    }
    op(Op::LocalGet(clock));
    op(Op::LocalGet(initial_clock));
    op(Op::I64Ne);
    op(Op::If(BlockType::Empty));
    op(Op::LocalGet(table));
    op(Op::LocalGet(clock));
    op(Op::I64Store(clock_field));
    op(Op::End);
    op(Op::LocalGet(0));
    op(Op::I32Const(offset_of!(RunState, vector_scratch) as i32));
    op(Op::I32Add);
    op(Op::LocalSet(scratch));
    for (at, host) in [(0, source), (4, destination)] {
        op(Op::LocalGet(scratch));
        op(Op::LocalGet(host));
        op(Op::I32Store(memory(at)));
    }
    op(Op::LocalGet(scratch));
    op(Op::I32Const(4097));
    op(Op::LocalGet(width));
    op(Op::I32Sub);
    op(Op::LocalGet(1));
    op(Op::I32WrapI64);
    op(Op::I32Const(4095));
    op(Op::I32And);
    op(Op::LocalTee(offset));
    op(Op::LocalGet(2));
    op(Op::I32WrapI64);
    op(Op::I32Const(4095));
    op(Op::I32And);
    op(Op::LocalTee(record));
    op(Op::LocalGet(offset));
    op(Op::LocalGet(record));
    op(Op::I32GtU);
    op(Op::Select);
    op(Op::I32Sub);
    op(Op::I32Store(memory(8)));
    op(Op::LocalGet(scratch));
    op(Op::Return);
    op(Op::End);
    op(Op::LocalGet(1));
    op(Op::LocalGet(2));
    op(Op::LocalGet(3));
    op(Op::I32Const(COPY_FILL_INDEX as i32));
    op(Op::I32Or);
    op(Op::Call(resolve));
    op(Op::End);
    body
}

#[derive(Clone, Copy)]
pub(super) struct CopyGroup {
    source: Address,
    destination: Address,
    register: u8,
    second_register: Option<u8>,
    width: u32,
    end: u64,
    adjustment: Option<DestinationAdjustment>,
}

#[derive(Clone, Copy)]
pub(super) struct FlatVectorCopy {
    group: CopyGroup,
    tail: Option<CopyTail>,
}

#[derive(Clone, Copy)]
struct CopyTail {
    advance: RegisterView,
    step: u64,
    compare: RegisterView,
    limit: RegisterView,
    condition: Condition,
    next_pc: u64,
}

#[derive(Clone, Copy)]
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) struct PreloadedCopyTail {
    advance: RegisterView,
    step: u64,
    stack: Address,
    compare_pc: u64,
    next_pc: u64,
}

impl FlatVectorCopy {
    pub(super) fn instruction_count(self) -> u32 {
        self.group.instruction_count() + 3 * u32::from(self.tail.is_some())
    }

    pub(super) fn can_retain_window(self) -> bool {
        let Some(tail) = self.tail else {
            return false;
        };
        self.group.has_fixed_stride(tail)
    }

    pub(super) fn can_defer_flags(self) -> bool {
        self.tail.is_some_and(|tail| {
            matches!(
                tail.condition,
                Condition::Below
                    | Condition::AboveOrEqual
                    | Condition::Equal
                    | Condition::NotEqual
                    | Condition::BelowOrEqual
                    | Condition::Above
                    | Condition::Less
                    | Condition::GreaterOrEqual
            )
        })
    }
}

impl CopyTail {
    fn recognize(instructions: &[Instruction], head: u64, width: Width) -> Option<Self> {
        let [advance, compare, branch, ..] = instructions else {
            return None;
        };
        let Operation::Alu {
            kind: Alu::Add,
            dst: advance_register,
            src: DataOperand::Immediate(step),
            width: advance_width,
        } = advance.operation
        else {
            return None;
        };
        let Operation::Alu {
            kind: Alu::Cmp,
            dst: compare_register,
            src: DataOperand::Register(limit),
            width: compare_width,
        } = compare.operation
        else {
            return None;
        };
        let Operation::Jcc { condition, target } = branch.operation else {
            return None;
        };
        (target == head
            && advance_width == width
            && compare_width == width
            && !advance_register.high_byte
            && !compare_register.high_byte
            && !limit.high_byte)
            .then_some(Self {
                advance: advance_register,
                step,
                compare: compare_register,
                limit,
                condition,
                next_pc: branch.next_pc,
            })
    }
}

#[derive(Clone, Copy)]
struct DestinationAdjustment {
    register: RegisterView,
    address: Address,
    advance: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyRegister {
    General(RegisterView, Width),
    Vector(u8),
}

#[derive(Clone, Copy)]
struct ScalarTransfer {
    register: CopyRegister,
    width: Width,
}

#[derive(Clone, Copy)]
pub(super) struct ScalarCopyGroup {
    source: Address,
    destination: Address,
    transfers: [Option<ScalarTransfer>; 4],
    pub(super) instructions: usize,
    width: u32,
    end: u64,
}

fn separate_copy_register(register: RegisterView, address: Address) -> bool {
    address.base != AddressBase::Register(register.bank)
        && address
            .index
            .is_none_or(|index| index.register != register.bank)
}

fn follows_address(base: Address, address: Address, offset: u32) -> bool {
    base.displacement
        .checked_add(i64::from(offset))
        .is_some_and(|displacement| {
            address
                == Address {
                    displacement,
                    ..base
                }
        })
}

impl ScalarCopyGroup {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn following_jump(self, unit: &Unit, head: usize) -> Option<u64> {
        let first = unit.instructions.get(head)?.pc;
        let jump = unit.instructions.get(head + self.instructions)?;
        let Operation::Jump { target } = jump.operation else {
            return None;
        };
        let length = u32::try_from(jump.next_pc.checked_sub(first)?).ok()?;
        (jump.pc == self.end
            && first >> 12 == target >> 12
            && unit.context.single_page_code_span(first, length)
            && unit.fetch_proof.physical_span(first, length).is_some())
        .then_some(target)
    }

    pub(super) fn finite_prefix(
        unit: &Unit,
        loops: [Option<StructuredLoop>; 2],
    ) -> Option<(usize, Self)> {
        let size = if unit.context.is_long64() {
            Width::W64
        } else if unit.context.flat_paged_write32() {
            Width::W32
        } else {
            return None;
        };
        unit.instructions
            .iter()
            .enumerate()
            .filter_map(|(head, first)| {
                let group = Self::recognize(&unit.instructions[head..], size)?;
                // Flat scalar accesses already use direct spans. A group earns
                // its two span probes by replacing multiple checked XMM moves.
                if size == Width::W32
                    && group
                        .transfers
                        .iter()
                        .flatten()
                        .filter(|step| matches!(step.register, CopyRegister::Vector(_)))
                        .take(2)
                        .count()
                        != 2
                {
                    return None;
                }
                let end = head + group.instructions;
                let length = u32::try_from(group.end.checked_sub(first.pc)?).ok()?;
                if !unit.context.single_page_code_span(first.pc, length)
                    || loops.into_iter().flatten().any(|body| {
                        (head..end).any(|index| body.order().position(index as u32).is_some())
                    })
                {
                    return None;
                }
                Some((head, group))
            })
            .max_by_key(|(head, group)| (group.width, group.instructions, std::cmp::Reverse(*head)))
    }

    fn recognize(instructions: &[Instruction], size: Width) -> Option<Self> {
        fn read(instruction: Instruction) -> Option<(Address, Width, CopyRegister)> {
            Some(match instruction.operation {
                Operation::Load {
                    dst,
                    address,
                    width,
                } => (address, width, CopyRegister::General(dst, width)),
                Operation::Extend {
                    dst,
                    src: Location::Memory(address),
                    source_width,
                    width,
                    signed: false,
                } => (address, source_width, CopyRegister::General(dst, width)),
                Operation::Vector(VectorInstruction::Move {
                    operand: VectorOperand::Memory(address),
                    register,
                    bytes: 8,
                    store: false,
                    aligned: false,
                    preserve_upper: false,
                    ..
                }) => (address, Width::W64, CopyRegister::Vector(register)),
                _ => return None,
            })
        }
        fn transfer(
            pair: &[Instruction],
            size: Width,
        ) -> Option<(Address, Address, ScalarTransfer)> {
            let (source, width, register) = read(*pair.first()?)?;
            let destination = match (pair.get(1)?.operation, register) {
                (
                    Operation::Store {
                        src: DataOperand::Register(src),
                        address,
                        width: stored,
                    },
                    CopyRegister::General(dst, _),
                ) if src == dst
                    && stored == width
                    && separate_copy_register(dst, source)
                    && separate_copy_register(dst, address) =>
                {
                    address
                }
                (
                    Operation::Vector(VectorInstruction::Move {
                        operand: VectorOperand::Memory(address),
                        register: dst,
                        bytes: 8,
                        store: true,
                        aligned: false,
                        ..
                    }),
                    CopyRegister::Vector(src),
                ) if src == dst => address,
                _ => return None,
            };
            [source, destination]
                .iter()
                .all(|address| {
                    address.size == size
                        && address.segment == Segment::Ds
                        && address.base != AddressBase::NextPc
                })
                .then_some((source, destination, ScalarTransfer { register, width }))
        }

        let (source, destination, _) = transfer(instructions, size)?;
        let mut transfers = [None; 4];
        let mut width = 0;
        let mut count = 0;
        let mut end = instructions[0].pc;
        for pair in instructions.chunks_exact(2).take(transfers.len()) {
            let Some((from, to, step)) = transfer(pair, size) else {
                break;
            };
            if pair[0].pc != end
                || pair[0].next_pc != pair[1].pc
                || !follows_address(source, from, width)
                || !follows_address(destination, to, width)
            {
                break;
            }
            transfers[count] = Some(step);
            count += 1;
            width += u32::from(step.width.bytes());
            end = pair[1].next_pc;
        }
        (count >= 2).then_some(Self {
            source,
            destination,
            transfers,
            instructions: count * 2,
            width,
            end,
        })
    }
}

impl CopyGroup {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn preloaded_tail(
        self,
        instructions: &[Instruction],
        unit: &Unit,
    ) -> Option<PreloadedCopyTail> {
        // The retained stack span does not perform the user-mode CMP's alignment check.
        if unit.context.cpl() != 0 {
            return None;
        }
        let [first, _, _, _, advance, compare, branch] = instructions else {
            return None;
        };
        let Operation::Alu {
            kind: Alu::Add,
            dst: register,
            src: DataOperand::Immediate(step),
            width: Width::W64,
        } = advance.operation
        else {
            return None;
        };
        let Operation::MemoryAlu {
            kind: Alu::Cmp,
            width: Width::W64,
            operands: MemoryAluOperands::MemoryFirst(DataOperand::Register(rhs)),
            address: stack,
        } = compare.operation
        else {
            return None;
        };
        let Operation::Jcc {
            condition: Condition::NotEqual,
            target,
        } = branch.operation
        else {
            return None;
        };
        let stride = |address: Address| {
            let base = u64::from(address.base == AddressBase::Register(register.bank));
            let index = address
                .index
                .filter(|index| index.register == register.bank);
            step.checked_mul(base + index.map_or(0, |index| u64::from(index.scale)))
        };
        let bytes = u32::try_from(branch.next_pc.checked_sub(first.pc)?).ok()?;
        (self.second_register.is_some()
            && register.bank != 4
            && rhs == register
            && stack.size == Width::W64
            && matches!(stack.segment, Segment::Ss | Segment::Ds)
            && stack.base == AddressBase::Register(4)
            && stack.index.is_none()
            && stride(self.source) == Some(32)
            && stride(self.destination) == Some(32)
            && target == first.pc
            && instructions
                .windows(2)
                .all(|pair| pair[0].next_pc == pair[1].pc)
            && unit.context.single_page_code_span(first.pc, bytes)
            && unit.fetch_proof.physical_span(first.pc, bytes).is_some())
        .then_some(PreloadedCopyTail {
            advance: register,
            step,
            stack,
            compare_pc: compare.pc,
            next_pc: branch.next_pc,
        })
    }

    fn has_fixed_stride(self, tail: CopyTail) -> bool {
        if self.adjustment.is_some_and(|adjustment| {
            adjustment.register.bank == tail.advance.bank
                || !separate_copy_register(adjustment.register, self.destination)
        }) {
            return false;
        }
        let stride = |address: Address| {
            let base = u64::from(address.base == AddressBase::Register(tail.advance.bank));
            let index = address
                .index
                .filter(|index| index.register == tail.advance.bank);
            tail.step * (base + index.map_or(0, |index| u64::from(index.scale)))
        };
        stride(self.source) == u64::from(self.width)
            && stride(self.destination) == u64::from(self.width)
    }

    fn counts_to_destination_limit(self, tail: CopyTail) -> bool {
        let width = u64::from(self.width);
        if tail.condition != Condition::Below || tail.step != width || tail.limit == tail.advance {
            return false;
        }
        if let Some(adjustment) = self.adjustment {
            adjustment.register == tail.compare
                && adjustment.register != tail.limit
                && adjustment.advance == width
                && self.destination.displacement == adjustment.address.displacement
        } else {
            tail.compare == tail.advance
                && self.destination.base == AddressBase::Register(tail.advance.bank)
                && self.destination.index.is_none()
                && self.destination.displacement == 0
        }
    }

    pub(super) fn finite_prefix(
        unit: &Unit,
        loops: [Option<StructuredLoop>; 2],
    ) -> Option<(usize, Self)> {
        if !unit.context.is_long64() {
            return None;
        }
        [2, 1].into_iter().find_map(|minimum_pairs| {
            let guard_count = if minimum_pairs == 1 { 3 } else { 4 };
            unit.instructions
                .windows(guard_count)
                .enumerate()
                .find_map(|(head, instructions)| {
                    let group = Self::recognize_with_minimum_pairs(
                        instructions,
                        minimum_pairs,
                        Width::W64,
                    )?;
                    let start = instructions[0].pc;
                    let end = instructions[guard_count - 1].next_pc;
                    let length = u32::try_from(end.checked_sub(start)?).ok()?;
                    if instructions
                        .windows(2)
                        .any(|pair| pair[0].next_pc != pair[1].pc)
                        || unit.fetch_proof.physical_span(start, length).is_none()
                        || loops.into_iter().flatten().any(|body| {
                            (head..head + guard_count)
                                .any(|index| body.order().position(index as u32).is_some())
                        })
                    {
                        return None;
                    }
                    Some((head, group))
                })
        })
    }

    pub(super) fn recognize(lap: &[Instruction]) -> Option<Self> {
        Self::recognize_with_minimum_pairs(lap, 2, Width::W64)
    }

    pub(super) fn preloaded_prefix(
        unit: &Unit,
        loops: [Option<StructuredLoop>; 2],
    ) -> Option<(usize, Self)> {
        if !unit.context.is_long64() {
            return None;
        }
        unit.instructions
            .windows(4)
            .enumerate()
            .find_map(|(head, body)| {
                let (source, register) = Self::vector_move(body[0], false, Width::W64)?;
                let (next_source, second) = Self::vector_move(body[1], false, Width::W64)?;
                let (destination, stored) = Self::vector_move(body[2], true, Width::W64)?;
                let (next_destination, next_stored) = Self::vector_move(body[3], true, Width::W64)?;
                let start = body[0].pc;
                let end = body[3].next_pc;
                let length = u32::try_from(end.checked_sub(start)?).ok()?;
                (register != second
                    && stored == register
                    && next_stored == second
                    && follows_address(source, next_source, 16)
                    && follows_address(destination, next_destination, 16)
                    && body.windows(2).all(|pair| pair[0].next_pc == pair[1].pc)
                    && unit.context.single_page_code_span(start, length)
                    && unit.fetch_proof.physical_span(start, length).is_some()
                    && !loops.into_iter().flatten().any(|loop_body| {
                        (head..head + body.len())
                            .any(|index| loop_body.order().position(index as u32).is_some())
                    }))
                .then_some((
                    head,
                    Self {
                        source,
                        destination,
                        register,
                        second_register: Some(second),
                        width: 32,
                        end,
                        adjustment: None,
                    },
                ))
            })
    }

    pub(super) fn flat_prefixes(
        unit: &Unit,
        body: Option<StructuredLoop>,
    ) -> Vec<(usize, FlatVectorCopy)> {
        if !unit.context.flat_paged_write32() {
            return Vec::new();
        }
        let instructions = &unit.instructions;
        let mut prefixes = Vec::new();
        let mut next = 0;
        for (head, first) in instructions.iter().enumerate() {
            if head < next {
                continue;
            }
            let tail = &instructions[head..];
            let Some(group) = [4, 6].into_iter().find_map(|len| {
                Self::recognize_with_minimum_pairs(tail.get(..len)?, 2, Width::W32)
            }) else {
                continue;
            };
            let tail = CopyTail::recognize(
                &tail[group.instruction_count() as usize..],
                first.pc,
                Width::W32,
            );
            if !unit.sampled && tail.is_none() {
                continue;
            }
            let copy = FlatVectorCopy { group, tail };
            let group_end = head + copy.instruction_count() as usize;
            let end = tail.map_or(group.end, |tail| tail.next_pc);
            let bytes = end.saturating_sub(first.pc);
            if bytes == 0
                || bytes > 4096
                || !unit.context.single_page_code_span(first.pc, bytes as u32)
                || instructions[head..group_end]
                    .windows(2)
                    .any(|pair| pair[0].next_pc != pair[1].pc)
                || body.is_some_and(|body| {
                    (head..group_end).any(|index| body.order().position(index as u32).is_some())
                })
            {
                continue;
            }
            prefixes.push((head, copy));
            next = group_end;
            if prefixes.len() == 2 {
                break;
            }
        }
        prefixes
    }

    fn vector_move(instruction: Instruction, writing: bool, size: Width) -> Option<(Address, u8)> {
        let Operation::Vector(VectorInstruction::Move {
            operand: VectorOperand::Memory(address),
            register,
            bytes: 16,
            aligned: false,
            store,
            ..
        }) = instruction.operation
        else {
            return None;
        };
        (store == writing
            && address.segment == Segment::Ds
            && address.size == size
            && address.base != AddressBase::NextPc)
            .then_some((address, register))
    }

    fn recognize_with_minimum_pairs(
        lap: &[Instruction],
        minimum_pairs: u32,
        size: Width,
    ) -> Option<Self> {
        let vector_move = |instruction, writing| Self::vector_move(instruction, writing, size);
        let (source, register) = vector_move(*lap.first()?, false)?;
        let (first_store, adjustment) = if vector_move(*lap.get(1)?, true).is_some() {
            (1, None)
        } else {
            (
                3,
                Some(DestinationAdjustment::recognize(
                    source,
                    lap.get(1)?,
                    lap.get(2)?,
                )?),
            )
        };
        let (destination_register, target) = vector_move(*lap.get(first_store)?, true)?;
        if target != register {
            return None;
        }
        let destination = match adjustment {
            Some(adjustment) => adjustment.resolve(destination_register)?,
            None => destination_register,
        };
        let consecutive = |base: Address, address: Address, offset: i64| {
            base.displacement
                .checked_add(offset)
                .is_some_and(|displacement| {
                    address
                        == Address {
                            displacement,
                            ..base
                        }
                })
        };
        let mut pairs = 1;
        for pair in lap[first_store + 1..].chunks_exact(2).take(3) {
            let offset = i64::from(pairs) * 16;
            if !vector_move(pair[0], false).is_some_and(|(address, bank)| {
                bank == register && consecutive(source, address, offset)
            }) || !vector_move(pair[1], true).is_some_and(|(address, bank)| {
                bank == register && consecutive(destination_register, address, offset)
            }) {
                break;
            }
            pairs += 1;
        }
        let instructions = (pairs * 2) as usize + first_store - 1;
        if pairs < minimum_pairs
            || lap[instructions..]
                .iter()
                .any(|i| matches!(i.operation, Operation::Vector(_)))
        {
            return None;
        }
        Some(Self {
            source,
            destination,
            register,
            second_register: None,
            width: pairs * 16,
            end: lap[instructions - 1].next_pc,
            adjustment,
        })
    }

    pub(super) fn instruction_count(self) -> u32 {
        self.width / 8 + if self.adjustment.is_some() { 2 } else { 0 }
    }

    pub(super) fn prefix_guard_count(self) -> usize {
        // Admission advances fetch recency past the store. Keep the successor
        // inside the guard for a single pair without retiring it here.
        if self.width == 16 {
            3
        } else {
            self.instruction_count() as usize
        }
    }
}

impl DestinationAdjustment {
    fn recognize(source: Address, lea: &Instruction, advance: &Instruction) -> Option<Self> {
        let Operation::Lea {
            dst: register,
            address,
            width,
        } = lea.operation
        else {
            return None;
        };
        let Operation::Alu {
            kind: Alu::Add,
            dst,
            src: DataOperand::Immediate(advance),
            width: add_width,
        } = advance.operation
        else {
            return None;
        };
        // Subsequent loads still use the preflighted source expression.
        (register == dst
            && width == source.size
            && add_width == width
            && address.size == width
            && address.base != AddressBase::NextPc
            && source.base != AddressBase::Register(register.bank)
            && source
                .index
                .is_none_or(|index| index.register != register.bank))
        .then_some(Self {
            register,
            address,
            advance,
        })
    }

    fn resolve(self, destination: Address) -> Option<Address> {
        if destination.base != AddressBase::Register(self.register.bank)
            || destination.index.is_some()
        {
            return None;
        }
        Some(Address {
            displacement: self
                .address
                .displacement
                .checked_add(self.advance as i64)?
                .checked_add(destination.displacement)?,
            segment: Segment::Ds,
            ..self.address
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct CopyWindow {
    pub(super) group: CopyGroup,
    pub(super) resolve: u32,
    pub(super) source_base: u32,
    pub(super) destination_base: u32,
    pub(super) limit: u32,
    pub(super) source_host: u32,
    pub(super) destination_host: u32,
}

impl Emitter {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn emit_preloaded_copy_loop(
        &mut self,
        instructions: &[Instruction],
        group: CopyGroup,
        tail: PreloadedCopyTail,
        loop_depth: u32,
        [guard, resolve, read]: [u32; 3],
        [more, source, destination, stack]: [u32; 4],
    ) {
        let head = instructions[0].pc;
        self.guard_straight_paged_prefix(
            instructions,
            guard,
            None,
            self.paged_copy_access_cache.is_some(),
            PagedPrefixFetch::RetainedSse,
            false,
        );
        self.address(group.source, 0);
        self.op(Op::LocalSet(LHS));
        self.address(group.destination, 0);
        self.op(Op::LocalSet(RHS));
        self.resolve_copy_window(32 | crate::embedding::layout::COPY_NO_SPLIT, resolve);
        self.op(Op::If(BlockType::Empty));
        for (offset, local) in [(0, source), (4, destination)] {
            self.op(Op::LocalGet(STORE_STATUS))
                .op(Op::I32Load(memory(offset)))
                .op(Op::LocalSet(local));
        }
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(memory(8)))
            .op(Op::I32Const(1))
            .op(Op::I32Sub)
            .op(Op::I32Const(5))
            .op(Op::I32ShrU)
            .op(Op::LocalSet(more))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(stack))
            .op(Op::Loop(BlockType::Empty));
        let depth = loop_depth + 4;
        self.guarded_paged_lap = true;
        self.stores = false;
        self.op(Op::LocalGet(source))
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::LocalGet(destination))
            .op(Op::I64ExtendI32U)
            .op(Op::LocalSet(ADDRESS));
        self.copy_ordered_vectors(group);
        self.integer(
            Alu::Add,
            tail.advance,
            DataOperand::Immediate(tail.step),
            Width::W64,
            None,
        );

        self.op(Op::LocalGet(stack))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.address(tail.stack, 0);
        self.op(Op::I32Const(8))
            .op(Op::I32Const(Segment::Ds as i32))
            .op(Op::Call(read))
            .op(Op::LocalTee(stack))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty))
            .op(Op::I64Const(tail.compare_pc as i64))
            .op(Op::LocalSet(RIP));
        self.retire_instructions(5);
        self.route(tail.compare_pc, depth + 2, depth + 3);
        self.op(Op::End)
            .op(Op::LocalGet(stack))
            .op(Op::I32Load(memory(
                offset_of!(crate::memory::DirectSpan, host) as u64,
            )))
            .op(Op::LocalSet(stack))
            .op(Op::End);

        // Load after the stores even when the stack aliases their backing.
        self.op(Op::LocalGet(stack))
            .op(Op::I64Load(memory(0)))
            .op(Op::LocalSet(LHS));
        self.data_register(tail.advance, Width::W64);
        self.op(Op::LocalSet(RHS));
        self.arithmetic_result(Alu::Cmp, Width::W64);
        self.arithmetic_flags(Alu::Cmp, Width::W64);
        self.retire_instructions(7);
        self.condition(Condition::NotEqual);
        self.op(Op::If(BlockType::Empty))
            .op(Op::I64Const(head as i64))
            .op(Op::LocalSet(RIP))
            .op(Op::LocalGet(more))
            .op(Op::I32Eqz)
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(7))
            .op(Op::I64LtU)
            .op(Op::I32Or)
            .op(Op::If(BlockType::Empty));
        self.route(head, depth + 2, depth + 3);
        self.op(Op::End)
            .op(Op::LocalGet(more))
            .op(Op::I32Const(1))
            .op(Op::I32Sub)
            .op(Op::LocalSet(more));
        for local in [source, destination] {
            self.op(Op::LocalGet(local))
                .op(Op::I32Const(32))
                .op(Op::I32Add)
                .op(Op::LocalSet(local));
        }
        self.op(Op::Br(1))
            .op(Op::End)
            .op(Op::I64Const(tail.next_pc as i64))
            .op(Op::LocalSet(RIP));
        self.route(tail.next_pc, depth, depth + 1);
        self.guarded_paged_lap = false;
        self.op(Op::End).op(Op::End).op(Op::End).op(Op::End);
    }

    pub(super) fn emit_linear_scalar_copy(
        &mut self,
        instructions: &[Instruction],
        group: ScalarCopyGroup,
        loop_depth: u32,
        spans: (SpanCache, SpanCache),
    ) {
        self.guard_linear_copy(
            instructions,
            group.source,
            group.destination,
            group.width,
            spans,
        );
        self.emit_scalar_transfers(group, group.end, group.instructions as u32);
        self.current_operation = instructions[group.instructions - 1].operation;
        self.stores = false;
        self.route(group.end, loop_depth + 3, loop_depth + 4);
        self.op(Op::End).op(Op::End).op(Op::End);
    }

    pub(super) fn emit_linear_vector_copy(
        &mut self,
        instructions: &[Instruction],
        copy: FlatVectorCopy,
        loop_depth: u32,
        spans: (SpanCache, SpanCache),
        start_remaining: Option<u32>,
        window: Option<u32>,
    ) {
        let start_remaining = start_remaining.filter(|_| copy.can_defer_flags());
        if let Some((laps, start)) = window
            .filter(|_| copy.can_retain_window())
            .zip(start_remaining)
        {
            self.emit_batched_flat_copy(instructions, copy, loop_depth, spans, start, laps);
            return;
        }
        if let Some(start) = start_remaining {
            self.op(Op::LocalGet(REMAINING)).op(Op::LocalSet(start));
        }
        let FlatVectorCopy { group, tail } = copy;
        if tail.is_some() {
            self.op(Op::Loop(BlockType::Empty));
        }
        self.guard_linear_copy(
            instructions,
            group.source,
            group.destination,
            group.width,
            spans,
        );
        self.current_operation = instructions.last().unwrap().operation;
        self.stores = false;
        if let Some(tail) = tail {
            // Both spans and the complete lap passed admission. Only the final
            // comparison can publish flags or affect the next branch.
            self.emit_flat_copy_lap(
                group,
                tail,
                copy.instruction_count(),
                start_remaining.is_some(),
            );
            self.op(Op::I64Const(instructions[0].pc as i64))
                .op(Op::LocalSet(RIP));
            self.cloned_condition(
                tail.condition,
                start_remaining.map(|_| super::loop_cfg::ConditionSource::Operands(Width::W32)),
            );
            self.op(Op::BrIf(3));
            if start_remaining.is_some() {
                self.arithmetic_result(Alu::Cmp, Width::W32);
                self.arithmetic_flags(Alu::Cmp, Width::W32);
            }
            self.op(Op::I64Const(tail.next_pc as i64))
                .op(Op::LocalSet(RIP));
            self.route(tail.next_pc, loop_depth + 4, loop_depth + 5);
        } else {
            self.emit_ordered_copy(group);
            self.route(group.end, loop_depth + 3, loop_depth + 4);
        }
        self.op(Op::End).op(Op::End).op(Op::End);
        if let Some(tail) = tail {
            self.op(Op::End);
            if let Some(start) = start_remaining {
                self.restore_flat_copy_flags(tail, start);
            }
        }
    }

    fn restore_flat_copy_flags(&mut self, tail: CopyTail, start: u32) {
        // A refused guard leaves the previous lap's compare registers live.
        self.op(Op::LocalGet(REMAINING))
            .op(Op::LocalGet(start))
            .op(Op::I64Ne)
            .op(Op::If(BlockType::Empty));
        self.integer(
            Alu::Cmp,
            tail.compare,
            DataOperand::Register(tail.limit),
            Width::W32,
            None,
        );
        self.op(Op::End);
    }

    fn emit_batched_flat_copy(
        &mut self,
        instructions: &[Instruction],
        copy: FlatVectorCopy,
        loop_depth: u32,
        spans: (SpanCache, SpanCache),
        start: u32,
        laps: u32,
    ) {
        let FlatVectorCopy { group, tail } = copy;
        let tail = tail.expect("batched copy carries an advancing tail");
        let count = copy.instruction_count();
        let counted = copy.group.counts_to_destination_limit(tail);
        self.op(Op::LocalGet(REMAINING)).op(Op::LocalSet(start));
        self.op(Op::Loop(BlockType::Empty));
        self.guard_linear_copy(
            instructions,
            group.source,
            group.destination,
            group.width,
            spans,
        );
        if counted {
            self.op(Op::LocalGet(RHS)).op(Op::LocalSet(VALUE));
        }
        // Copy only the complete laps covered by both spans and the exact quota.
        // A stop inside the repeated instruction sequence rejects the guard.
        self.flat_copy_bytes_left(LHS, spans.0);
        self.op(Op::LocalSet(LHS));
        self.flat_copy_bytes_left(RHS, spans.1);
        self.op(Op::LocalSet(RHS));
        self.select_smaller_copy_bound();
        self.op(Op::I64Const(i64::from(group.width)))
            .op(Op::I64DivU)
            .op(Op::LocalSet(LHS))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(count)))
            .op(Op::I64DivU)
            .op(Op::LocalSet(RHS));
        self.select_smaller_copy_bound();
        self.op(Op::I32WrapI64).op(Op::LocalSet(laps));
        if counted {
            self.emit_counted_flat_copy(instructions, group, tail, count, loop_depth, laps);
            for _ in 0..4 {
                self.op(Op::End);
            }
            self.restore_flat_copy_flags(tail, start);
            return;
        }
        self.op(Op::Loop(BlockType::Empty));
        self.current_operation = instructions.last().unwrap().operation;
        self.stores = false;
        self.emit_flat_copy_lap(group, tail, count, true);
        self.cloned_condition(
            tail.condition,
            Some(super::loop_cfg::ConditionSource::Operands(Width::W32)),
        );
        self.op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Const(group.width as i32))
            .op(Op::I32Add)
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I64Const(i64::from(group.width)))
            .op(Op::I64Add)
            .op(Op::LocalSet(ADDRESS))
            .op(Op::LocalGet(laps))
            .op(Op::I32Const(1))
            .op(Op::I32Sub)
            .op(Op::LocalTee(laps))
            .op(Op::BrIf(1))
            .op(Op::I64Const(instructions[0].pc as i64))
            .op(Op::LocalSet(RIP))
            .op(Op::Br(5))
            .op(Op::End);
        self.arithmetic_result(Alu::Cmp, Width::W32);
        self.arithmetic_flags(Alu::Cmp, Width::W32);
        self.op(Op::I64Const(tail.next_pc as i64))
            .op(Op::LocalSet(RIP));
        self.route(tail.next_pc, loop_depth + 5, loop_depth + 6);
        for _ in 0..5 {
            self.op(Op::End);
        }
        self.restore_flat_copy_flags(tail, start);
    }

    fn emit_counted_flat_copy(
        &mut self,
        instructions: &[Instruction],
        group: CopyGroup,
        tail: CopyTail,
        count: u32,
        loop_depth: u32,
        laps: u32,
    ) {
        use wasm_encoder::ValType;

        let width = i64::from(group.width);
        // CMP observes the guest destination after its increment. Bound the
        // trip count by the first unsigned exit and by the last nonwrapping
        // update. If the next update wraps, replay the original entry.
        self.data_register(tail.limit, Width::W32);
        self.op(Op::LocalTee(RHS))
            .op(Op::LocalGet(VALUE))
            .op(Op::I64GtU)
            .op(Op::If(BlockType::Result(ValType::I64)))
            .op(Op::LocalGet(RHS))
            .op(Op::LocalGet(VALUE))
            .op(Op::I64Sub)
            .op(Op::I64Const(width - 1))
            .op(Op::I64Add)
            .op(Op::I64Const(width))
            .op(Op::I64DivU)
            .op(Op::Else)
            .op(Op::I64Const(1))
            .op(Op::End)
            .op(Op::LocalSet(RHS))
            .op(Op::I64Const(i64::from(u32::MAX)))
            .op(Op::LocalGet(VALUE))
            .op(Op::I64Sub)
            .op(Op::I64Const(width))
            .op(Op::I64DivU)
            .op(Op::LocalSet(LHS));
        self.select_smaller_copy_bound();
        self.op(Op::LocalSet(LHS))
            .op(Op::LocalGet(laps))
            .op(Op::I64ExtendI32U)
            .op(Op::LocalSet(RHS));
        self.select_smaller_copy_bound();
        self.op(Op::LocalTee(LHS))
            .op(Op::I64Eqz)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(LHS))
            .op(Op::I32WrapI64)
            .op(Op::LocalSet(laps))
            .op(Op::LocalGet(REMAINING))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Const(i64::from(count)))
            .op(Op::I64Mul)
            .op(Op::I64Sub)
            .op(Op::LocalSet(REMAINING))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Const(width))
            .op(Op::I64Mul)
            .op(Op::LocalSet(LHS))
            .op(Op::Loop(BlockType::Empty));
        self.copy_ordered_vectors(CopyGroup {
            adjustment: None,
            ..group
        });
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Const(group.width as i32))
            .op(Op::I32Add)
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I64Const(width))
            .op(Op::I64Add)
            .op(Op::LocalSet(ADDRESS))
            .op(Op::LocalGet(laps))
            .op(Op::I32Const(1))
            .op(Op::I32Sub)
            .op(Op::LocalTee(laps))
            .op(Op::BrIf(0))
            .op(Op::End);

        self.data_register(tail.advance, Width::W32);
        self.op(Op::LocalGet(LHS)).op(Op::I64Add);
        self.narrow(Width::W32);
        self.write_register(tail.advance, Width::W32);
        if let Some(adjustment) = group.adjustment {
            self.op(Op::LocalGet(VALUE))
                .op(Op::LocalGet(LHS))
                .op(Op::I64Add);
            self.write_register(adjustment.register, Width::W32);
        }
        self.current_operation = instructions.last().unwrap().operation;
        self.stores = false;
        self.integer(
            Alu::Cmp,
            tail.compare,
            DataOperand::Register(tail.limit),
            Width::W32,
            None,
        );
        self.op(Op::I64Const(instructions[0].pc as i64))
            .op(Op::LocalSet(RIP));
        self.cloned_condition(
            tail.condition,
            Some(super::loop_cfg::ConditionSource::Operands(Width::W32)),
        );
        self.op(Op::BrIf(3))
            .op(Op::I64Const(tail.next_pc as i64))
            .op(Op::LocalSet(RIP));
        self.route(tail.next_pc, loop_depth + 4, loop_depth + 5);
    }

    fn emit_flat_copy_lap(
        &mut self,
        group: CopyGroup,
        tail: CopyTail,
        count: u32,
        defer_flags: bool,
    ) {
        let omit = self.omit_result_status;
        self.omit_result_status = true;
        self.copy_ordered_vectors(group);
        self.integer(
            Alu::Add,
            tail.advance,
            DataOperand::Immediate(tail.step),
            Width::W32,
            None,
        );
        self.omit_result_status = defer_flags;
        self.integer(
            Alu::Cmp,
            tail.compare,
            DataOperand::Register(tail.limit),
            Width::W32,
            None,
        );
        self.omit_result_status = omit;
        self.retire_instructions(count);
    }

    fn select_smaller_copy_bound(&mut self) {
        self.op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::I64LtU)
            .op(Op::Select);
    }

    fn guard_linear_copy(
        &mut self,
        instructions: &[Instruction],
        source: Address,
        destination: Address,
        bytes: u32,
        (read, write): (SpanCache, SpanCache),
    ) {
        self.guard_straight_prefix(
            &instructions[0],
            instructions.last().unwrap(),
            instructions.len() as u32,
        );
        self.address(source, 0);
        self.op(Op::LocalSet(LHS));
        self.flat_copy_span_host(LHS, bytes, read);
        self.op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Empty));
        self.address(destination, 0);
        self.op(Op::LocalSet(RHS));
        self.flat_copy_span_host(RHS, bytes, write);
        self.op(Op::I64ExtendI32U)
            .op(Op::LocalTee(ADDRESS))
            .op(Op::I64Const(0))
            .op(Op::I64Ne)
            .op(Op::If(BlockType::Empty));
    }

    pub(super) fn emit_finite_scalar_copy(
        &mut self,
        instructions: &[Instruction],
        group: ScalarCopyGroup,
        loop_depth: u32,
        guard: u32,
        resolve: u32,
        jump: Option<u64>,
    ) {
        self.guard_straight_paged_prefix(
            instructions,
            guard,
            None,
            self.paged_copy_access_cache.is_some(),
            PagedPrefixFetch::RetainedSse,
            jump.is_some(),
        );
        self.address(group.source, 0);
        self.op(Op::LocalSet(LHS));
        self.address(group.destination, 0);
        self.op(Op::LocalSet(RHS));
        // Four nibbles preserve the original load/store widths on a split page.
        let widths = group.transfers.into_iter().flatten().enumerate().fold(
            0,
            |encoded, (index, transfer)| {
                encoded | (u32::from(transfer.width.bytes()) << (8 + 4 * index))
            },
        );
        self.resolve_copy_window(
            group.width | widths | crate::embedding::layout::COPY_SCALAR,
            resolve,
        );
        self.op(Op::If(BlockType::Empty));
        self.copy_descriptor_hosts();
        let target = jump.unwrap_or(group.end);
        self.emit_scalar_transfers(group, target, instructions.len() as u32);
        self.guarded_paged_lap = true;
        self.stores = false;
        self.route(target, loop_depth + 3, loop_depth + 4);
        self.guarded_paged_lap = false;
        self.op(Op::End).op(Op::End).op(Op::End);
        if jump.is_some() {
            // A refused copy enters checked execution without the source-page proof.
            self.clear_quiet_fetch();
        }
    }

    pub(super) fn emit_finite_copy_prefix(
        &mut self,
        instructions: &[Instruction],
        group: CopyGroup,
        loop_depth: u32,
        guard: u32,
        resolve: u32,
    ) {
        self.guard_straight_paged_prefix(
            instructions,
            guard,
            None,
            self.paged_copy_access_cache.is_some(),
            PagedPrefixFetch::RetainedSse,
            false,
        );
        self.address(group.source, 0);
        self.op(Op::LocalSet(LHS));
        self.address(group.destination, 0);
        self.op(Op::LocalSet(RHS));
        let width = if group.second_register.is_some() {
            group.width | crate::embedding::layout::COPY_NO_SPLIT
        } else {
            group.width
        };
        self.resolve_copy_window(width, resolve);
        self.op(Op::If(BlockType::Empty));
        self.copy_descriptor_hosts();
        self.emit_ordered_copy(group);
        self.guarded_paged_lap = true;
        self.stores = false;
        self.route(group.end, loop_depth + 3, loop_depth + 4);
        self.guarded_paged_lap = false;
        self.op(Op::End).op(Op::End).op(Op::End);
    }

    fn copy_descriptor_hosts(&mut self) {
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(memory(4)))
            .op(Op::I64ExtendI32U)
            .op(Op::LocalSet(ADDRESS))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(memory(0)))
            .op(Op::LocalSet(STORE_STATUS));
    }

    pub(super) fn emit_paged_copy_group(
        &mut self,
        window: CopyWindow,
        exit_depth: u32,
        instructions: &[Instruction],
        body: StructuredLoop,
    ) {
        let group = window.group;
        let count = body.max_lap_len;
        let tail = if body.paged_vector_copy()
            && !body.forward_edges()
            && count == group.instruction_count() + 3
        {
            let head = body.head as usize;
            CopyTail::recognize(
                &instructions[head + group.instruction_count() as usize..=body.tail as usize],
                instructions[head].pc,
                Width::W64,
            )
            .filter(|tail| {
                group.counts_to_destination_limit(*tail) && group.has_fixed_stride(*tail)
            })
        } else {
            None
        };
        self.address(group.source, 0);
        self.op(Op::LocalTee(LHS))
            .op(Op::LocalGet(window.source_base))
            .op(Op::I64Sub)
            .op(Op::LocalTee(VALUE))
            .op(Op::LocalGet(window.limit))
            .op(Op::I64LtU);
        self.address(group.destination, 0);
        self.op(Op::LocalTee(RHS))
            .op(Op::LocalGet(window.destination_base))
            .op(Op::I64Sub)
            .op(Op::LocalTee(ADDRESS))
            .op(Op::LocalGet(window.limit))
            .op(Op::I64LtU)
            .op(Op::I32And)
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.resolve_copy_window(group.width, window.resolve);
        self.op(Op::I32Eqz);
        self.retry_guarded_paged_operand(exit_depth + 1, true);
        self.op(Op::LocalGet(LHS))
            .op(Op::LocalSet(window.source_base))
            .op(Op::LocalGet(RHS))
            .op(Op::LocalSet(window.destination_base))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(memory(0)))
            .op(Op::LocalSet(window.source_host))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(memory(4)))
            .op(Op::LocalSet(window.destination_host))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(memory(8)))
            .op(Op::I64ExtendI32U)
            .op(Op::LocalSet(window.limit))
            .op(Op::I64Const(0))
            .op(Op::LocalSet(VALUE))
            .op(Op::I64Const(0))
            .op(Op::LocalSet(ADDRESS))
            .op(Op::End);

        if tail.is_some() {
            // Both offsets are inside the current descriptor. Reserve its last
            // reachable lap for the original group and branch tail.
            self.op(Op::LocalGet(window.limit))
                .op(Op::I64Const(1))
                .op(Op::I64Sub)
                .op(Op::LocalGet(VALUE))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::LocalGet(VALUE))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::I64GtU)
                .op(Op::Select)
                .op(Op::I64Sub)
                .op(Op::I64Const(i64::from(group.width)))
                .op(Op::I64DivU)
                .op(Op::LocalSet(LHS));
        }
        self.op(Op::LocalGet(VALUE))
            .op(Op::I32WrapI64)
            .op(Op::LocalGet(window.source_host))
            .op(Op::I32Add)
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::LocalGet(window.destination_host))
            .op(Op::I32Add)
            .op(Op::I64ExtendI32U)
            .op(Op::LocalSet(ADDRESS));
        if let Some(tail) = tail {
            self.emit_paged_copy_extra_laps(group, tail, count);
        }
        self.emit_ordered_copy(group);
    }

    fn emit_paged_copy_extra_laps(&mut self, group: CopyGroup, tail: CopyTail, count: u32) {
        let width = i64::from(group.width);
        self.op(Op::Block(BlockType::Empty))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Eqz)
            .op(Op::BrIf(0));
        self.data_register(tail.limit, Width::W64);
        self.op(Op::LocalTee(VALUE))
            .op(Op::LocalGet(RHS))
            .op(Op::I64LeU)
            .op(Op::BrIf(0))
            // After every skipped latch, the proved next destination must
            // still be strictly below the unsigned comparison limit.
            .op(Op::LocalGet(VALUE))
            .op(Op::LocalGet(RHS))
            .op(Op::I64Sub)
            .op(Op::I64Const(1))
            .op(Op::I64Sub)
            .op(Op::I64Const(width))
            .op(Op::I64DivU)
            .op(Op::LocalSet(RHS));
        self.select_smaller_copy_bound();
        self.op(Op::LocalSet(LHS))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(count)))
            .op(Op::I64DivU)
            .op(Op::I64Const(1))
            .op(Op::I64Sub)
            .op(Op::LocalSet(RHS));
        self.select_smaller_copy_bound();
        self.op(Op::LocalTee(LHS))
            .op(Op::I64Eqz)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(REMAINING))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Const(i64::from(count)))
            .op(Op::I64Mul)
            .op(Op::I64Sub)
            .op(Op::LocalSet(REMAINING))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Const(width))
            .op(Op::I64Mul)
            .op(Op::LocalSet(RHS))
            .op(Op::Loop(BlockType::Empty));
        self.copy_ordered_vectors(CopyGroup {
            adjustment: None,
            ..group
        });
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Const(group.width as i32))
            .op(Op::I32Add)
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I64Const(width))
            .op(Op::I64Add)
            .op(Op::LocalSet(ADDRESS))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Const(1))
            .op(Op::I64Sub)
            .op(Op::LocalTee(LHS))
            .op(Op::I32WrapI64)
            .op(Op::BrIf(0))
            .op(Op::End);
        // The final ordinary lap rebuilds the destination adjustment and
        // publishes all flags. The intermediate copies cannot exit or fault.
        self.data_register(tail.advance, Width::W64);
        self.op(Op::LocalGet(RHS)).op(Op::I64Add);
        self.write_register(tail.advance, Width::W64);
        self.op(Op::End);
    }

    fn resolve_copy_window(&mut self, width: u32, resolve: u32) {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let resolve = if let Some(cached) = self.paged_copy_access_cache {
            self.op(Op::LocalGet(0));
            cached
        } else {
            resolve
        };
        self.op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::I32Const(width as i32))
            .op(Op::Call(resolve))
            .op(Op::LocalTee(STORE_STATUS));
    }

    fn emit_scalar_transfers(&mut self, group: ScalarCopyGroup, target: u64, count: u32) {
        let mut offset = 0;
        for movement in group.transfers.into_iter().flatten() {
            self.op(Op::LocalGet(STORE_STATUS));
            self.direct_scalar_load(movement.width, memory(offset));
            match movement.register {
                CopyRegister::General(register, width) => {
                    self.op(Op::LocalGet(VALUE));
                    self.write_register(register, width);
                }
                CopyRegister::Vector(register) => {
                    let xmm = self.xmm(register);
                    self.op(Op::V128Const(0))
                        .op(Op::LocalGet(VALUE))
                        .op(Op::I64x2ReplaceLane(0))
                        .op(Op::LocalSet(xmm));
                }
            }
            self.op(Op::LocalGet(ADDRESS))
                .op(Op::I32WrapI64)
                .op(Op::LocalGet(VALUE));
            self.direct_scalar_store(movement.width, memory(offset));
            offset += u64::from(movement.width.bytes());
        }
        self.op(Op::I64Const(target as i64)).op(Op::LocalSet(RIP));
        self.retire_instructions(count);
    }

    fn emit_ordered_copy(&mut self, group: CopyGroup) {
        self.copy_ordered_vectors(group);
        self.op(Op::I64Const(group.end as i64))
            .op(Op::LocalSet(RIP))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(group.instruction_count())))
            .op(Op::I64Sub)
            .op(Op::LocalSet(REMAINING));
    }

    fn copy_ordered_vectors(&mut self, group: CopyGroup) {
        let xmm = self.xmm(group.register);
        if let Some(second) = group.second_register {
            let second = self.xmm(second);
            // Both guest loads must complete before either aliased store.
            for (offset, bank) in [(0, xmm), (16, second)] {
                self.op(Op::LocalGet(STORE_STATUS))
                    .op(Op::V128Load(memory(offset)))
                    .op(Op::LocalSet(bank));
            }
            for (offset, bank) in [(0, xmm), (16, second)] {
                self.op(Op::LocalGet(ADDRESS))
                    .op(Op::I32WrapI64)
                    .op(Op::LocalGet(bank))
                    .op(Op::V128Store(memory(offset)));
            }
            return;
        }
        for offset in (0..group.width).step_by(16) {
            if offset == 0
                && let Some(adjustment) = group.adjustment
            {
                self.op(Op::LocalGet(STORE_STATUS))
                    .op(Op::V128Load(memory(0)))
                    .op(Op::LocalSet(xmm));
                let width = adjustment.address.size;
                self.lea(adjustment.register, adjustment.address, width, 0);
                self.integer(
                    Alu::Add,
                    adjustment.register,
                    DataOperand::Immediate(adjustment.advance),
                    width,
                    None,
                );
                self.op(Op::LocalGet(ADDRESS))
                    .op(Op::I32WrapI64)
                    .op(Op::LocalGet(xmm))
                    .op(Op::V128Store(memory(0)));
                continue;
            }
            // A dependent later load sees the preceding store even on aliased physical backing.
            self.op(Op::LocalGet(ADDRESS))
                .op(Op::I32WrapI64)
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::V128Load(memory(u64::from(offset))));
            if offset + 16 == group.width {
                self.op(Op::LocalTee(xmm));
            }
            self.op(Op::V128Store(memory(u64::from(offset))));
        }
    }
}

fn memory(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 0,
        memory_index: 0,
    }
}
