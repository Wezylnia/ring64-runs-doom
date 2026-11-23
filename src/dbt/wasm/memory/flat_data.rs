use super::{ADDRESS, Emitter, GPR};
use crate::{
    memory::Access,
    x86::{
        context::{ExecutionContext, Mode, Segment},
        instruction::{
            Address, AddressBase, Alu, Count, DataOperand, Instruction, Location,
            MemoryAluOperands, Operand, Operation, Shift, Width,
        },
    },
};
use wasm_encoder::{BlockType, Instruction as Op, ValType};

pub(super) struct FlatDataWindow {
    pub(super) head: usize,
    base: u8,
    first: i64,
    bytes: u32,
    reads: bool,
    writes: bool,
    uses: u32,
}

impl FlatDataWindow {
    pub(super) fn recognize(context: &ExecutionContext, group: &[Instruction]) -> Option<Self> {
        if context.mode() != Mode::Protected
            || context.segment(Segment::Ds).limit != u32::MAX
            || [Access::Read, Access::Write]
                .into_iter()
                .any(|access| context.translate(0, 4, Segment::Ds, access, true).ok() != Some(0))
        {
            return None;
        }
        let mut bounds = [u32::MAX; 16];
        let mut base = None;
        let mut head = 0;
        let mut first = i64::MAX;
        let mut end = i64::MIN;
        let mut reads = false;
        let mut writes = false;
        let mut uses = 0u32;
        for (index, instruction) in group.iter().enumerate() {
            let op = instruction.operation;
            if index > 0
                && let Some((address, read, write)) = data_operand(op)
                && address.segment == Segment::Ds
                && address.size == Width::W32
                && let AddressBase::Register(register) = address.base
                && register != 4
                && base.is_none_or(|base| base == register)
            {
                let offset = address.index.map_or(0, |index| {
                    u64::from(bounds[usize::from(index.register)]) * u64::from(index.scale)
                });
                // Admit a compact envelope, with checked fallback for a wide index.
                if offset < 4096 {
                    if base.is_none() {
                        base = Some(register);
                        head = index;
                    }
                    uses |= 1 << index;
                    first = first.min(address.displacement);
                    end = end.max(address.displacement + offset as i64 + 4);
                    reads |= read;
                    writes |= write;
                }
            }
            let Some(regs) = changed_registers(op) else {
                break;
            };
            if base.is_some_and(|base| regs & (1 << base) != 0) {
                break;
            }
            if regs != 0 {
                let bound = result_bound(op, &bounds);
                for (register, old) in bounds.iter_mut().enumerate() {
                    if regs & (1 << register) != 0 {
                        *old = bound;
                    }
                }
            }
        }
        if uses.count_ones() < 2 || end - first > 4096 {
            return None;
        }
        Some(Self {
            head,
            base: base?,
            first,
            bytes: (end - first) as u32,
            reads,
            writes,
            uses,
        })
    }

    pub(super) fn uses(&self, index: usize) -> bool {
        self.uses & (1 << index) != 0
    }
}

fn data_operand(op: Operation) -> Option<(Address, bool, bool)> {
    match op {
        Operation::Load {
            address,
            width: Width::W32,
            ..
        }
        | Operation::Imul {
            src: Location::Memory(address),
            width: Width::W32,
            ..
        }
        | Operation::Call {
            target: Operand::Memory(address),
            width: Width::W32,
        } => Some((address, true, false)),
        Operation::Store {
            address,
            width: Width::W32,
            ..
        } => Some((address, false, true)),
        Operation::MemoryAlu {
            kind,
            address,
            operands,
            width: Width::W32,
        } => Some((
            address,
            true,
            kind.writes_result() && matches!(operands, MemoryAluOperands::MemoryFirst(_)),
        )),
        Operation::Shift {
            kind: Shift::Rol | Shift::Ror,
            dst: Location::Memory(address),
            width: Width::W32,
            ..
        } => Some((address, true, true)),
        _ => None,
    }
}

fn changed_registers(op: Operation) -> Option<u16> {
    match op {
        Operation::Alu { kind, .. } | Operation::MemoryAlu { kind, .. }
            if !kind.writes_result() =>
        {
            Some(0)
        }
        Operation::Alu { dst, .. }
        | Operation::Lea { dst, .. }
        | Operation::Mov { dst, .. }
        | Operation::Load { dst, .. }
        | Operation::Extend { dst, .. }
        | Operation::Imul { dst, .. }
        | Operation::Cmov { dst, .. }
        | Operation::MemoryAlu {
            operands: MemoryAluOperands::RegisterFirst(dst),
            ..
        }
        | Operation::Shift {
            dst: Location::Register(dst),
            ..
        }
        | Operation::DoubleShift {
            dst: Location::Register(dst),
            ..
        }
        | Operation::Unary {
            dst: Location::Register(dst),
            ..
        } => Some(1 << dst.bank),
        Operation::Pop { dst, .. } => Some((1 << dst) | (1 << 4)),
        Operation::Push { .. } | Operation::Call { .. } | Operation::Return { .. } => Some(1 << 4),
        Operation::MemoryAlu {
            operands: MemoryAluOperands::MemoryFirst(_),
            ..
        }
        | Operation::Store { .. }
        | Operation::Shift {
            dst: Location::Memory(_),
            ..
        }
        | Operation::Nop
        | Operation::Jcc { .. } => Some(0),
        _ => None,
    }
}

fn result_bound(op: Operation, bounds: &[u32; 16]) -> u32 {
    match op {
        Operation::Alu {
            kind: Alu::And,
            dst,
            src: DataOperand::Immediate(mask),
            width: Width::W32,
        } => bounds[usize::from(dst.bank)].min(mask as u32),
        Operation::Shift {
            kind: Shift::Shr,
            dst: Location::Register(dst),
            count: Count::Immediate(count),
            width: Width::W32,
        } => bounds[usize::from(dst.bank)] >> (count & 31),
        Operation::Extend {
            source_width,
            width: Width::W32,
            signed: false,
            ..
        } => source_width.mask() as u32,
        Operation::Mov {
            src: DataOperand::Immediate(value),
            width: Width::W32,
            ..
        } => value as u32,
        Operation::Mov {
            src: DataOperand::Register(src),
            width: Width::W32,
            ..
        } => bounds[usize::from(src.bank)],
        _ => u32::MAX,
    }
}

impl Emitter {
    pub(super) fn guard_flat_data_window(&mut self, plan: &FlatDataWindow, bias: u32) {
        let read = plan
            .reads
            .then(|| self.read_span.expect("flat data read span"));
        let write = plan
            .writes
            .then(|| self.write_spans[0].expect("flat data write span"));
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0))
            .op(Op::LocalGet(GPR + u32::from(plan.base)))
            .op(Op::I32WrapI64)
            .op(Op::I32Const(plan.first as i32))
            .op(Op::I32Add)
            .op(Op::I64ExtendI32U)
            .op(Op::LocalSet(ADDRESS));
        for span in [read, write].into_iter().flatten() {
            self.op(Op::LocalGet(ADDRESS));
            self.flat_span_contains(span, plan.bytes);
            self.op(Op::I32Eqz).op(Op::BrIf(0));
        }
        let span = write.or(read).expect("flat data span");
        self.op(Op::Drop);
        self.flat_span_bias(span);
        self.op(Op::LocalSet(bias))
            .op(Op::I32Const(1))
            .op(Op::End)
            .op(Op::I32Eqz)
            .unlikely(Op::If(BlockType::Empty));
    }
}
