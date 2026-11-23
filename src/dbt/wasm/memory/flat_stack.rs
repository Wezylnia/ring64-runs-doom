use super::{ADDRESS, Emitter, GPR};
use crate::{
    memory::Access,
    x86::{
        context::{ExecutionContext, Mode, Segment},
        instruction::{
            Address, AddressBase, Alu, DataOperand, Instruction, Location, MemoryAluOperands,
            Operand, Operation, Width,
        },
    },
};
use wasm_encoder::{BlockType, Instruction as Op, ValType};

#[derive(Clone, Copy, Default)]
pub(super) struct StackAccess {
    pub(super) read: Option<i64>,
    pub(super) write: Option<i64>,
}

pub(super) struct FlatStackWindow {
    first: i64,
    bytes: u32,
    reads: bool,
    writes: bool,
    access: Vec<StackAccess>,
}

impl FlatStackWindow {
    pub(super) fn recognize(context: &ExecutionContext, group: &[Instruction]) -> Option<Self> {
        if context.mode() != Mode::Protected
            || context.stack_width() != Width::W32
            || context.segment(Segment::Ss).limit != u32::MAX
            || [Access::Read, Access::Write]
                .into_iter()
                .any(|access| context.translate(0, 4, Segment::Ss, access, false).ok() != Some(0))
        {
            return None;
        }
        let mut sp = 0i64;
        let mut first = i64::MAX;
        let mut end = i64::MIN;
        let mut reads = 0;
        let mut writes = 0;
        let mut access = Vec::with_capacity(group.len());
        for instruction in group {
            let offset = |address: Address, width: Width| {
                (width == Width::W32
                    && address.size == Width::W32
                    && address.segment == Segment::Ss
                    && address.base == AddressBase::Register(4)
                    && address.index.is_none())
                .then(|| sp + address.displacement)
            };
            let location = |src, width| match src {
                Location::Memory(address) => offset(address, width),
                Location::Register(_) => None,
            };
            let mut slot = StackAccess::default();
            match instruction.operation {
                Operation::Push {
                    src,
                    width: Width::W32,
                }
                | Operation::Call {
                    target: src,
                    width: Width::W32,
                } => {
                    if let Operand::Memory(address) = src {
                        slot.read = offset(address, Width::W32);
                    }
                    sp -= 4;
                    slot.write = Some(sp);
                }
                Operation::Pop {
                    dst,
                    width: Width::W32,
                } if dst != 4 => {
                    slot.read = Some(sp);
                    sp += 4;
                }
                Operation::Return {
                    cleanup,
                    width: Width::W32,
                } => {
                    slot.read = Some(sp);
                    sp += 4 + i64::from(cleanup);
                }
                Operation::Alu {
                    kind: kind @ (Alu::Add | Alu::Sub),
                    dst,
                    src: DataOperand::Immediate(immediate),
                    width: Width::W32,
                } if dst.bank == 4 => {
                    let delta = i64::from(immediate as i32);
                    sp += if kind == Alu::Add { delta } else { -delta };
                }
                Operation::Alu { kind, dst, .. } if !kind.writes_result() || dst.bank != 4 => {}
                Operation::Load {
                    dst,
                    address,
                    width,
                } if dst.bank != 4 => {
                    slot.read = offset(address, width);
                }
                Operation::Extend {
                    dst,
                    src,
                    source_width,
                    ..
                } if dst.bank != 4 => {
                    slot.read = location(src, source_width);
                }
                Operation::Imul {
                    dst, src, width, ..
                } if dst.bank != 4 => {
                    slot.read = location(src, width);
                }
                Operation::MemoryAlu {
                    kind,
                    operands,
                    address,
                    width,
                } => match operands {
                    MemoryAluOperands::RegisterFirst(dst)
                        if !kind.writes_result() || dst.bank != 4 =>
                    {
                        slot.read = offset(address, width);
                    }
                    MemoryAluOperands::MemoryFirst(_) => {
                        if !kind.writes_result() {
                            slot.read = offset(address, width);
                        }
                    }
                    _ => return None,
                },
                Operation::Store { address, width, .. } => {
                    slot.write = offset(address, width);
                }
                Operation::Mov { dst, .. }
                | Operation::Lea { dst, .. }
                | Operation::Cmov { dst, .. }
                | Operation::Shift {
                    dst: Location::Register(dst),
                    ..
                }
                | Operation::Unary {
                    dst: Location::Register(dst),
                    ..
                } if dst.bank != 4 => {}
                Operation::Nop
                | Operation::Jcc { .. }
                | Operation::Shift {
                    dst: Location::Memory(_),
                    ..
                } => {}
                _ => return None,
            }
            reads += u32::from(slot.read.is_some());
            writes += u32::from(slot.write.is_some());
            for offset in [slot.read, slot.write].into_iter().flatten() {
                first = first.min(offset);
                end = end.max(offset + 4);
            }
            access.push(slot);
        }
        if reads + writes < 3 {
            return None;
        }
        let bytes = u32::try_from(end - first).ok()?;
        for slot in &mut access {
            slot.read = slot.read.map(|offset| offset - first);
            slot.write = slot.write.map(|offset| offset - first);
        }
        Some(Self {
            first,
            bytes,
            reads: reads > 0,
            writes: writes > 0,
            access,
        })
    }

    pub(super) fn access(&self, index: usize) -> StackAccess {
        self.access[index]
    }
}

impl Emitter {
    pub(super) fn guard_flat_stack_window(&mut self, plan: &FlatStackWindow, host: u32) {
        let read = plan
            .reads
            .then(|| self.flat_stack_pointer_span.expect("flat stack read span"));
        let write = plan
            .writes
            .then(|| self.write_spans[1].expect("flat stack write span"));
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0))
            .op(Op::LocalGet(GPR + 4))
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
        // A uniquely mapped interval gives both permissions the same host bytes.
        let span = write.or(read).expect("flat stack span");
        self.op(Op::Drop);
        self.flat_span_pointer(span);
        self.op(Op::LocalSet(host))
            .op(Op::I32Const(1))
            .op(Op::End)
            .op(Op::If(BlockType::Empty));
    }
}
