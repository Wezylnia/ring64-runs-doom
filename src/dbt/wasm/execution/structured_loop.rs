use super::{
    Emitter, GPR, REMAINING,
    control::InstructionRetirement,
    loop_cfg::{CompareAction, LoopOrder, ReadFaultFlags},
};
use wasm_encoder::{BlockType, Instruction as Op};

use crate::x86::{
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
    ForwardEdges,
}

#[derive(Clone, Copy)]
pub(super) struct StructuredLoop {
    earlier: u32,
    pub(super) head: u32,
    pub(super) tail: u32,
    max_lap_len: u32,
    kind: LoopKind,
}

impl StructuredLoop {
    pub(super) fn dword_writes(self, unit: &super::Unit) -> u16 {
        if cfg!(feature = "parallel")
            || self.kind != LoopKind::Registers
            || unit.context.is_long64()
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
            matches!(self.kind, LoopKind::Float | LoopKind::Registers)
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

    pub(super) fn order(self) -> LoopOrder {
        LoopOrder {
            earlier: self.earlier,
            head: self.head,
            tail: self.tail,
        }
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) fn select_shared_float(instructions: &[Instruction]) -> Option<Self> {
        Self::select_body(instructions, LoopKind::Float).filter(|body| body.max_lap_len <= 64)
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) fn select_shared_rotated_reads(unit: &super::Unit) -> Option<Self> {
        use crate::x86::context::Mode;

        if unit.context.mode() != Mode::Protected || !unit.context.code32() {
            return None;
        }
        super::loop_cfg::select_shared_rotated_reads(&unit.instructions).map(Self::forward)
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) fn is_optional_shared_loop(self) -> bool {
        self.kind != LoopKind::Registers
    }

    pub(super) fn select(instructions: &[Instruction]) -> Option<Self> {
        if cfg!(feature = "parallel") {
            return Self::select_body(instructions, LoopKind::Registers);
        }
        Self::select_body(instructions, LoopKind::Float)
            .or_else(|| Self::select_body(instructions, LoopKind::IntegerReads))
            .or_else(|| super::loop_cfg::select(instructions).map(Self::forward))
            .or_else(|| Self::select_body(instructions, LoopKind::Registers))
    }

    fn forward((order, max_lap_len): (LoopOrder, u32)) -> Self {
        Self {
            earlier: order.earlier,
            head: order.head,
            tail: order.tail,
            max_lap_len,
            kind: LoopKind::ForwardEdges,
        }
    }

    fn select_body(instructions: &[Instruction], kind: LoopKind) -> Option<Self> {
        let inside = match kind {
            LoopKind::Float => inside_float_body,
            LoopKind::Registers => inside_register_body,
            LoopKind::IntegerReads => inside_integer_read_body,
            LoopKind::ForwardEdges => unreachable!("forward edges use graph admission"),
        };
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
                    && match kind {
                        LoopKind::Registers => true,
                        LoopKind::Float => instructions[head..index]
                            .iter()
                            .any(|i| scalar_f32(i.operation)),
                        LoopKind::IntegerReads => {
                            // Amortize the extra body and translation cost across several loads.
                            const MIN_READS: usize = 4;
                            instructions[head..index]
                                .iter()
                                .filter(|i| integer_read(i.operation))
                                .take(MIN_READS)
                                .count()
                                == MIN_READS
                        }
                        LoopKind::ForwardEdges => unreachable!("forward edges use graph admission"),
                    }
                {
                    best = Some(Self {
                        earlier: head as u32,
                        head: head as u32,
                        tail: index as u32,
                        max_lap_len: length as u32 + 1,
                        kind,
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
        self.emit_prechecked_body(instructions, body, loop_depth + 1, write_segment_base);
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
        self.guard_integer_read_loop(proof, span, first_bias);
        self.op(Op::If(BlockType::Empty));
        self.proved_reads = Some((proof, first_bias));
        self.emit_prechecked_body(instructions, body, loop_depth + 2, write_segment_base);
        self.proved_reads = None;
        self.op(Op::Else);
        self.emit_prechecked_body(instructions, body, loop_depth + 2, write_segment_base);
        self.op(Op::End).op(Op::End);
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
        self.op(Op::Loop(BlockType::Empty));
        if self.loop_gates_prechecked {
            // Leave the guarded clone for the checked clone before a partial lap.
            self.op(Op::LocalGet(REMAINING))
                .op(Op::I64Const(i64::from(body.max_lap_len)))
                .op(Op::I64LtU);
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
        let (predicates, read_prefix_len) =
            if !cfg!(feature = "parallel") && self.loop_gates_prechecked && body.forward_edges() {
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
