use super::float_dyadic::DyadicReduction;
use crate::dbt::wasm::{
    ENTRY, Emitter, FLAGS, GPR, REMAINING, RIP, StructuredLoop, Unit,
    memory::read_loop::ReadLoop,
    numeric::{float::MASKED_STICKY_PRECISION, vector::Banks},
};
use crate::memory::canonical48;
use crate::x86::instruction::{Alu, Operation};
use wasm_encoder::{BlockType, Function, Instruction as Op, TypeSection, ValType};

pub(in crate::dbt::wasm) const HEADER_REJECTION: i32 = -1;

pub(in crate::dbt::wasm) struct FloatOutline {
    index: u32,
    pub(in crate::dbt::wasm) signature: u32,
    pub(in crate::dbt::wasm) sticky_index: Option<u32>,
    pub(in crate::dbt::wasm) dyadic: Option<DyadicReduction>,
    induction_input: (u32, u32),
    inputs: Vec<(u32, ValType)>,
    outputs: Vec<(u32, ValType)>,
}

impl FloatOutline {
    pub(in crate::dbt::wasm) fn new(
        unit: &Unit,
        body: StructuredLoop,
        proof: ReadLoop,
        first_bias: u32,
        xmm: [Option<u32>; 16],
        index: u32,
        types: &mut TypeSection,
    ) -> Option<Self> {
        let instructions = &unit.instructions[body.head as usize..=body.tail as usize];
        // The child never calls a fault helper. Prove the static latch's target too.
        let head = instructions[0].pc;
        if if unit.context.is_long64() {
            !canonical48(head)
        } else {
            head > unit.context.code_limit()
        } {
            return None;
        }
        let (gprs, induction, biases) = proof.outline_registers()?;
        let banks = Banks::for_instructions(instructions);
        let mut inputs = vec![
            (0, ValType::I32),
            (FLAGS, ValType::I64),
            (RIP, ValType::I64),
            (REMAINING, ValType::I64),
        ];
        let mut outputs = vec![
            (ENTRY, ValType::I32),
            (FLAGS, ValType::I64),
            (RIP, ValType::I64),
            (REMAINING, ValType::I64),
            (GPR + u32::from(induction), ValType::I64),
        ];
        for bank in 0..16u32 {
            if gprs & (1 << bank) != 0 {
                inputs.push((GPR + bank, ValType::I64));
            }
            if banks.used & (1 << bank) != 0 {
                inputs.push((
                    xmm[bank as usize].expect("outline XMM input"),
                    ValType::V128,
                ));
            }
            if banks.written & (1 << bank) != 0 {
                outputs.push((
                    xmm[bank as usize].expect("outline XMM output"),
                    ValType::V128,
                ));
            }
        }
        for bias in 0..biases as u32 {
            inputs.push((first_bias + bias, ValType::I32));
        }
        let induction = GPR + u32::from(induction);
        let induction_parameter = inputs.iter().position(|&(local, _)| local == induction)?;
        let signature = types.len();
        types.ty().function(
            inputs.iter().map(|(_, ty)| *ty),
            outputs.iter().map(|(_, ty)| *ty),
        );
        Some(Self {
            index,
            signature,
            sticky_index: None,
            dyadic: None,
            induction_input: (induction_parameter as u32, induction),
            inputs,
            outputs,
        })
    }

    pub(in crate::dbt::wasm) fn enable_sticky(&mut self) {
        self.sticky_index = Some(self.index + 1);
    }
}

