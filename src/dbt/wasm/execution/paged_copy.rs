use super::{
    ADDRESS, Emitter, LHS, REMAINING, RESULT, RHS, RIP, STORE_STATUS, StructuredLoop, Unit, VALUE,
};
use crate::x86::context::Segment;
use crate::x86::instruction::{
    Address, AddressBase, Alu, DataOperand, Instruction, Location, Operation, RegisterView,
    VectorInstruction, VectorOperand, Width,
};
use wasm_encoder::{BlockType, Instruction as Op, MemArg, ValType};

#[derive(Clone, Copy)]
pub(super) struct CopyGroup {
    source: Address,
    destination: Address,
    register: u8,
    width: u32,
    end: u64,
    adjustment: Option<DestinationAdjustment>,
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
                    let group = Self::recognize_with_minimum_pairs(instructions, minimum_pairs)?;
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
        Self::recognize_with_minimum_pairs(lap, 2)
    }

    fn recognize_with_minimum_pairs(lap: &[Instruction], minimum_pairs: u32) -> Option<Self> {
        fn vector_move(instruction: Instruction, writing: bool) -> Option<(Address, u8)> {
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
                && address.size == Width::W64
                && address.base != AddressBase::NextPc)
                .then_some((address, register))
        }

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
            width: Width::W64,
        } = lea.operation
        else {
            return None;
        };
        let Operation::Alu {
            kind: Alu::Add,
            dst,
            src: DataOperand::Immediate(advance),
            width: Width::W64,
        } = advance.operation
        else {
            return None;
        };
        // Subsequent loads still use the preflighted source expression.
        (register == dst
            && address.size == Width::W64
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
    pub(super) fn emit_linear_scalar_copy(
        &mut self,
        instructions: &[Instruction],
        group: ScalarCopyGroup,
        loop_depth: u32,
        (read, write): (u32, u32),
    ) {
        self.guard_straight_prefix(
            &instructions[0],
            &instructions[group.instructions - 1],
            group.instructions as u32,
        );
        self.address(group.source, 0);
        self.op(Op::LocalSet(LHS));
        self.linear_copy_host(LHS, group.width, read);
        self.op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Empty));
        self.address(group.destination, 0);
        self.op(Op::LocalSet(RHS));
        self.linear_copy_host(RHS, group.width, write);
        self.op(Op::I64ExtendI32U)
            .op(Op::LocalTee(ADDRESS))
            .op(Op::I64Const(0))
            .op(Op::I64Ne)
            .op(Op::If(BlockType::Empty));
        self.emit_scalar_transfers(group);
        self.current_operation = instructions[group.instructions - 1].operation;
        self.stores = false;
        self.route(group.end, loop_depth + 3, loop_depth + 4);
        self.op(Op::End).op(Op::End).op(Op::End);
    }

    fn linear_copy_host(&mut self, address: u32, width: u32, resolve: u32) {
        use crate::memory::DirectSpan;

        self.op(Op::LocalGet(address))
            .op(Op::I32Const(width as i32))
            .op(Op::I32Const(Segment::Ds as i32))
            .op(Op::Call(resolve))
            .op(Op::I64ExtendI32U)
            .op(Op::LocalTee(RESULT))
            .op(Op::I32WrapI64)
            .op(Op::If(BlockType::Result(ValType::I32)))
            .op(Op::LocalGet(address))
            .op(Op::LocalGet(RESULT))
            .op(Op::I32WrapI64)
            .op(Op::I64Load(memory(
                std::mem::offset_of!(DirectSpan, base) as u64
            )))
            .op(Op::I64Sub)
            .op(Op::I32WrapI64)
            .op(Op::LocalGet(RESULT))
            .op(Op::I32WrapI64)
            .op(Op::I32Load(memory(
                std::mem::offset_of!(DirectSpan, host) as u64
            )))
            .op(Op::I32Add)
            .op(Op::Else)
            .op(Op::I32Const(0))
            .op(Op::End);
    }

    pub(super) fn emit_finite_scalar_copy(
        &mut self,
        instructions: &[Instruction],
        group: ScalarCopyGroup,
        loop_depth: u32,
        guard: u32,
        resolve: u32,
    ) {
        self.guard_straight_paged_prefix(
            &instructions[0],
            &instructions[group.instructions - 1],
            group.instructions as u32,
            guard,
            None,
        );
        self.address(group.source, 0);
        self.op(Op::LocalSet(LHS));
        self.address(group.destination, 0);
        self.op(Op::LocalSet(RHS));
        self.resolve_copy_window(group.width | crate::embedding::layout::COPY_SCALAR, resolve);
        self.op(Op::If(BlockType::Empty));
        self.copy_descriptor_hosts();
        self.emit_scalar_transfers(group);
        self.guarded_paged_lap = true;
        self.stores = false;
        self.route(group.end, loop_depth + 3, loop_depth + 4);
        self.guarded_paged_lap = false;
        self.op(Op::End).op(Op::End).op(Op::End);
    }

    pub(super) fn emit_finite_copy_prefix(
        &mut self,
        instructions: &[Instruction],
        group: CopyGroup,
        loop_depth: u32,
        guard: u32,
        resolve: u32,
    ) {
        let count = instructions.len();
        self.guard_straight_paged_prefix(
            &instructions[0],
            &instructions[count - 1],
            count as u32,
            guard,
            None,
        );
        self.address(group.source, 0);
        self.op(Op::LocalSet(LHS));
        self.address(group.destination, 0);
        self.op(Op::LocalSet(RHS));
        self.resolve_copy_window(group.width, resolve);
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

    pub(super) fn emit_paged_copy_group(&mut self, window: CopyWindow, exit_depth: u32) {
        let group = window.group;
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
        self.emit_ordered_copy(group);
    }

    fn resolve_copy_window(&mut self, width: u32, resolve: u32) {
        self.op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::I32Const(width as i32))
            .op(Op::Call(resolve))
            .op(Op::LocalTee(STORE_STATUS));
    }

    fn emit_scalar_transfers(&mut self, group: ScalarCopyGroup) {
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
        self.op(Op::I64Const(group.end as i64))
            .op(Op::LocalSet(RIP));
        self.retire_instructions(group.instructions as u32);
    }

    fn emit_ordered_copy(&mut self, group: CopyGroup) {
        let xmm = self.xmm(group.register);
        for offset in (0..group.width).step_by(16) {
            if offset == 0
                && let Some(adjustment) = group.adjustment
            {
                self.op(Op::LocalGet(STORE_STATUS))
                    .op(Op::V128Load(memory(0)))
                    .op(Op::LocalSet(xmm));
                self.lea(adjustment.register, adjustment.address, Width::W64, 0);
                self.integer(
                    Alu::Add,
                    adjustment.register,
                    DataOperand::Immediate(adjustment.advance),
                    Width::W64,
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
        self.op(Op::I64Const(group.end as i64))
            .op(Op::LocalSet(RIP))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(group.instruction_count())))
            .op(Op::I64Sub)
            .op(Op::LocalSet(REMAINING));
    }
}

fn memory(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 0,
        memory_index: 0,
    }
}
