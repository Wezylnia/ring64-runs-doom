use super::{Emitter, GPR, REMAINING};
use wasm_encoder::{BlockType, Instruction as Op};

use crate::x86::{
    float::{Format, Kind},
    instruction::{Address, Instruction, Location, Operation, VectorInstruction, Width},
};

#[derive(Clone, Copy)]
pub(super) struct StructuredLoop {
    pub(super) head: u32,
    pub(super) tail: u32,
    pub(super) register_only: bool,
}

impl StructuredLoop {
    pub(super) fn dword_writes(self, unit: &super::Unit) -> u16 {
        if !self.register_only || unit.context.is_long64() {
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
        if self.register_only {
            return None;
        }
        super::read_loop::ReadLoop::recognize(
            &unit.context,
            &unit.instructions[self.head as usize..=self.tail as usize],
        )
    }

    pub(super) fn select(instructions: &[Instruction]) -> Option<Self> {
        Self::select_body(instructions, false).or_else(|| Self::select_body(instructions, true))
    }

    fn select_body(instructions: &[Instruction], register_only: bool) -> Option<Self> {
        let inside = if register_only {
            inside_register_body
        } else {
            inside_float_body
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
                    && (register_only
                        || instructions[head..index]
                            .iter()
                            .any(|i| scalar_f32(i.operation)))
                {
                    best = Some(Self {
                        head: head as u32,
                        tail: index as u32,
                        register_only,
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

impl Emitter {
    pub(super) fn exclude_loop_stop(&mut self, instructions: &[Instruction], body: StructuredLoop) {
        let first = instructions[body.head as usize].pc as i64;
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
        let current_index = self.current_index;
        self.op(Op::If(BlockType::Empty));
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
        self.emit_structured_loop(instructions, body, loop_depth + 1, write_segment_base);
        self.loop_gates_prechecked = false;
        self.current_index = current_index;
        self.op(Op::End);
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
        self.structured_loop = Some((body, inner_depth));
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let prove_zero_upper = self.proved_reads.is_some();
        self.op(Op::Loop(BlockType::Empty));
        if self.loop_gates_prechecked {
            // Leave the guarded clone for the checked clone before a partial lap.
            self.op(Op::LocalGet(REMAINING))
                .op(Op::I64Const(i64::from(body.tail - body.head + 1)))
                .op(Op::I64LtU);
            if self.register_upper_dwords.iter().any(Option::is_some) {
                self.op(Op::If(BlockType::Empty));
                self.restore_loop_registers();
                self.op(Op::Br(2)).op(Op::End);
            } else {
                self.op(Op::BrIf(1));
            }
        }
        for ordinal in body.head as usize..=body.tail as usize {
            self.current_index = ordinal as u32;
            self.instruction(
                instructions[ordinal],
                instructions.get(ordinal + 1),
                inner_depth,
                write_segment_base,
            );
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            if prove_zero_upper {
                // Each lap starts with unknown XMM values at its head.
                self.zero_upper_xmm =
                    zero_upper_after(self.zero_upper_xmm, instructions[ordinal].operation);
            }
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        {
            self.zero_upper_xmm = 0;
        }
        self.op(Op::End);
        self.structured_loop = None;
    }
}
