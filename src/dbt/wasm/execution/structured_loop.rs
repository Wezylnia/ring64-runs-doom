use super::{
    Emitter, GPR, REASON, REMAINING,
    control::InstructionRetirement,
    loop_cfg::{CompareAction, LoopOrder, ReadFaultFlags},
};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::{
    RIP, STORE_STATUS,
    loop_cfg::{ConditionSource, ReadLoopDiamond},
};
use wasm_encoder::{BlockType, Instruction as Op};

use crate::x86::{
    context::Mode,
    float::{Format, Kind},
    instruction::{
        Address, Alu, Count, Instruction, Location, Operation, Shift, Unary, VectorInstruction,
        Width,
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoopKind {
    Float,
    Registers,
    IntegerReads,
    #[cfg(not(feature = "parallel"))]
    PagedReads,
    #[cfg(not(feature = "parallel"))]
    PagedScalarF32,
    ForwardEdges,
}

#[derive(Clone, Copy)]
pub(super) struct StructuredLoop {
    earlier: u32,
    pub(super) head: u32,
    pub(super) tail: u32,
    pub(super) max_lap_len: u32,
    kind: LoopKind,
    project_dwords: bool,
    pub(super) paged_fetch_bytes: Option<u32>,
    pub(super) paged_stack_writes: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_read_span: Option<super::read_loop::ReadLoop>,
}

impl StructuredLoop {
    pub(super) fn dword_writes(self, unit: &super::Unit) -> u16 {
        if !self.project_dwords || self.kind != LoopKind::Registers || unit.context.is_long64() {
            return 0;
        }
        if cfg!(feature = "parallel")
            && (!cfg!(target_arch = "wasm32")
                || unit.context.mode() != Mode::Protected
                || !unit.context.code32()
                // The only taken edge in the register body returns to head.
                || unit.instructions[self.head as usize].pc > unit.context.code_limit())
        {
            return 0;
        }
        let mut writes = 0;
        for instruction in &unit.instructions[self.head as usize..self.tail as usize] {
            let (dst, width) = match instruction.operation {
                Operation::Alu {
                    width: Width::W64, ..
                }
                | Operation::Lea {
                    address:
                        Address {
                            size: Width::W64, ..
                        },
                    ..
                } => return 0,
                Operation::Alu {
                    kind, dst, width, ..
                } if kind.writes_result() => (dst, width),
                Operation::Mov { dst, width, .. }
                | Operation::Lea { dst, width, .. }
                | Operation::Shift {
                    dst: Location::Register(dst),
                    width,
                    ..
                }
                | Operation::DoubleShift {
                    dst: Location::Register(dst),
                    width,
                    ..
                }
                | Operation::Unary {
                    dst: Location::Register(dst),
                    width,
                    ..
                } => (dst, width),
                _ => continue,
            };
            if width == Width::W64 {
                return 0;
            }
            if width == Width::W32 {
                writes |= 1 << dst.bank;
            }
        }
        writes
    }

    #[cfg(any(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn without_dword_projection(mut self) -> Self {
        self.project_dwords = false;
        self
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn read_proof(self, unit: &super::Unit) -> Option<super::read_loop::ReadLoop> {
        if self.precheck_gates_without_read_proof() {
            return None;
        }
        super::read_loop::ReadLoop::recognize(
            &unit.context,
            &unit.instructions[self.head as usize..=self.tail as usize],
        )
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn stack_proof(self, unit: &super::Unit) -> Option<super::stack_slots::StackSlots> {
        if !self.forward_edges() {
            return None;
        }
        super::stack_slots::StackSlots::recognize(
            &unit.context,
            &unit.instructions[self.earlier as usize..=self.tail as usize],
        )
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn indexed_read_address(self, unit: &super::Unit) -> Option<Address> {
        if self.kind != LoopKind::IntegerReads {
            return None;
        }
        super::read_loop::IndexedReadWindow::recognize(
            &unit.context,
            &unit.instructions[self.head as usize..=self.tail as usize],
        )
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn integer_read_proof(
        self,
        unit: &super::Unit,
    ) -> Option<super::read_loop::ReadLoop> {
        if self.kind != LoopKind::IntegerReads {
            return None;
        }
        super::read_loop::ReadLoop::recognize_integer(
            &unit.context,
            &unit.instructions[self.head as usize..=self.tail as usize],
        )
    }

    pub(super) fn precheck_gates_without_read_proof(self) -> bool {
        if cfg!(feature = "parallel") {
            matches!(
                self.kind,
                LoopKind::Float | LoopKind::Registers | LoopKind::IntegerReads
            ) || (cfg!(target_arch = "wasm32") && self.natural_forward_loop())
        } else {
            self.kind != LoopKind::Float
        }
    }

    pub(super) fn active_labels(self, ordinal: u32) -> u32 {
        if self.forward_edges() {
            let order = self.order();
            order.len() - 1 - order.position(ordinal).unwrap()
        } else {
            0
        }
    }

    pub(super) fn forward_edges(self) -> bool {
        self.kind == LoopKind::ForwardEdges
    }

    pub(super) fn natural_forward_loop(self) -> bool {
        self.forward_edges() && self.earlier == self.head
    }

    pub(super) fn order(self) -> LoopOrder {
        LoopOrder {
            earlier: self.earlier,
            head: self.head,
            tail: self.tail,
        }
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn select_paged_registers(instructions: &[Instruction]) -> Option<Self> {
        Self::select_body(instructions, LoopKind::Registers).map(Self::without_dword_projection)
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn select_paged_reads(unit: &super::Unit) -> Option<Self> {
        let long64 = unit.context.is_long64();
        if !long64 && !unit.context.flat_paged_data32() {
            return None;
        }
        let address_size = (!long64).then_some(Width::W32);
        let read = |operation| paged_read_width(operation, address_size).is_some();
        if !unit.instructions.iter().any(|i| read(i.operation)) {
            return None;
        }
        let scalar_read = |op| read(op) && (long64 || !matches!(op, Operation::MemoryAlu { .. }));
        let simple = Self::select_straight_body(
            &unit.instructions,
            LoopKind::PagedReads,
            |op| inside_register_body(op) || scalar_read(op),
            |body| {
                body.iter()
                    .filter(|i| scalar_read(i.operation))
                    .take(4)
                    .count()
                    == 4
            },
        );
        simple
            .or_else(|| {
                super::loop_cfg::select_paged_read_graph(&unit.instructions, |body| {
                    body.iter().any(|i| read(i.operation))
                        && body.iter().all(|i| match i.operation {
                            Operation::Jump { target } | Operation::Jcc { target, .. } => {
                                target <= unit.context.code_limit()
                            }
                            op => inside_register_body(op) || read(op),
                        })
                })
                .map(Self::forward)
            })
            .or_else(|| {
                if !long64
                    || !unit
                        .instructions
                        .iter()
                        .any(|i| paged_byte_store(i.operation, None))
                {
                    return None;
                }
                super::loop_cfg::select_paged_write_graph(&unit.instructions, |body| {
                    body.iter().any(|i| read(i.operation))
                        && body.iter().any(|i| paged_byte_store(i.operation, None))
                        && body.iter().all(|i| {
                            inside_paged_read_graph(i.operation)
                                || paged_byte_store(i.operation, None)
                                || paged_write_graph_register(i.operation)
                        })
                })
                .map(Self::forward)
            })
            .or_else(|| Self::select_paged_stack_writes(unit))?
            .without_dword_projection()
            .within_paged_fetch(unit)
    }

    #[cfg(not(feature = "parallel"))]
    fn select_paged_stack_writes(unit: &super::Unit) -> Option<Self> {
        use crate::memory::Access;

        let addr32 = Some(Width::W32);
        let byte_store = |op| paged_byte_store(op, addr32);
        if !unit.context.flat_paged_write32()
            || !unit
                .instructions
                .iter()
                .any(|i| paged_stack_dword(i.operation) == Some(Access::Write))
        {
            return None;
        }
        let graph = super::loop_cfg::select_paged_write_graph(&unit.instructions, |body| {
            body.iter().any(|i| byte_store(i.operation))
                && body
                    .iter()
                    .any(|i| paged_stack_dword(i.operation) == Some(Access::Write))
                && body.iter().all(|i| match i.operation {
                    Operation::Jump { target } | Operation::Jcc { target, .. } => {
                        target <= unit.context.code_limit()
                    }
                    op => {
                        inside_register_body(op)
                            || paged_write_graph_register(op)
                            || paged_read_width(op, addr32).is_some()
                            || byte_store(op)
                            || paged_stack_dword(op).is_some()
                    }
                })
        })?;
        let mut body = Self::forward(graph);
        body.paged_stack_writes = true;
        Some(body)
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn paged_read_widths(self, unit: &super::Unit) -> [bool; 2] {
        if self.paged_sse() {
            return [false, true];
        }
        let mut widths = [false; 2];
        if self.paged_fetch_bytes.is_some() {
            for instruction in &unit.instructions[self.earlier as usize..self.tail as usize] {
                let width = paged_read_width(instruction.operation, None).or_else(|| {
                    (self.paged_stack_writes
                        && paged_stack_dword(instruction.operation)
                            == Some(crate::memory::Access::Read))
                    .then_some(Width::W32)
                });
                match width {
                    Some(Width::W8) => widths[0] = true,
                    Some(Width::W32) => widths[1] = true,
                    _ => {}
                }
            }
        }
        widths
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn has_paged_byte_store(self, unit: &super::Unit) -> bool {
        (unit.context.is_long64() || self.paged_stack_writes)
            && self.paged_fetch_bytes.is_some()
            && self.natural_forward_loop()
            && unit.instructions[self.head as usize..self.tail as usize]
                .iter()
                .any(|instruction| paged_byte_store(instruction.operation, None))
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn select_paged_scalar_f32(unit: &super::Unit) -> Option<Self> {
        if !unit.context.is_long64() {
            return None;
        }
        Self::select_straight_body(
            &unit.instructions,
            LoopKind::PagedScalarF32,
            |operation| {
                inside_register_body(operation)
                    || paged_scalar_f32_read(operation)
                    || matches!(operation, Operation::Float(instruction)
                        if instruction.op.scalar && instruction.op.format == Format::F32
                            && instruction.op.kind == Kind::Add && matches!(instruction.src, Location::Register(_)))
            },
            |body| body.iter().any(|i| paged_scalar_f32_read(i.operation)),
        )?
        .without_dword_projection()
        .within_paged_fetch(unit)
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn paged_sse(self) -> bool {
        self.kind == LoopKind::PagedScalarF32
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn with_paged_read_span(mut self, unit: &super::Unit) -> Option<Self> {
        if self.kind != LoopKind::PagedReads || self.paged_read_widths(unit) != [true, false] {
            return None;
        }
        let proof = super::read_loop::ReadLoop::recognize_integer(
            &unit.context,
            &unit.instructions[self.head as usize..=self.tail as usize],
        )?;
        if !proof.one_stream() {
            return None;
        }
        self.paged_read_span = Some(proof);
        Some(self)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn with_paged_f32_read_span(mut self, unit: &super::Unit) -> Option<Self> {
        debug_assert!(self.paged_sse());
        self.paged_read_span = Some(super::read_loop::ReadLoop::recognize_paged_f32(
            &unit.context,
            &unit.instructions[self.head as usize..=self.tail as usize],
        )?);
        Some(self)
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn within_paged_fetch(mut self, unit: &super::Unit) -> Option<Self> {
        let first = unit.instructions[self.earlier as usize].pc;
        let end = unit.instructions[self.tail as usize].next_pc;
        let length = u32::try_from(end.checked_sub(first)?).ok()?;
        if !unit.context.single_page_code_span(first, length) {
            return None;
        }
        self.paged_fetch_bytes = Some(length);
        Some(self)
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) fn select_shared_float(instructions: &[Instruction]) -> Option<Self> {
        Self::select_body(instructions, LoopKind::Float).filter(|body| body.max_lap_len <= 64)
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) fn is_shared_float(self) -> bool {
        self.kind == LoopKind::Float
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) fn select_shared_rotated_reads(unit: &super::Unit) -> Option<Self> {
        if unit.context.mode() != Mode::Protected || !unit.context.code32() {
            return None;
        }
        super::loop_cfg::select_shared_rotated_reads(&unit.instructions).map(Self::forward)
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) fn select_shared_natural_memory(instructions: &[Instruction]) -> Option<Self> {
        super::loop_cfg::select_shared_natural_memory(instructions).map(Self::forward)
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    fn select_shared_integer_reads(unit: &super::Unit) -> Option<Self> {
        if !(unit.context.is_long64()
            || unit.context.mode() == Mode::Protected && unit.context.code32())
        {
            return None;
        }
        Self::select_body(&unit.instructions, LoopKind::IntegerReads)
            .filter(|body| body.max_lap_len <= 64)
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) fn shared_memory_prechecked(self) -> bool {
        self.kind == LoopKind::IntegerReads || self.natural_forward_loop()
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) fn is_optional_shared_loop(self) -> bool {
        self.kind != LoopKind::Registers
    }

    pub(super) fn select(unit: &super::Unit) -> Option<Self> {
        let instructions = &unit.instructions;
        if cfg!(feature = "parallel") {
            let body = Self::select_body(instructions, LoopKind::Registers);
            #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
            let body = body.or_else(|| Self::select_shared_integer_reads(unit));
            return body;
        }
        let existing = Self::select_body(instructions, LoopKind::Float)
            .or_else(|| Self::select_body(instructions, LoopKind::IntegerReads))
            .or_else(|| super::loop_cfg::select(instructions).map(Self::forward))
            .or_else(|| Self::select_body(instructions, LoopKind::Registers));
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let existing = existing.or_else(|| Self::select_increment_reads(unit));
        existing.or_else(|| {
            super::loop_cfg::select_natural_read_shifts(instructions).map(Self::forward)
        })
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn select_increment_reads(unit: &super::Unit) -> Option<Self> {
        Self::select_straight_body(
            &unit.instructions,
            LoopKind::Float,
            |operation| {
                inside_float_body(operation)
                    || matches!(
                        operation,
                        Operation::Unary {
                            kind: Unary::Inc,
                            dst: Location::Register(_),
                            width: Width::W32 | Width::W64,
                        }
                    )
            },
            |body| {
                let [reads @ .., increment, _, _] = body else {
                    return false;
                };
                matches!(
                    increment.operation,
                    Operation::Unary {
                        kind: Unary::Inc,
                        ..
                    }
                ) && reads.iter().any(|i| scalar_f32(i.operation))
                    && super::read_loop::ReadLoop::recognize(&unit.context, body).is_some()
            },
        )
    }

    fn forward((order, max_lap_len): (LoopOrder, u32)) -> Self {
        Self {
            earlier: order.earlier,
            head: order.head,
            tail: order.tail,
            max_lap_len,
            kind: LoopKind::ForwardEdges,
            project_dwords: true,
            paged_fetch_bytes: None,
            paged_stack_writes: false,
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            paged_read_span: None,
        }
    }

    fn select_body(instructions: &[Instruction], kind: LoopKind) -> Option<Self> {
        let inside = match kind {
            LoopKind::Float => inside_float_body,
            LoopKind::Registers => inside_register_body,
            LoopKind::IntegerReads => inside_integer_read_body,
            #[cfg(not(feature = "parallel"))]
            LoopKind::PagedReads | LoopKind::PagedScalarF32 => {
                unreachable!("paged reads use guarded admission")
            }
            LoopKind::ForwardEdges => unreachable!("forward edges use graph admission"),
        };
        Self::select_straight_body(instructions, kind, inside, |body| match kind {
            LoopKind::Registers => true,
            LoopKind::Float => body.iter().any(|i| scalar_f32(i.operation)),
            LoopKind::IntegerReads => {
                // Amortize the extra body and translation cost across several loads.
                const MIN_READS: usize = 4;
                body.iter()
                    .filter(|i| integer_read(i.operation))
                    .take(MIN_READS)
                    .count()
                    == MIN_READS
            }
            #[cfg(not(feature = "parallel"))]
            LoopKind::PagedReads | LoopKind::PagedScalarF32 => {
                unreachable!("paged reads use guarded admission")
            }
            LoopKind::ForwardEdges => unreachable!("forward edges use graph admission"),
        })
    }

    fn select_straight_body(
        instructions: &[Instruction],
        kind: LoopKind,
        inside: impl Fn(Operation) -> bool,
        accepts: impl Fn(&[Instruction]) -> bool,
    ) -> Option<Self> {
        let mut first = 0;
        let mut best = None::<Self>;
        for (index, instruction) in instructions.iter().enumerate() {
            if let Operation::Jcc { target, .. } = instruction.operation
                && instruction.next_pc > instruction.pc
                && let Ok(offset) =
                    instructions[first..index].binary_search_by_key(&target, |i| i.pc)
            {
                let head = first + offset;
                let length = index - head;
                if best.is_none_or(|previous| length > (previous.tail - previous.head) as usize)
                    && accepts(&instructions[head..=index])
                {
                    best = Some(Self {
                        earlier: head as u32,
                        head: head as u32,
                        tail: index as u32,
                        max_lap_len: length as u32 + 1,
                        kind,
                        project_dwords: true,
                        paged_fetch_bytes: None,
                        paged_stack_writes: false,
                        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                        paged_read_span: None,
                    });
                }
            }
            if !inside(instruction.operation)
                || instructions
                    .get(index + 1)
                    .is_none_or(|next| instruction.next_pc != next.pc || instruction.pc >= next.pc)
            {
                first = index + 1;
            }
        }
        best
    }
}

fn inside_register_body(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::Alu { .. }
            | Operation::Lea { .. }
            | Operation::Mov { .. }
            | Operation::Nop
            | Operation::Shift {
                dst: Location::Register(_),
                ..
            }
            | Operation::DoubleShift {
                dst: Location::Register(_),
                ..
            }
            | Operation::Unary {
                dst: Location::Register(_),
                ..
            }
    )
}

#[cfg(not(feature = "parallel"))]
fn paged_read_width(operation: Operation, address_size: Option<Width>) -> Option<Width> {
    use crate::x86::context::Segment;

    let (address, width) = match operation {
        Operation::Load {
            width: Width::W32,
            address,
            ..
        } => (address, Width::W32),
        Operation::MemoryAlu {
            kind: Alu::Cmp,
            width: Width::W32,
            address,
            ..
        } => (address, Width::W32),
        Operation::Extend {
            src: Location::Memory(address),
            source_width: Width::W8,
            width: Width::W32,
            signed: false,
            ..
        } => (address, Width::W8),
        _ => return None,
    };
    (address.segment == Segment::Ds && address_size.is_none_or(|size| size == address.size))
        .then_some(width)
}

#[cfg(not(feature = "parallel"))]
fn inside_paged_read_graph(operation: Operation) -> bool {
    inside_register_body(operation)
        || paged_read_width(operation, None).is_some()
        || matches!(operation, Operation::Jump { .. } | Operation::Jcc { .. })
}

#[cfg(not(feature = "parallel"))]
fn paged_byte_store(operation: Operation, address_size: Option<Width>) -> bool {
    matches!(
        operation,
        Operation::Store {
            width: Width::W8,
            address: Address {
                segment: crate::x86::context::Segment::Ds,
                size,
                ..
            },
            ..
        } if address_size.is_none_or(|required| required == size)
    )
}

#[cfg(not(feature = "parallel"))]
fn paged_stack_dword(operation: Operation) -> Option<crate::memory::Access> {
    use crate::{
        memory::Access,
        x86::{context::Segment, instruction::AddressBase},
    };

    let (address, access) = match operation {
        Operation::Load {
            width: Width::W32,
            address,
            ..
        }
        | Operation::MemoryAlu {
            kind: Alu::Cmp,
            width: Width::W32,
            address,
            ..
        } => (address, Access::Read),
        Operation::Store {
            width: Width::W32,
            address,
            ..
        } => (address, Access::Write),
        _ => return None,
    };
    (address.segment == Segment::Ss
        && address.size == Width::W32
        && address.base == AddressBase::Register(4)
        && address.index.is_none())
    .then_some(access)
}

#[cfg(not(feature = "parallel"))]
fn paged_write_graph_register(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::Cmov {
            src: Location::Register(_),
            ..
        } | Operation::Extend {
            src: Location::Register(_),
            signed: false,
            ..
        }
    )
}

#[cfg(not(feature = "parallel"))]
fn paged_scalar_f32_read(operation: Operation) -> bool {
    use crate::x86::{context::Segment, instruction::VectorOperand};

    match operation {
        Operation::Vector(VectorInstruction::Move {
            operand: VectorOperand::Memory(address),
            bytes: 4,
            aligned: false,
            store: false,
            ..
        }) => address.segment == Segment::Ds,
        Operation::Float(instruction)
            if instruction.op.scalar
                && instruction.op.format == Format::F32
                && instruction.op.kind == Kind::Mul =>
        {
            matches!(instruction.src, Location::Memory(address) if address.segment == Segment::Ds)
        }
        _ => false,
    }
}

fn integer_read(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::Load { .. }
            | Operation::Extend {
                src: Location::Memory(_),
                ..
            }
    )
}

// Register ADD/SUB can defer flags across one scalar read. That read preserves
// the arithmetic operands and restores the flags only if it exits on a fault.
fn can_omit_flags(instructions: &[Instruction]) -> (bool, Option<ReadFaultFlags>) {
    let [current, next, rest @ ..] = instructions else {
        return (false, None);
    };
    let Operation::Alu {
        kind: kind @ (Alu::Add | Alu::Sub),
        width,
        ..
    } = current.operation
    else {
        return (false, None);
    };
    let overwrites = |instruction: &Instruction| {
        matches!(
            instruction.operation,
            Operation::Alu {
                kind: Alu::Add | Alu::Sub | Alu::Cmp | Alu::Test,
                ..
            }
        )
    };
    if overwrites(next) {
        return (true, None);
    }
    let defer = integer_read(next.operation) && rest.first().is_some_and(overwrites);
    (
        defer,
        defer.then_some(ReadFaultFlags::Arithmetic(kind, width)),
    )
}

fn inside_integer_read_body(operation: Operation) -> bool {
    integer_read(operation)
        || matches!(
            operation,
            Operation::Alu { .. }
                | Operation::Lea { .. }
                | Operation::Mov { .. }
                | Operation::Nop
                | Operation::Extend {
                    src: Location::Register(_),
                    ..
                }
                | Operation::Imul {
                    src: Location::Register(_),
                    ..
                }
        )
}

#[derive(PartialEq, Eq)]
enum StatusEffect {
    Preserve,
    Replace,
    NeedsPrior,
}

fn status_effect(operation: Operation) -> StatusEffect {
    match operation {
        Operation::Alu {
            kind: Alu::Adc | Alu::Sbb,
            ..
        } => StatusEffect::NeedsPrior,
        Operation::Alu { .. } => StatusEffect::Replace,
        Operation::Shift {
            kind: Shift::Shl | Shift::Shr | Shift::Sar,
            dst: Location::Register(_),
            count: Count::Immediate(count),
            width,
        } if count & (if width == Width::W64 { 63 } else { 31 }) != 0 => StatusEffect::Replace,
        Operation::Mov { .. }
        | Operation::Lea { .. }
        | Operation::Nop
        | Operation::Unary {
            kind: Unary::Not, ..
        } => StatusEffect::Preserve,
        _ => StatusEffect::NeedsPrior,
    }
}

// Leave stores, control transfers and context-changing operations on the
// ordinary path. The clone still uses the original lowering and fault exits.
fn inside_float_body(operation: Operation) -> bool {
    match operation {
        Operation::Float(_) => scalar_f32(operation),
        Operation::Alu { .. }
        | Operation::Imul { .. }
        | Operation::Load { .. }
        | Operation::Lea { .. }
        | Operation::Mov { .. }
        | Operation::Nop
        | Operation::Vector(
            VectorInstruction::Move { store: false, .. }
            | VectorInstruction::Binary { .. }
            | VectorInstruction::ToXmm { .. },
        ) => true,
        _ => false,
    }
}

fn scalar_f32(operation: Operation) -> bool {
    matches!(operation, Operation::Float(float)
        if float.op.scalar && float.op.format == Format::F32
            && matches!(float.op.kind, Kind::Add | Kind::Mul))
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn zero_upper_after(known: u16, operation: Operation) -> u16 {
    match operation {
        Operation::Vector(VectorInstruction::Move {
            register,
            bytes: 4,
            preserve_upper: false,
            store: false,
            ..
        }) => known | (1 << register),
        Operation::Vector(VectorInstruction::Move {
            bytes: 4,
            preserve_upper: true,
            store: false,
            ..
        })
        | Operation::Nop => known,
        operation if scalar_f32(operation) => known,
        _ => 0,
    }
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn normal_low_after(known: u16, operation: Operation) -> u16 {
    match operation {
        Operation::Float(instruction) if scalar_f32(operation) => known | (1 << instruction.dst),
        Operation::Vector(VectorInstruction::Move { register, .. }) => known & !(1 << register),
        Operation::Nop => known,
        _ => 0,
    }
}

impl Emitter {
    pub(super) fn exclude_loop_stop(&mut self, instructions: &[Instruction], body: StructuredLoop) {
        let first = instructions[body.earlier as usize].pc as i64;
        let last = instructions[body.tail as usize].pc as i64;
        self.op(Op::LocalGet(3))
            .op(Op::LocalGet(2))
            .op(Op::I64Const(first))
            .op(Op::I64GeU)
            .op(Op::I32And)
            .op(Op::LocalGet(2))
            .op(Op::I64Const(last))
            .op(Op::I64LeU)
            .op(Op::I32And)
            .op(Op::I32Eqz);
    }

    pub(super) fn emit_prechecked_loop(
        &mut self,
        instructions: &[Instruction],
        body: StructuredLoop,
        loop_depth: u32,
        write_segment_base: Option<u32>,
    ) {
        self.op(Op::If(BlockType::Empty));
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(window) = self.indexed_read_window {
            self.guard_indexed_read_window(window, body.max_lap_len);
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        let trial = if let Some((_, _, budget_floor)) = self.parallel_vector_span {
            let lap = i64::from(body.max_lap_len);
            let trial_cost = super::parallel::VECTOR_SPAN_TRIAL_LAPS * lap;
            // Reuse the lap gate. If three laps find no hit, enter the
            // original loop at its head with the already committed state.
            self.op(Op::LocalGet(REMAINING))
                .op(Op::I64Const(trial_cost + lap))
                .op(Op::I64GeU)
                .op(Op::If(BlockType::Result(wasm_encoder::ValType::I64)))
                .op(Op::LocalGet(REMAINING))
                .op(Op::I64Const(trial_cost - 1))
                .op(Op::I64Sub)
                .op(Op::Else)
                .op(Op::I64Const(lap))
                .op(Op::End)
                .op(Op::LocalSet(budget_floor))
                .op(Op::Block(BlockType::Empty));
            self.emit_prechecked_body(instructions, body, loop_depth + 2, write_segment_base);
            self.op(Op::End);
            self.parallel_vector_span.take()
        } else {
            None
        };
        self.emit_prechecked_body(instructions, body, loop_depth + 1, write_segment_base);
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        {
            self.parallel_vector_span = trial;
        }
        self.op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn emit_stack_loops(
        &mut self,
        instructions: &[Instruction],
        body: StructuredLoop,
        loop_depth: u32,
        write_segment_base: Option<u32>,
        guard: super::stack_slots::StackGuard,
    ) {
        self.op(Op::If(BlockType::Empty));
        self.guard_stack_slots(guard);
        self.op(Op::If(BlockType::Empty));
        self.proved_stack = Some((guard.slots, guard.host));
        self.emit_prechecked_body(instructions, body, loop_depth + 2, write_segment_base);
        self.proved_stack = None;
        self.op(Op::Else);
        self.emit_prechecked_body(instructions, body, loop_depth + 2, write_segment_base);
        self.op(Op::End).op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn emit_integer_read_loops(
        &mut self,
        instructions: &[Instruction],
        body: StructuredLoop,
        loop_depth: u32,
        write_segment_base: Option<u32>,
        guard: (super::read_loop::ReadLoop, super::memory::SpanCache, u32),
    ) {
        let (proof, span, first_bias) = guard;
        self.op(Op::If(BlockType::Empty));
        self.guard_integer_read_loop(proof, span.resolve, first_bias);
        self.op(Op::If(BlockType::Empty));
        self.proved_reads = Some((proof, first_bias));
        self.emit_prechecked_body(instructions, body, loop_depth + 2, write_segment_base);
        self.proved_reads = None;
        self.op(Op::Else);
        self.emit_prechecked_body(instructions, body, loop_depth + 2, write_segment_base);
        self.op(Op::End).op(Op::End);
    }

    pub(super) fn emit_guarded_paged_loop(
        &mut self,
        instructions: &[Instruction],
        body: StructuredLoop,
        loop_depth: u32,
        (guard, attempts): (u32, u32),
    ) {
        self.op(Op::LocalGet(attempts))
            .op(Op::I32Const(2))
            .op(Op::I32LtU)
            .op(Op::If(BlockType::Empty));
        self.exclude_loop_stop(instructions, body);
        self.op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(body.max_lap_len)))
            .op(Op::I64GeU)
            .op(Op::I32And)
            .op(Op::If(BlockType::Empty))
            .op(Op::I64Const(instructions[body.earlier as usize].pc as i64))
            .op(Op::I32Const(body.paged_fetch_bytes.unwrap() as i32));
        self.system_bits();
        self.op(Op::LocalGet(super::SHADOW))
            .op(Op::Call(guard))
            .op(Op::LocalTee(REASON))
            .op(Op::I32Const(1))
            .op(Op::I32Eq)
            .op(Op::If(BlockType::Empty))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(REASON));
        self.guarded_paged_lap = true;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some((proof, resolve, bias)) = self.paged_read_span {
            if body.paged_sse() {
                self.guard_paged_f32_read_loop(proof, resolve, bias);
                self.paged_proved_f32_reads = Some((proof, bias));
            } else {
                self.guard_integer_read_loop(proof, resolve, bias);
                self.proved_reads = Some((proof, bias));
            }
            self.op(Op::If(BlockType::Empty));
            self.emit_prechecked_body(instructions, body, loop_depth + 4, None);
            self.proved_reads = None;
            self.paged_proved_f32_reads = None;
            self.op(Op::Else);
            self.emit_prechecked_body(instructions, body, loop_depth + 4, None);
            self.op(Op::End);
        } else {
            self.emit_prechecked_body(instructions, body, loop_depth + 3, None);
        }
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        self.emit_prechecked_body(instructions, body, loop_depth + 3, None);
        self.guarded_paged_lap = false;
        // Later visits can revalidate after a successful clone. A cold failure
        // spends one attempt and an observer rejection disables the clone.
        self.op(Op::Else)
            .op(Op::LocalGet(attempts))
            .op(Op::LocalGet(REASON))
            .op(Op::I32Add)
            .op(Op::I32Const(1))
            .op(Op::I32Add)
            .op(Op::LocalSet(attempts))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(REASON))
            .op(Op::End)
            .op(Op::End)
            .op(Op::End);
    }

    pub(super) fn emit_prechecked_body(
        &mut self,
        instructions: &[Instruction],
        body: StructuredLoop,
        loop_depth: u32,
        write_segment_base: Option<u32>,
    ) {
        let current_index = self.current_index;
        for (bank, upper) in self.register_upper_dwords.into_iter().enumerate() {
            if let Some(upper) = upper {
                self.op(Op::LocalGet(GPR + bank as u32))
                    .op(Op::I64Const(!i64::from(u32::MAX)))
                    .op(Op::I64And)
                    .op(Op::LocalSet(upper));
                self.register(bank as u8, Width::W32);
                self.op(Op::LocalSet(GPR + bank as u32));
            }
        }
        self.loop_gates_prechecked = true;
        self.emit_structured_loop(instructions, body, loop_depth, write_segment_base);
        self.loop_gates_prechecked = false;
        self.current_index = current_index;
    }

    pub(super) fn restore_loop_registers(&mut self) {
        for (bank, upper) in self.register_upper_dwords.into_iter().enumerate() {
            if let Some(upper) = upper {
                let bank = GPR + bank as u32;
                self.op(Op::LocalGet(bank))
                    .op(Op::LocalGet(upper))
                    .op(Op::I64Or)
                    .op(Op::LocalSet(bank));
            }
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn emit_read_loop_diamond(
        &mut self,
        diamond: ReadLoopDiamond,
        body: StructuredLoop,
        depth: u32,
    ) {
        use crate::x86::instruction::{Condition, DataOperand};
        let ReadLoopDiamond {
            condition,
            low,
            high,
            advance,
            replace,
            head,
            exit,
            ..
        } = diamond;
        let width = Width::W32;
        self.cloned_condition(condition, Some(ConditionSource::Operands(width)));
        // Keep the original data-dependent branch. Each arm commits only
        // its selected register update and its original retirement count.
        self.op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Empty));
        self.data_operand(replace, width);
        self.write_register(high, width);
        self.retire_instructions(4); // selector + MOV + CMP + Jcc
        self.op(Op::Else);
        self.data_register(low, width);
        self.op(Op::I32WrapI64);
        self.data_operand(advance, width);
        self.op(Op::I32WrapI64)
            .op(Op::I32Add)
            .op(Op::I32Const(1))
            .op(Op::I32Add)
            .op(Op::I64ExtendI32U);
        self.write_register(low, width);
        self.retire_instructions(5); // selector + ADD + INC + CMP + Jcc
        self.op(Op::End);
        self.omit_result_status = false;
        self.stores = false;
        self.op(Op::I64Const(head as i64)).op(Op::LocalSet(RIP));
        self.data_register(low, width);
        self.op(Op::I32WrapI64);
        self.data_register(high, width);
        let loop_label = body.active_labels(self.current_index);
        self.op(Op::I32WrapI64)
            .op(Op::I32LtU)
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(body.max_lap_len)))
            .op(Op::I64GeU)
            .op(Op::I32And)
            .op(Op::BrIf(loop_label));
        // Both real exits and the checked budget handoff expose the last CMP.
        self.integer(Alu::Cmp, low, DataOperand::Register(high), width, None);
        self.condition(Condition::Below);
        self.op(Op::BrIf(loop_label + 1));

        self.restore_loop_registers();
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::If(BlockType::Empty));
        self.op(Op::I64Const(exit[1] as i64)).op(Op::LocalSet(RIP));
        self.route(exit[1], depth + 1, depth + 2);
        self.op(Op::End);
        self.op(Op::I64Const(exit[0] as i64)).op(Op::LocalSet(RIP));
        self.route(exit[0], depth, depth + 1);
    }

    pub(super) fn emit_structured_loop(
        &mut self,
        instructions: &[Instruction],
        body: StructuredLoop,
        loop_depth: u32,
        write_segment_base: Option<u32>,
    ) {
        let inner_depth = loop_depth + 1;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let prove_zero_upper = self.proved_reads.is_some();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let dyadic = self.float_dyadic;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(reduction) = dyadic {
            self.admit_dyadic_accumulator(reduction);
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let diamond = self
            .read_loop_diamond
            .filter(|_| self.loop_gates_prechecked);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let precheck_entry = diamond.is_some();
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        let precheck_entry = false;
        if precheck_entry {
            // Fund the first lap here and later laps at their merged latch.
            self.op(Op::LocalGet(REMAINING))
                .op(Op::I64Const(i64::from(body.max_lap_len)))
                .op(Op::I64LtU)
                .op(Op::BrIf(0));
        }
        self.op(Op::Loop(BlockType::Empty));
        if !self.loop_gates_prechecked
            && let Some(guard) = self.paged_loop_guard
        {
            self.emit_guarded_paged_loop(instructions, body, inner_depth, guard);
        }
        if self.loop_gates_prechecked && !precheck_entry {
            // Leave the guarded clone before a partial lap.
            let minimum = Op::I64Const(i64::from(body.max_lap_len));
            #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
            let minimum = self
                .parallel_vector_span
                .map_or(minimum, |(_, _, budget_floor)| Op::LocalGet(budget_floor));
            self.op(Op::LocalGet(REMAINING)).op(minimum).op(Op::I64LtU);
            if self.register_upper_dwords.iter().any(Option::is_some) {
                self.op(Op::If(BlockType::Empty));
                self.restore_loop_registers();
                self.op(Op::Br(2)).op(Op::End);
            } else {
                self.op(Op::BrIf(1));
            }
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(reduction) = dyadic {
            self.op(Op::Block(BlockType::Empty));
            self.try_dyadic_prefix(reduction);
        }
        let order = body.order();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let outlined_float = self.float_outline_params != 0;
        #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
        let outlined_float = false;
        let (predicates, read_prefix_len) = if !cfg!(feature = "parallel")
            && self.loop_gates_prechecked
            && body.forward_edges()
            && !self.guarded_paged_lap
        {
            // Batching the protected dword prefix removes a hot spill
            // needed only when the first read faults.
            let read_prefix_len = if self.context.mode() == crate::x86::context::Mode::Protected
                && self.context.code32()
            {
                super::loop_cfg::read_prefix_len(instructions, order)
            } else {
                0
            };
            (
                super::loop_cfg::compare_predicates(instructions, order, &self.context),
                read_prefix_len,
            )
        } else {
            (Vec::new(), 0)
        };
        if body.forward_edges() {
            // Each later cloned instruction has an enclosing forward label.
            for _ in 1..order.len() {
                self.op(Op::Block(BlockType::Empty));
            }
        }
        let defer_read_flags = !cfg!(feature = "parallel")
            && self.loop_gates_prechecked
            && body.kind == LoopKind::IntegerReads;
        let mut flags_on_next_read_fault = None;
        for position in 0..order.len() {
            let ordinal = order.at(position) as usize;
            if body.forward_edges() && position != 0 {
                self.op(Op::End);
            }
            self.current_index = ordinal as u32;
            let depth = inner_depth + body.active_labels(self.current_index);
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            let depth = if dyadic.is_some() {
                if self.current_index == body.tail - 2 {
                    self.op(Op::End);
                }
                depth + u32::from(self.current_index < body.tail - 2)
            } else {
                depth
            };
            self.structured_loop = Some((body, depth));
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            if let Some(diamond) = diamond.filter(|diamond| diamond.read + 1 == self.current_index)
            {
                self.current_operation = instructions[ordinal].operation;
                self.current_pc = instructions[ordinal].pc;
                self.emit_read_loop_diamond(diamond, body, depth);
                for _ in 0..body.active_labels(self.current_index) {
                    self.op(Op::End);
                }
                break;
            }
            let compare = predicates
                .get(position as usize)
                .copied()
                .unwrap_or_default();
            self.flags_on_read_fault = match compare {
                CompareAction::RestoreOnLoadFault(flags)
                | CompareAction::SkipStatusAndRestoreOnLoadFault(flags) => Some(flags),
                _ => None,
            };
            // The complete-lap guard and the no-fault body rule hide intermediate status.
            // A worker signal can publish flags at any instruction entry.
            self.omit_result_status = !cfg!(feature = "parallel")
                && !self.observed
                && body.kind == LoopKind::Registers
                && self.loop_gates_prechecked
                && status_effect(instructions[ordinal].operation) == StatusEffect::Replace
                && instructions[ordinal + 1..body.tail as usize + 1]
                    .iter()
                    .map(|i| status_effect(i.operation))
                    .find(|effect| *effect != StatusEffect::Preserve)
                    == Some(StatusEffect::Replace);
            if defer_read_flags {
                self.flags_on_read_fault = flags_on_next_read_fault;
                (self.omit_result_status, flags_on_next_read_fault) =
                    can_omit_flags(&instructions[ordinal..=body.tail as usize]);
            }
            self.omit_result_status |= matches!(
                compare,
                CompareAction::SkipStatus | CompareAction::SkipStatusAndRestoreOnLoadFault(_)
            );
            let condition = if outlined_float && self.current_index == body.tail {
                let Operation::Alu { width, .. } = instructions[ordinal - 1].operation else {
                    unreachable!("proved float loop comparison");
                };
                Some(super::loop_cfg::ConditionSource::Operands(width))
            } else if let CompareAction::Condition(source) = compare {
                Some(source)
            } else {
                None
            };
            self.omit_result_status |=
                outlined_float && matches!(body.tail - self.current_index, 1 | 2);
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            if diamond.is_some_and(|diamond| diamond.read == self.current_index) {
                self.omit_result_status = true;
            }
            self.instruction_retirement = if outlined_float && self.current_index == body.tail {
                // The guard funded the complete lap. A numeric rejection
                // publishes its own prefix before unwinding the child.
                InstructionRetirement::AfterPrefix(position)
            } else if outlined_float || position < read_prefix_len {
                InstructionRetirement::Deferred
            } else if read_prefix_len != 0 && position == read_prefix_len {
                InstructionRetirement::AfterPrefix(read_prefix_len)
            } else {
                InstructionRetirement::One
            };
            self.instruction(
                instructions[ordinal],
                instructions.get(ordinal + 1),
                depth,
                write_segment_base,
                condition,
            );
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            if prove_zero_upper {
                // Each lap starts with unknown XMM values at its head.
                self.zero_upper_xmm =
                    zero_upper_after(self.zero_upper_xmm, instructions[ordinal].operation);
                // A failed classification leaves this body before committing the instruction.
                self.normal_low_f32 =
                    normal_low_after(self.normal_low_f32, instructions[ordinal].operation);
            }
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        {
            self.zero_upper_xmm = 0;
            self.normal_low_f32 = 0;
        }
        self.omit_result_status = false;
        self.flags_on_read_fault = None;
        self.instruction_retirement = InstructionRetirement::One;
        self.op(Op::End);
        self.structured_loop = None;
    }
}
