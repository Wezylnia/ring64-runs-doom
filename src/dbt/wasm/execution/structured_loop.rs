use super::Emitter;
use wasm_encoder::{BlockType, Instruction as Op};

use crate::x86::{
    float::{Format, Kind},
    instruction::{Instruction, Operation, VectorInstruction},
};

#[derive(Clone, Copy)]
pub(super) struct StructuredLoop {
    pub(super) head: u32,
    pub(super) tail: u32,
}

impl StructuredLoop {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn read_proof(self, unit: &super::Unit) -> Option<super::read_loop::ReadLoop> {
        super::read_loop::ReadLoop::recognize(
            &unit.context,
            &unit.instructions[self.head as usize..=self.tail as usize],
        )
    }

    pub(super) fn select(instructions: &[Instruction]) -> Option<Self> {
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
                    && instructions[head..index]
                        .iter()
                        .any(|i| scalar_f32(i.operation))
                {
                    best = Some(Self {
                        head: head as u32,
                        tail: index as u32,
                    });
                }
            }
            if !inside_body(instruction.operation)
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

// Leave stores, control transfers and context-changing operations on the
// ordinary path. The clone still uses the original lowering and fault exits.
fn inside_body(operation: Operation) -> bool {
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

impl Emitter {
    pub(super) fn emit_structured_loop(
        &mut self,
        instructions: &[Instruction],
        body: StructuredLoop,
        loop_depth: u32,
        write_segment_base: Option<u32>,
    ) {
        let inner_depth = loop_depth + 1;
        self.structured_loop = Some((body, inner_depth));
        self.op(Op::Loop(BlockType::Empty));
        for ordinal in body.head as usize..=body.tail as usize {
            self.current_index = ordinal as u32;
            self.instruction(
                instructions[ordinal],
                instructions.get(ordinal + 1),
                inner_depth,
                write_segment_base,
            );
        }
        self.op(Op::End);
        self.structured_loop = None;
    }
}
