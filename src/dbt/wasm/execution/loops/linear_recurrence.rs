use crate::dbt::wasm::{Emitter, GPR, LHS, REMAINING, RHS, VALUE};
use crate::x86::instruction::{
    Alu, Condition, Count, DataOperand, Instruction, Location, Operation, RegisterView, Shift,
    Width,
};
use wasm_encoder::{BlockType, Instruction as Op};

#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct Recurrence {
    bank: u8,
    counter: RegisterView,
    step: u32,
    lap: u32,
    columns: [u32; 32],
    bias: u32,
}

impl Recurrence {
    pub(in crate::dbt::wasm) fn recognize(instructions: &[Instruction]) -> Option<Self> {
        let [ref prefix @ .., adjust, latch] = *instructions else {
            return None;
        };
        let Operation::Jcc {
            condition: Condition::NotEqual,
            target,
        } = latch.operation
        else {
            return None;
        };
        let Operation::Alu {
            kind,
            dst: counter,
            src: DataOperand::Immediate(imm),
            width: Width::W32,
        } = adjust.operation
        else {
            return None;
        };
        let step = match kind {
            Alu::Sub => imm as u32,
            Alu::Add => (imm as u32).wrapping_neg(),
            _ => return None,
        };
        if prefix.len() < 3
            || target != prefix[0].pc
            || !(1..=1024).contains(&step)
            || !instructions
                .windows(2)
                .all(|pair| pair[0].next_pc == pair[1].pc)
        {
            return None;
        }
        // The accumulator is the only loop-carried input to this body. All
        // temporaries must be defined in this lap before their first use.
        (0..16)
            .filter(|&bank| bank != counter.bank)
            .find_map(|bank| {
                let bias = evaluate(prefix, bank, counter.bank, 0)?;
                let columns = std::array::from_fn(|index| {
                    evaluate(prefix, bank, counter.bank, 1 << index).unwrap() ^ bias
                });
                Some(Self {
                    bank,
                    counter,
                    step,
                    lap: instructions.len() as u32,
                    columns,
                    bias,
                })
            })
    }

    fn square(&mut self) {
        let transform = |mut input: u32| {
            let mut output = 0;
            while input != 0 {
                output ^= self.columns[input.trailing_zeros() as usize];
                input &= input - 1;
            }
            output
        };
        let columns = self.columns.map(transform);
        self.bias ^= transform(self.bias);
        self.columns = columns;
    }
}

fn evaluate(instructions: &[Instruction], bank: u8, counter: u8, input: u32) -> Option<u32> {
    let mut regs = [None; 16];
    regs[usize::from(bank)] = Some(input);
    let mut written = false;
    for instruction in instructions {
        let operand = |src| match src {
            DataOperand::Register(reg) => regs[usize::from(reg.bank)],
            DataOperand::Immediate(value) => Some(value as u32),
        };
        let (dst, result) = match instruction.operation {
            Operation::Mov {
                dst,
                src,
                width: Width::W32,
            } => (dst, operand(src)?),
            Operation::Alu {
                kind: Alu::Xor,
                dst,
                src,
                width: Width::W32,
            } => (dst, regs[usize::from(dst.bank)]? ^ operand(src)?),
            Operation::Shift {
                kind,
                dst: Location::Register(dst),
                count: Count::Immediate(count),
                width: Width::W32,
            } if count & 31 != 0 => {
                let value = regs[usize::from(dst.bank)]?;
                let result = match kind {
                    Shift::Shl => value.wrapping_shl(u32::from(count)),
                    Shift::Shr => value.wrapping_shr(u32::from(count)),
                    _ => return None,
                };
                (dst, result)
            }
            Operation::Nop => continue,
            _ => return None,
        };
        if dst.bank == counter {
            return None;
        }
        written |= dst.bank == bank;
        regs[usize::from(dst.bank)] = Some(result);
    }
    written.then_some(regs[usize::from(bank)]?)
}

impl Emitter {
    pub(in crate::dbt::wasm) fn emit_linear_recurrence(&mut self, mut recurrence: Recurrence) {
        let step = i64::from(recurrence.step);
        let lap = i64::from(recurrence.lap);
        self.data_register(recurrence.counter, Width::W32);
        self.op(Op::LocalTee(RHS))
            .op(Op::I64Const(step))
            .op(Op::I64RemU)
            .op(Op::I64Eqz)
            .op(Op::LocalGet(RHS))
            .op(Op::I64Const(step * 64))
            .op(Op::I64GeU)
            .op(Op::I32And)
            .op(Op::If(BlockType::Empty));
        self.op(Op::LocalGet(RHS))
            .op(Op::I64Const(step))
            .op(Op::I64DivU)
            .op(Op::LocalSet(LHS))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(lap))
            .op(Op::I64DivU)
            .op(Op::LocalSet(VALUE))
            .op(Op::LocalGet(LHS))
            .op(Op::LocalGet(VALUE))
            .op(Op::LocalGet(LHS))
            .op(Op::LocalGet(VALUE))
            .op(Op::I64LtU)
            .op(Op::Select)
            .op(Op::LocalTee(LHS))
            .op(Op::I64Const(64))
            .op(Op::I64GeU)
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Const(1))
            .op(Op::I64Sub)
            .op(Op::LocalSet(LHS));
        // Raise the affine map to the skipped lap count. Only the
        // accumulator and loop count are live across laps. One full original
        // lap remains to reconstruct the remaining registers and FLAGS.
        let register = GPR + u32::from(recurrence.bank);
        for power in 0..32 {
            self.op(Op::LocalGet(LHS))
                .op(Op::I64Const(1 << power))
                .op(Op::I64And)
                .op(Op::I64Eqz)
                .op(Op::I32Eqz)
                .op(Op::If(BlockType::Empty))
                .op(Op::I32Const(recurrence.bias as i32));
            for (bit, value) in recurrence.columns.iter().copied().enumerate() {
                if value != 0 {
                    self.op(Op::I32Const(value as i32))
                        .op(Op::I32Const(0))
                        .op(Op::LocalGet(register))
                        .op(Op::I32WrapI64)
                        .op(Op::I32Const((1u32 << bit) as i32))
                        .op(Op::I32And)
                        .op(Op::Select)
                        .op(Op::I32Xor);
                }
            }
            self.op(Op::I64ExtendI32U);
            self.write_register(RegisterView::low(recurrence.bank), Width::W32);
            self.op(Op::End);
            recurrence.square();
        }
        self.op(Op::LocalGet(RHS))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Const(step))
            .op(Op::I64Mul)
            .op(Op::I64Sub);
        self.write_register(recurrence.counter, Width::W32);
        self.op(Op::LocalGet(REMAINING))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Const(lap))
            .op(Op::I64Mul)
            .op(Op::I64Sub)
            .op(Op::LocalSet(REMAINING))
            .op(Op::End)
            .op(Op::End);
    }
}
