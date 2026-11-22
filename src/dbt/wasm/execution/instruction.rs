#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::ENTRY;
use super::{Emitter, GPR, REASON, loop_cfg};
use crate::embedding::layout::DISPATCH;
use crate::x86::instruction::{Alu, Instruction, Operation};
use wasm_encoder::{BlockType, Instruction as Op};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn paged_prefix_read(
    instruction: Instruction,
) -> Option<(
    crate::x86::instruction::RegisterView,
    crate::x86::instruction::Address,
    crate::x86::instruction::Width,
    crate::x86::instruction::Width,
)> {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{Location, Width};

    let Operation::Extend {
        dst,
        src: Location::Memory(address),
        source_width: source_width @ (Width::W8 | Width::W16),
        width,
        signed: false,
    } = instruction.operation
    else {
        return None;
    };
    (address.segment == Segment::Ds).then_some((dst, address, source_width, width))
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn paged_prefix_dword_read(instruction: Instruction) -> bool {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{Location, MemoryAluOperands, Width};

    let address = match instruction.operation {
        Operation::Load {
            address,
            width: Width::W32,
            ..
        }
        | Operation::Imul {
            src: Location::Memory(address),
            width: Width::W32,
            ..
        } => address,
        Operation::MemoryAlu {
            kind,
            address,
            operands,
            width: Width::W32,
        } if matches!(operands, MemoryAluOperands::RegisterFirst(_)) || !kind.writes_result() => {
            address
        }
        _ => return false,
    };
    address.segment == Segment::Ds
}

impl Emitter {
    pub(super) fn instruction(
        &mut self,
        instruction: Instruction,
        next: Option<&Instruction>,
        loop_depth: u32,
        write_segment_base: Option<u32>,
        compare_source: Option<loop_cfg::ConditionSource>,
    ) {
        self.emit_instruction(
            instruction,
            next,
            loop_depth,
            write_segment_base,
            compare_source,
            true,
        );
    }

    fn emit_instruction(
        &mut self,
        instruction: Instruction,
        next: Option<&Instruction>,
        loop_depth: u32,
        write_segment_base: Option<u32>,
        compare_source: Option<loop_cfg::ConditionSource>,
        route: bool,
    ) {
        self.current_pc = instruction.pc;
        self.current_operation = instruction.operation;
        self.stores = false;
        let exit_depth = loop_depth + 1;
        if !self.loop_gates_prechecked {
            self.gates(exit_depth);
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        self.parallel_boundary(exit_depth);
        self.begin(instruction.operation, exit_depth);
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        let exit_depth = self.begin_parallel_instruction(instruction, exit_depth);
        match instruction.operation {
            Operation::ProtectionKey(operation) => self.protection_key(operation, exit_depth),
            Operation::Processor(operation) => {
                self.processor(operation, exit_depth);
                if operation == crate::x86::processor::Instruction::Cpuid {
                    self.commit(instruction.next_pc);
                    if self.finish_instruction(exit_depth) {
                        return;
                    }
                    self.external_boundary(exit_depth);
                    self.gates(exit_depth);
                    self.exit(DISPATCH, exit_depth);
                    return;
                }
            }
            Operation::FastSystem(operation) => {
                self.fast_system(operation, instruction.next_pc, exit_depth);
                return;
            }
            Operation::SegmentBase {
                segment,
                register,
                width,
                write,
            } => {
                if write {
                    self.op(Op::I32Const(segment as i32));
                    self.register(register, width);
                    self.op(Op::Call(
                        write_segment_base.expect("segment base write import"),
                    ))
                    .op(Op::LocalTee(REASON))
                    .op(Op::BrIf(exit_depth));
                } else {
                    let base = self.context.segment_base(segment) & width.mask();
                    self.op(Op::I64Const(base as i64))
                        .op(Op::LocalSet(GPR + u32::from(register)));
                }
            }
            Operation::Paging(operation) => {
                self.paging_control(operation, instruction.next_pc, exit_depth);
                return;
            }
            Operation::Halt => {
                self.halt(instruction.next_pc, exit_depth);
                return;
            }
            Operation::SoftwareEvent(operation) => {
                if self.software_event(operation, instruction.next_pc, exit_depth) {
                    return;
                }
            }
            Operation::DebugRegister {
                register,
                gpr,
                write,
                width,
            } => {
                self.debug_register(register, gpr, write, width, instruction.next_pc, exit_depth);
                return;
            }
            Operation::SystemFlags(operation) => self.system_flags(operation, exit_depth),
            Operation::PopSs { width } => self.pop_ss(width, exit_depth),
            Operation::MaskedStore {
                data,
                mask,
                mmx,
                address,
            } => self.masked_store(data, mask, mmx, address, instruction.next_pc, exit_depth),
            Operation::Far(far) => {
                self.far_control(far, instruction.next_pc, exit_depth);
                return;
            }
            Operation::Protection(operation) => {
                self.protection(operation, instruction.next_pc, exit_depth)
            }
            Operation::Clflush { address } => {
                self.clflush(address, instruction.next_pc, exit_depth)
            }
            Operation::Io { input, port, width } => self.io(input, port, width, exit_depth),
            Operation::Nop | Operation::X87CompatibilityNop => {}
            Operation::Mmx { op, address } => {
                self.mmx(op, address, instruction.next_pc, exit_depth)
            }
            Operation::NumericImage { op, address } => {
                self.numeric_image(op, address, instruction.next_pc, exit_depth)
            }
            Operation::X87(x87) => {
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if let Some(second) = next {
                    self.emit_x87_register_pair(instruction, *second, loop_depth, exit_depth);
                }
                self.x87(x87, instruction.pc, instruction.next_pc, exit_depth);
            }
            Operation::Float(float) => self.float(float, instruction.next_pc, exit_depth),
            Operation::Mxcsr { address, store } => {
                self.mxcsr(address, store, instruction.next_pc, exit_depth)
            }
            Operation::Vector(vector) => self.vector(vector, instruction.next_pc, exit_depth),
            Operation::Exchange { dst, src, width } => {
                self.exchange(dst, src, width, instruction.next_pc, exit_depth)
            }
            Operation::Xadd { dst, src, width } => {
                self.xadd(dst, src, width, instruction.next_pc, exit_depth)
            }
            Operation::CompareExchange { dst, src, width } => {
                self.compare_exchange(dst, src, width, instruction.next_pc, exit_depth)
            }
            Operation::CompareExchange8b { address } => {
                self.compare_exchange8b(address, instruction.next_pc, exit_depth)
            }
            Operation::CompareExchange16b { address } => {
                self.compare_exchange16b(address, instruction.next_pc, exit_depth)
            }
            Operation::Direction { set } => self.direction(set),
            Operation::String {
                kind,
                width,
                address_width,
                source_segment,
                repeat,
            } => {
                self.string(
                    kind,
                    (width, address_width),
                    source_segment,
                    repeat,
                    exit_depth,
                );
            }
            Operation::Mov { dst, src, width } => {
                self.data_operand(src, width);
                self.write_register(dst, width);
            }
            Operation::Alu {
                kind,
                dst,
                src,
                width,
            } => {
                let overwrite = !self.observed
                    && !cfg!(feature = "parallel")
                    && matches!(kind, Alu::Add | Alu::Sub)
                    && next.is_some_and(|next| {
                        next.pc == instruction.next_pc
                            && matches!(
                                next.operation,
                                Operation::Alu {
                                    kind: Alu::Cmp | Alu::Test,
                                    ..
                                }
                            )
                    });
                self.integer(
                    kind,
                    dst,
                    src,
                    width,
                    overwrite.then_some(instruction.next_pc),
                );
            }
            Operation::MemoryAlu {
                kind,
                address,
                operands,
                width,
            } => self.memory_integer(
                kind,
                address,
                operands,
                width,
                instruction.next_pc,
                exit_depth,
            ),
            Operation::Load {
                dst,
                address,
                width,
            } => {
                if self.mmio_operand.is_some() {
                    self.mmio_move(
                        super::mmio::Move::Read(dst),
                        address,
                        width,
                        instruction.next_pc,
                        exit_depth,
                    );
                } else {
                    self.load(dst, address, width, instruction.next_pc, exit_depth);
                }
            }
            Operation::Store {
                src,
                address,
                width,
            } => {
                if self.mmio_operand.is_some() {
                    self.mmio_move(
                        super::mmio::Move::Write(src),
                        address,
                        width,
                        instruction.next_pc,
                        exit_depth,
                    );
                } else {
                    self.store(src, address, width, instruction.next_pc, exit_depth);
                }
            }
            Operation::NonTemporalStore {
                src,
                address,
                width,
            } => {
                self.store(src, address, width, instruction.next_pc, exit_depth);
            }
            Operation::Lea {
                dst,
                address,
                width,
            } => self.lea(dst, address, width, instruction.next_pc),
            Operation::Extend {
                dst,
                src,
                source_width,
                width,
                signed,
            } => self.extend(
                dst,
                src,
                (source_width, width),
                signed,
                instruction.next_pc,
                exit_depth,
            ),
            Operation::SignHigh { width } => self.sign_high(width),
            Operation::Unary { kind, dst, width } => {
                self.unary(kind, dst, width, instruction.next_pc, exit_depth)
            }
            Operation::Multiply { signed, src, width } => {
                self.multiply(signed, src, width, instruction.next_pc, exit_depth)
            }
            Operation::Imul {
                dst,
                src,
                immediate,
                width,
            } => self.imul(dst, src, immediate, width, instruction.next_pc, exit_depth),
            Operation::Divide { signed, src, width } => {
                self.divide(signed, src, width, instruction.next_pc, exit_depth)
            }
            Operation::Shift {
                kind,
                dst,
                count,
                width,
            } => self.shift(kind, dst, count, width, instruction.next_pc, exit_depth),
            Operation::DoubleShift {
                kind,
                dst,
                src,
                count,
                width,
            } => self.double_shift(
                kind,
                dst,
                (src, count),
                width,
                instruction.next_pc,
                exit_depth,
            ),
            Operation::Carry { kind } => self.carry(kind),
            Operation::Lahf => self.lahf(),
            Operation::Sahf => self.sahf(),
            Operation::Bit {
                kind,
                dst,
                index,
                width,
            } => self.bit(kind, dst, index, width, instruction.next_pc, exit_depth),
            Operation::Scan {
                reverse,
                dst,
                src,
                width,
            } => self.scan(reverse, dst, src, width, instruction.next_pc, exit_depth),
            Operation::Bmi {
                kind,
                dst,
                src,
                width,
            } => self.bmi(kind, dst, src, width, instruction.next_pc, exit_depth),
            Operation::Bswap { dst, width } => self.bswap(dst, width),
            Operation::Setcc { dst, condition } => {
                self.setcc(dst, condition, instruction.next_pc, exit_depth);
            }
            Operation::Cmov {
                dst,
                src,
                width,
                condition,
            } => self.cmov(
                dst,
                src,
                width,
                (condition, compare_source),
                instruction.next_pc,
                exit_depth,
            ),
            Operation::Push { src, width } => {
                self.push(src, width, instruction.next_pc, exit_depth)
            }
            Operation::Pop { dst, width } => self.pop(dst, width, exit_depth),
            Operation::PopMemory { address, width } => {
                self.pop_memory(address, width, instruction.next_pc, exit_depth)
            }
            Operation::Leave { width } => self.leave(width, exit_depth),
            Operation::CompoundStack(operation) => self.compound_stack(operation, exit_depth),
            Operation::Call { target, width } => {
                self.call(target, width, instruction.next_pc, loop_depth, exit_depth);
                return;
            }
            Operation::JumpRegister { target, width } => {
                self.jump_register(target, width, exit_depth);
                return;
            }
            Operation::JumpMemory { address, width } => {
                self.jump_memory(address, width, instruction.next_pc, exit_depth);
                return;
            }
            Operation::Return { cleanup, width } => {
                self.return_stack(cleanup, width, exit_depth);
                return;
            }
            Operation::Jump { target } => {
                self.target(target, loop_depth, exit_depth);
                return;
            }
            Operation::Jcc { condition, target } => {
                self.cloned_condition(condition, compare_source);
                self.op(Op::If(BlockType::Empty));
                if let Some(loop_cfg::ConditionSource::OperandsWithTakenFlags(width)) =
                    compare_source
                {
                    self.arithmetic_result(Alu::Cmp, width);
                    self.arithmetic_flags(Alu::Cmp, width);
                }
                self.target(target, loop_depth + 1, exit_depth + 1);
                self.op(Op::End);
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if let Some((tail, retry)) = self.proved_read_retry
                    && self.current_index == tail
                    && self.structured_loop.is_none()
                {
                    // Falling out of the resumed lap loses its stream proof.
                    self.op(Op::I32Const(0)).op(Op::LocalSet(retry));
                }
            }
            Operation::CountBranch {
                condition,
                count_width,
                target,
            } => {
                self.count_branch(condition, count_width, target, loop_depth, exit_depth);
            }
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        let exit_depth = self.end_parallel_instruction(instruction, exit_depth);
        self.commit(instruction.next_pc);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.float_outline_params != 0
            && let Some((body, _)) = self.structured_loop
            && self.current_index == body.tail
        {
            self.op(Op::I32Const((body.tail + 1) as i32))
                .op(Op::LocalSet(ENTRY))
                .op(Op::Br(1));
            return;
        }
        if route {
            self.route(instruction.next_pc, loop_depth, exit_depth);
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn emit_guarded_register_prefix(
        &mut self,
        instructions: &[Instruction],
        loop_depth: u32,
        guard: u32,
        read_import: Option<u32>,
    ) {
        let first = instructions.first().unwrap();
        let last = instructions.last().unwrap();
        self.guard_straight_paged_prefix(first, last, instructions.len() as u32, guard);
        let read = paged_prefix_read(*first)
            .map(|operation| (read_import.expect("leading read prefix import"), operation));
        if let Some((read, (register, address, source_width, width))) = read {
            use super::{ADDRESS, STORE_STATUS};
            use crate::x86::instruction::Width;
            use wasm_encoder::{MemArg, ValType};
            let mem = |offset| MemArg {
                offset,
                align: 0,
                memory_index: 0,
            };

            self.address(address, first.next_pc);
            let check_alignment = source_width == Width::W16 && self.context.cpl() == 3;
            if check_alignment {
                // A checked CPL3 load owns any naturally-unaligned #AC decision.
                self.op(Op::LocalTee(ADDRESS))
                    .op(Op::I64Const(1))
                    .op(Op::I64And)
                    .op(Op::I64Eqz)
                    .op(Op::If(BlockType::Result(ValType::I32)))
                    .op(Op::LocalGet(ADDRESS));
            }
            self.op(Op::I32Const(i32::from(source_width.bytes())))
                .op(Op::I32Const(address.segment as i32))
                .op(Op::Call(read));
            if check_alignment {
                self.op(Op::Else).op(Op::I32Const(0)).op(Op::End);
            }
            self.op(Op::LocalTee(STORE_STATUS))
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::I32Load(mem(
                    std::mem::offset_of!(crate::memory::DirectSpan, host) as u64,
                )))
                .op(if source_width == Width::W8 {
                    Op::I64Load8U(mem(0))
                } else {
                    Op::I64Load16U(mem(0))
                });
            self.write_register(register, width);
            self.commit(first.next_pc);
        }
        let loop_depth = loop_depth + 2 + u32::from(read.is_some());
        self.guarded_paged_lap = true;
        self.loop_gates_prechecked = true;
        for instruction in &instructions[usize::from(read.is_some())..] {
            self.paged_prefix_read = read_import.filter(|_| paged_prefix_dword_read(*instruction));
            self.emit_instruction(*instruction, None, loop_depth, None, None, false);
        }
        self.paged_prefix_read = None;
        self.loop_gates_prechecked = false;
        self.route(last.next_pc, loop_depth, loop_depth + 1);
        self.guarded_paged_lap = false;
        if read.is_some() {
            self.op(Op::End);
        }
        self.op(Op::End).op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn guard_straight_paged_prefix(
        &mut self,
        first: &Instruction,
        last: &Instruction,
        count: u32,
        guard: u32,
    ) {
        use super::{REMAINING, SHADOW};

        self.op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(count)))
            .op(Op::I64GeU)
            .op(Op::LocalGet(3))
            .op(Op::LocalGet(2))
            .op(Op::I64Const(first.pc as i64))
            .op(Op::I64GeU)
            .op(Op::I32And)
            .op(Op::LocalGet(2))
            .op(Op::I64Const(last.pc as i64))
            .op(Op::I64LeU)
            .op(Op::I32And)
            .op(Op::I32Eqz)
            .op(Op::I32And)
            .op(Op::If(BlockType::Empty));
        self.op(Op::I64Const(first.pc as i64))
            .op(Op::I32Const((last.next_pc - first.pc) as i32));
        self.system_bits();
        self.op(Op::LocalGet(SHADOW))
            .op(Op::Call(guard))
            .op(Op::I32Const(1))
            .op(Op::I32Eq)
            .op(Op::If(BlockType::Empty));
    }
}