impl Emitter {
    pub(in crate::dbt::wasm) fn emit_float_outline(
        &mut self,
        outline: &FloatOutline,
        locals: &[(u32, ValType)],
        instructions: &[crate::x86::instruction::Instruction],
        loop_body: StructuredLoop,
        sticky: bool,
    ) -> (u32, Function, Vec<wasm_encoder::BranchHint>) {
        let current_index = self.current_index;
        let current_pc = self.current_pc;
        let current_operation = self.current_operation;
        let stores = self.stores;
        let segment = self.segment;

        // Retain the normal emitter's local numbering. The first five locals
        // here correspond to the parent function's leading parameters.
        let mut declarations = vec![(1, ValType::I32), (2, ValType::I64), (2, ValType::I32)];
        declarations.extend_from_slice(locals);
        let dyadic = if sticky { None } else { outline.dyadic };
        if dyadic.is_some() {
            declarations.extend_from_slice(&DyadicReduction::LOCALS);
        }
        let function = Function::new(declarations);
        let parent = std::mem::replace(&mut self.body, function);
        let parent_hints = std::mem::take(&mut self.branch_hints);
        self.float_outline_params = outline.inputs.len() as u32;
        self.float_raw_sticky = sticky;
        self.float_dyadic = dyadic;
        for (argument, (local, _)) in outline.inputs.iter().enumerate() {
            self.body.instruction(&Op::LocalGet(argument as u32));
            self.op(Op::LocalSet(*local));
        }
        // The head ordinal means a partial budget. Numeric rejection returns
        // the interior ordinal or HEADER_REJECTION. Tail+1 means completion.
        self.op(Op::I32Const(loop_body.head as i32))
            .op(Op::LocalSet(ENTRY))
            .op(Op::Block(BlockType::Empty));
        self.loop_gates_prechecked = true;
        self.emit_structured_loop(instructions, loop_body, 0, None);
        self.loop_gates_prechecked = false;
        self.op(Op::End);
        if let Some(reduction) = dyadic {
            self.flush_dyadic_prefixes(reduction);
        }
        let (parameter, induction) = outline.induction_input;
        // An unchanged induction register means no complete lap retired.
        // Otherwise the final visible status belongs to its latch compare.
        self.body.instruction(&Op::LocalGet(parameter));
        self.op(Op::LocalGet(induction))
            .op(Op::I64Ne)
            .op(Op::If(BlockType::Empty));
        let Operation::Alu {
            kind: Alu::Cmp,
            dst,
            src,
            width,
        } = instructions[loop_body.tail as usize - 1].operation
        else {
            unreachable!("proved float loop comparison");
        };
        self.integer(Alu::Cmp, dst, src, width, None);
        self.op(Op::End);
        for (local, _) in &outline.outputs {
            self.op(Op::LocalGet(*local));
        }
        self.op(Op::End);
        self.float_outline_params = 0;
        self.float_raw_sticky = false;
        self.float_dyadic = None;
        self.current_index = current_index;
        self.current_pc = current_pc;
        self.current_operation = current_operation;
        self.stores = stores;
        self.segment = segment;
        let body = std::mem::replace(&mut self.body, parent);
        let hints = std::mem::replace(&mut self.branch_hints, parent_hints);
        let index = if sticky {
            outline.sticky_index.expect("sticky float outline index")
        } else {
            outline.index
        };
        (index, body, hints)
    }

    pub(in crate::dbt::wasm) fn call_float_outline(
        &mut self,
        outline: &FloatOutline,
        body: StructuredLoop,
        tail: crate::x86::instruction::Instruction,
        loop_depth: u32,
        retry: u32,
        rejected: u32,
    ) {
        for (local, _) in &outline.inputs {
            self.op(Op::LocalGet(*local));
        }
        if let Some(raw) = outline.sticky_index {
            self.load_float_mxcsr();
            self.op(Op::I32Const(MASKED_STICKY_PRECISION))
                .op(Op::I32And)
                .op(Op::I32Const(MASKED_STICKY_PRECISION))
                .op(Op::I32Eq)
                .unlikely(Op::If(BlockType::FunctionType(outline.signature)))
                .op(Op::Call(raw))
                .op(Op::Else)
                .op(Op::Call(outline.index))
                .op(Op::End);
        } else {
            self.op(Op::Call(outline.index));
        }
        for (local, _) in outline.outputs.iter().rev() {
            self.op(Op::LocalSet(*local));
        }
        self.op(Op::LocalGet(ENTRY))
            .op(Op::I32Const((body.tail + 1) as i32))
            .op(Op::I32Eq)
            .op(Op::If(BlockType::Empty));
        self.current_operation = tail.operation;
        self.stores = false;
        self.route(tail.next_pc, loop_depth + 1, loop_depth + 2);
        self.op(Op::End)
            .op(Op::LocalGet(ENTRY))
            .op(Op::I32Const(body.head as i32))
            .op(Op::I32Ne)
            .op(Op::If(BlockType::Empty))
            .op(Op::I32Const(1))
            .op(Op::LocalSet(rejected))
            .op(Op::LocalGet(ENTRY))
            .op(Op::I32Const(HEADER_REJECTION))
            .op(Op::I32Ne)
            .op(Op::If(BlockType::Empty))
            .op(Op::I32Const(1))
            .op(Op::LocalSet(retry))
            .op(Op::Br(loop_depth + 2))
            .op(Op::End)
            .op(Op::I32Const(body.head as i32))
            .op(Op::LocalSet(ENTRY))
            .op(Op::End);
    }
}
