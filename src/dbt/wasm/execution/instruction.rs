#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::ENTRY;
use super::{Emitter, GPR, REASON, loop_cfg};
use crate::embedding::layout::DISPATCH;
use crate::x86::instruction::{Alu, Instruction, Operation};
use wasm_encoder::{BlockType, Instruction as Op};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn indexed_scalar_read(operation: Operation) -> bool {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{Location, MemoryAluOperands, Width};
    let address = match operation {
        Operation::Load { address, .. }
        | Operation::Extend {
            src: Location::Memory(address),
            ..
        } => address,
        Operation::MemoryAlu {
            address,
            kind,
            operands,
            ..
        } if !kind.writes_result() || matches!(operands, MemoryAluOperands::RegisterFirst(_)) => {
            address
        }
        Operation::Pop { .. }
        | Operation::Return {
            width: Width::W64, ..
        } => return true,
        _ => return false,
    };
    matches!(address.segment, Segment::Ds | Segment::Ss)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn indexed_scalar_write(operation: Operation) -> bool {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{Operand, Width};

    matches!(
        operation,
        Operation::Store { address, .. } if matches!(address.segment, Segment::Ds | Segment::Ss)
    ) || matches!(
        operation,
        Operation::Push {
            src: Operand::Register(_) | Operand::Immediate(_),
            width: Width::W64,
        }
    )
}

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

    let (dst, address, source_width, width) = match instruction.operation {
        Operation::Extend {
            dst,
            src: Location::Memory(address),
            source_width: source_width @ (Width::W8 | Width::W16),
            width,
            signed: false,
        } => (dst, address, source_width, width),
        Operation::Load {
            dst,
            address,
            width: Width::W32,
        } => (dst, address, Width::W32, Width::W32),
        _ => return None,
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

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn paged_prefix_wide_read(instruction: Instruction) -> bool {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{MemoryAluOperands, Width};
    matches!(
        instruction.operation,
        Operation::MemoryAlu {
            kind: Alu::Cmp,
            width: Width::W64,
            operands: MemoryAluOperands::RegisterFirst(_),
            address,
        } if matches!(address.segment, Segment::Ds | Segment::Ss)
    )
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn paged_prefix_dword_store(instruction: Instruction) -> bool {
    use crate::x86::{
        context::Segment,
        instruction::{Address, Width},
    };

    matches!(
        instruction.operation,
        Operation::Store {
            address: Address {
                segment: Segment::Ds,
                ..
            },
            width: Width::W32,
            ..
        }
    )
}

impl Emitter {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn uses_indexed_scalar_read(&self, operation: Operation) -> bool {
        self.indexed_scalar_scratch.is_some()
            && !self.guarded_paged_lap
            && indexed_scalar_read(operation)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn uses_indexed_scalar_write(&self, operation: Operation) -> bool {
        self.indexed_store_fill.is_some()
            && !self.guarded_paged_lap
            && indexed_scalar_write(operation)
    }

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
        self.begin(instruction.operation, instruction.next_pc, exit_depth);
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
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if let Some((local, destination)) = self.paged_prefix_fetch_out {
                    // A successful retained read leaves the same fetch newest.
                    // Publish the proof only on this decoded, same-page edge.
                    self.op(Op::I32Const(destination as i32 + 1))
                        .op(Op::LocalSet(local));
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
        write_import: Option<u32>,
        fetch_handoff: Option<(u32, bool, Option<usize>)>,
    ) {
        use super::control::InstructionRetirement;

        let first = instructions.first().unwrap();
        let last = instructions.last().unwrap();
        let leading_wide_read = paged_prefix_wide_read(*first);
        let incoming = fetch_handoff.and_then(|(local, incoming, _)| incoming.then_some(local));
        let read = paged_prefix_read(*first)
            .map(|operation| (read_import.expect("leading read prefix import"), operation));
        let indexed_dword = self.indexed_dword_prefixes
            && instructions.iter().copied().any(paged_prefix_dword_read);
        let indexed_leading = read.is_some_and(|(_, (_, _, width, _))| {
            if width == crate::x86::instruction::Width::W32 {
                indexed_dword
            } else {
                self.sampled_region && incoming.is_none() && self.indexed_paged_read.is_some()
            }
        });
        // A handoff can bypass its guard only directly from a read-only
        // prefix in this unit. Request the same authority at every real
        // guard in the chain, including register-only predecessors.
        self.guard_straight_paged_prefix(
            instructions,
            guard,
            incoming,
            indexed_leading || self.indexed_dword_prefixes,
            true,
        );
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
            if indexed_leading {
                self.indexed_prefix_operand_host(read, source_width.bytes());
                self.op(Op::LocalTee(STORE_STATUS))
                    .op(Op::If(BlockType::Empty))
                    .op(Op::LocalGet(STORE_STATUS));
            } else {
                let check_alignment = source_width != Width::W8 && self.context.cpl() == 3;
                if check_alignment {
                    // A checked CPL3 load owns any naturally-unaligned #AC decision.
                    self.op(Op::LocalTee(ADDRESS))
                        .op(Op::I64Const(i64::from(source_width.bytes() - 1)))
                        .op(Op::I64And)
                        .op(Op::I64Eqz)
                        .op(Op::If(BlockType::Result(ValType::I32)))
                        .op(Op::LocalGet(ADDRESS));
                }
                let segment = if source_width == Width::W32 {
                    crate::embedding::layout::paging::PREFIX_DWORD_READ_SEGMENT
                } else {
                    address.segment as u32
                };
                self.op(Op::I32Const(i32::from(source_width.bytes())))
                    .op(Op::I32Const(segment as i32))
                    .op(Op::Call(read));
                if check_alignment {
                    self.op(Op::Else).op(Op::I32Const(0)).op(Op::End);
                }
                self.op(Op::LocalTee(STORE_STATUS))
                    .op(Op::If(BlockType::Empty))
                    .op(Op::LocalGet(STORE_STATUS))
                    .op(Op::I32Load(mem(
                        std::mem::offset_of!(crate::memory::DirectSpan, host) as u64,
                    )));
            }
            self.op(match source_width {
                Width::W8 => Op::I64Load8U(mem(0)),
                Width::W16 => Op::I64Load16U(mem(0)),
                Width::W32 => Op::I64Load32U(mem(0)),
                Width::W64 => unreachable!("unsupported prefix read width"),
            });
            self.write_register(register, width);
            self.commit(first.next_pc);
        }
        if leading_wide_read {
            // A first-operand refusal drops directly to the checked entry.
            self.op(Op::Block(BlockType::Empty));
        }
        let loop_depth = loop_depth + 2 + u32::from(read.is_some() || leading_wide_read);
        self.guarded_paged_lap = true;
        self.loop_gates_prechecked = true;
        self.paged_prefix_fetch_out =
            fetch_handoff.and_then(|(local, _, outgoing)| outgoing.map(|target| (local, target)));
        let guarded = &instructions[usize::from(read.is_some())..];
        let mut retired_read = false;
        for (index, instruction) in guarded.iter().enumerate() {
            let wide = paged_prefix_wide_read(*instruction);
            let fuse_branch = wide && guarded.get(index + 1).is_some_and(|next| {
                matches!(next.operation, Operation::Jcc { target, .. } if crate::memory::canonical48(target))
            });
            self.instruction_retirement = if fuse_branch {
                InstructionRetirement::DeferredCommit
            } else if retired_read {
                InstructionRetirement::AfterPrefix(1)
            } else {
                InstructionRetirement::One
            };
            retired_read = fuse_branch;
            self.paged_prefix_read = read_import
                .filter(|_| wide || paged_prefix_dword_read(*instruction))
                .map(|read| {
                    (
                        read,
                        !wide && indexed_dword,
                        leading_wide_read && index == 0,
                    )
                });
            self.paged_prefix_store =
                write_import.filter(|_| paged_prefix_dword_store(*instruction));
            self.emit_instruction(*instruction, None, loop_depth, None, None, false);
        }
        self.instruction_retirement = InstructionRetirement::One;
        self.paged_prefix_read = None;
        self.paged_prefix_store = None;
        self.paged_prefix_fetch_out = None;
        self.loop_gates_prechecked = false;
        self.route(last.next_pc, loop_depth, loop_depth + 1);
        self.guarded_paged_lap = false;
        if read.is_some() || leading_wide_read {
            self.op(Op::End);
        }
        self.op(Op::End).op(Op::End);
        if let Some(local) = incoming {
            // A stop or short budget can reject the group before it consumes
            // the handoff. The checked copy cannot carry that proof forward.
            self.op(Op::I32Const(0)).op(Op::LocalSet(local));
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn indexed_prefix_operand_host(&mut self, read: u32, width: u8) {
        use super::{ADDRESS, STORE_STATUS};
        use wasm_encoder::{MemArg, ValType};

        self.op(Op::LocalSet(ADDRESS));
        self.indexed_read_host(
            self.indexed_paged_read.expect("indexed prefix locals"),
            width,
            true,
            false,
        );
        self.op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Result(ValType::I32)))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::Else)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32Const(i32::from(width)))
            .op(Op::I32Const(
                crate::embedding::layout::paging::PREFIX_INDEXED_READ_SEGMENT as i32,
            ))
            .op(Op::Call(read))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Result(ValType::I32)))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(MemArg {
                offset: std::mem::offset_of!(crate::memory::DirectSpan, host) as u64,
                align: 0,
                memory_index: 0,
            }))
            .op(Op::Else)
            .op(Op::I32Const(0))
            .op(Op::End)
            .op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn indexed_read_host(
        &mut self,
        [table, entry, data, fetch]: [u32; 4],
        width: u8,
        preserve_fetch: bool,
        write: bool,
    ) {
        use super::{ADDRESS, LHS, STORE_STATUS, VALUE};
        use crate::embedding::layout::StateStorage;
        use crate::x86::paging::cache::{
            CAPACITY, INDEXED_READ_ENTRIES, IndexedReadEntry, IndexedReadTable,
        };
        use std::mem::{offset_of, size_of};
        use wasm_encoder::{MemArg, ValType};

        let mem = |offset, align| MemArg {
            offset: offset as u64,
            align,
            memory_index: 0,
        };
        let clock = mem(offset_of!(IndexedReadTable, clock), 3);
        let ages = mem(offset_of!(IndexedReadTable, ages), 3);
        let live_ids = mem(offset_of!(IndexedReadTable, live_ids), 3);
        let (record_base, records) = if write {
            (0, offset_of!(StateStorage, indexed_paged_writes))
        } else {
            (table, offset_of!(IndexedReadTable, entries))
        };
        let [id, stamp] = if preserve_fetch {
            [LHS, VALUE]
        } else {
            self.indexed_scalar_scratch.unwrap()
        };
        // Keep a zero result beneath the inner checks. An invalid authority
        // exits the inner block without touching recency or operand memory.
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0))
            .op(Op::Block(BlockType::Empty))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(mem(
                offset_of!(StateStorage, indexed_paged_reads),
                2,
            )))
            .op(Op::LocalTee(table))
            .op(Op::I32Eqz)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Const(4095))
            .op(Op::I32And)
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Const(4096 - i32::from(width)))
            .op(Op::I32GtU)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(record_base))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Const(12))
            .op(Op::I32ShrU)
            .op(Op::I32Const(INDEXED_READ_ENTRIES as i32 - 1))
            .op(Op::I32And)
            .op(Op::I32Const(size_of::<IndexedReadEntry>() as i32))
            .op(Op::I32Mul)
            .op(Op::I32Add)
            .op(Op::I32Const(records as i32))
            .op(Op::I32Add)
            .op(Op::LocalTee(entry))
            .op(Op::I64Load(mem(offset_of!(IndexedReadEntry, page), 3)))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I64Const(!4095))
            .op(Op::I64And)
            .op(Op::I64Ne)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(entry))
            .op(Op::I64Load(mem(offset_of!(IndexedReadEntry, id), 3)))
            .op(Op::LocalTee(id))
            .op(Op::I64Eqz)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(entry))
            .op(Op::I32Load(mem(offset_of!(IndexedReadEntry, slot), 2)));
        if preserve_fetch {
            self.op(Op::LocalSet(data))
                .op(Op::LocalGet(table))
                .op(Op::I32Load(mem(offset_of!(IndexedReadTable, hint), 2)))
                .op(Op::LocalSet(fetch));
            for slot in [data, fetch] {
                self.op(Op::LocalGet(slot))
                    .op(Op::I32Const(CAPACITY as i32))
                    .op(Op::I32GeU)
                    .op(Op::BrIf(0))
                    .op(Op::LocalGet(table))
                    .op(Op::LocalGet(slot))
                    .op(Op::I32Const(3))
                    .op(Op::I32Shl)
                    .op(Op::I32Add)
                    .op(Op::LocalSet(slot));
            }
            self.op(Op::LocalGet(data))
                .op(Op::I64Load(live_ids))
                .op(Op::LocalGet(id))
                .op(Op::I64Ne)
                .op(Op::BrIf(0))
                .op(Op::LocalGet(fetch))
                .op(Op::I64Load(live_ids))
                .op(Op::I64Eqz)
                .op(Op::BrIf(0))
                .op(Op::LocalGet(table))
                .op(Op::I64Load(clock))
                .op(Op::LocalTee(stamp))
                .op(Op::I64Const(-3))
                .op(Op::I64GtU)
                .op(Op::BrIf(0))
                .op(Op::LocalGet(fetch))
                .op(Op::I64Load(ages))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Ne)
                .op(Op::BrIf(0));

            // Match data_translation_then_fetch. Leave the hot data/fetch order
            // untouched if it already matches and otherwise install both ages.
            self.op(Op::LocalGet(data))
                .op(Op::LocalGet(fetch))
                .op(Op::I32Ne)
                .op(Op::LocalGet(data))
                .op(Op::I64Load(ages))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Const(1))
                .op(Op::I64Sub)
                .op(Op::I64Ne)
                .op(Op::I32And)
                .op(Op::If(BlockType::Empty));
            for (slot, advance) in [(data, 1), (fetch, 2)] {
                self.op(Op::LocalGet(slot))
                    .op(Op::LocalGet(stamp))
                    .op(Op::I64Const(advance))
                    .op(Op::I64Add)
                    .op(Op::I64Store(ages));
            }
            self.op(Op::LocalGet(table))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Const(2))
                .op(Op::I64Add)
                .op(Op::I64Store(clock))
                .op(Op::End);
        } else {
            // A checked scalar operand touches only the data leaf.
            let hint = mem(offset_of!(IndexedReadTable, hint), 2);
            self.op(Op::LocalTee(data))
                .op(Op::I32Const(CAPACITY as i32))
                .op(Op::I32GeU)
                .op(Op::BrIf(0))
                .op(Op::LocalGet(table))
                .op(Op::LocalGet(data))
                .op(Op::I32Const(3))
                .op(Op::I32Shl)
                .op(Op::I32Add)
                .op(Op::LocalTee(fetch))
                .op(Op::I64Load(live_ids))
                .op(Op::LocalGet(id))
                .op(Op::I64Ne)
                .op(Op::BrIf(0))
                .op(Op::LocalGet(table))
                .op(Op::I64Load(clock))
                .op(Op::LocalTee(stamp))
                .op(Op::I64Const(-1))
                .op(Op::I64Eq)
                .op(Op::BrIf(0))
                .op(Op::LocalGet(table))
                .op(Op::LocalGet(data))
                .op(Op::I32Store(hint))
                .op(Op::LocalGet(fetch))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Const(1))
                .op(Op::I64Add)
                .op(Op::LocalTee(stamp))
                .op(Op::I64Store(ages))
                .op(Op::LocalGet(table))
                .op(Op::LocalGet(stamp))
                .op(Op::I64Store(clock));
        }
        self.op(Op::LocalGet(entry))
            .op(Op::I32Load(mem(offset_of!(IndexedReadEntry, host), 2)))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Add)
            .op(Op::Br(1))
            .op(Op::End)
            .op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn newest_prefix_fetch(&mut self, pc: u64) {
        use super::{SHADOW, STORE_STATUS};
        use crate::embedding::layout::debug::{Continuation, DebugState};
        use crate::embedding::layout::paging::NewestFetch;
        use crate::embedding::layout::{RawEventState, RawInterruptState, RunState, StateStorage};
        use crate::x86::paging::cache::IndexedReadTable;
        use std::mem::offset_of;
        use wasm_encoder::{MemArg, ValType};

        let mem = |offset, align| MemArg {
            offset: offset as u64,
            align,
            memory_index: 0,
        };
        let field = |offset, align| mem(offset_of!(StateStorage, newest_fetch) + offset, align);
        let table = field(offset_of!(NewestFetch, table), 2);
        let slot = field(offset_of!(NewestFetch, slot), 2);
        let clock = mem(offset_of!(IndexedReadTable, clock), 3);
        let debug = offset_of!(RunState, debug);
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0))
            .op(Op::Block(BlockType::Empty))
            .op(Op::LocalGet(0))
            .op(Op::I64Load(field(offset_of!(NewestFetch, tag), 3)))
            .op(Op::I64Const(((pc & !4095) | 1) as i64))
            .op(Op::I64Ne)
            .op(Op::BrIf(0));
        self.system_bits();
        self.op(Op::I64Const(0x10100))
            .op(Op::I64And)
            .op(Op::I64Eqz)
            .op(Op::I32Eqz)
            .op(Op::LocalGet(SHADOW))
            .op(Op::I32Or)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(field(
                offset_of!(NewestFetch, source_invalid),
                2,
            )))
            .op(Op::I32Load8U(mem(0, 0)))
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(field(
                offset_of!(NewestFetch, cache_changed),
                2,
            )))
            .op(Op::I32Load8U(mem(0, 0)))
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load8U(mem(
                debug
                    + offset_of!(DebugState, registers)
                    + offset_of!(crate::x86::debug::State, dr7),
                0,
            )))
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(mem(
                debug + offset_of!(DebugState, continuation) + offset_of!(Continuation, slots),
                2,
            )))
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(mem(
                offset_of!(RunState, event) + offset_of!(RawEventState, kind),
                2,
            )))
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I64Load(mem(offset_of!(RunState, interrupts), 2)))
            .op(Op::I64Eqz)
            .op(Op::I32Eqz)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(mem(
                offset_of!(RunState, interrupts) + offset_of!(RawInterruptState, nmi_blocked),
                2,
            )))
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(table))
            .op(Op::I32Load(mem(offset_of!(IndexedReadTable, hint), 2)))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(slot))
            .op(Op::I32Ne)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(table))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(slot))
            .op(Op::I32Const(3))
            .op(Op::I32Shl)
            .op(Op::I32Add)
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I64Load(mem(offset_of!(IndexedReadTable, live_ids), 3)))
            .op(Op::LocalGet(0))
            .op(Op::I64Load(field(offset_of!(NewestFetch, id), 3)))
            .op(Op::I64Ne)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(table))
            .op(Op::I64Load(clock))
            .op(Op::I64Const(-1))
            .op(Op::I64Eq)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(table))
            .op(Op::I64Load(clock))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load(mem(offset_of!(IndexedReadTable, ages), 3)))
            .op(Op::I64Ne)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Const(crate::x86::context::Segment::Cs as i32))
            .op(Op::I32Store(mem(offset_of!(RunState, fault_segment), 2)))
            .op(Op::I32Const(1))
            .op(Op::Br(1))
            .op(Op::End)
            .op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn guard_straight_paged_prefix(
        &mut self,
        instructions: &[Instruction],
        guard: u32,
        handoff: Option<u32>,
        indexed_read: bool,
        check_newest: bool,
    ) {
        use super::SHADOW;

        let first = &instructions[0];
        let last = instructions.last().unwrap();
        self.guard_straight_prefix(first, last, instructions.len() as u32);
        self.clear_quiet_fetch();
        if let Some(local) = handoff {
            let expected = self.current_index as i32 + 1;
            self.op(Op::LocalGet(local))
                .op(Op::I32Const(expected))
                .op(Op::I32Eq)
                .op(Op::I32Const(0))
                .op(Op::LocalSet(local))
                .op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                .op(Op::I32Const(1))
                .op(Op::Else);
        }
        let newest = check_newest
            && self.sampled_region
            && self.context.is_long64()
            && self.context.cpl() == 0
            && !indexed_read
            && last.next_pc > first.pc
            && (last.next_pc - 1) & !4095 == first.pc & !4095;
        if newest {
            self.newest_prefix_fetch(first.pc);
            self.op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                .op(Op::I32Const(1))
                .op(Op::Else);
        }
        self.op(Op::I64Const(first.pc as i64))
            .op(Op::I32Const((last.next_pc - first.pc) as i32));
        self.system_bits();
        self.op(Op::LocalGet(SHADOW));
        if indexed_read {
            self.op(Op::I32Const(
                crate::embedding::layout::paging::INDEXED_PAGED_READ_GUARD as i32,
            ))
            .op(Op::I32Or);
        }
        if newest {
            self.op(Op::I32Const(
                crate::embedding::layout::paging::NEWEST_FETCH_GUARD as i32,
            ))
            .op(Op::I32Or);
        }
        self.op(Op::Call(guard));
        if newest {
            self.op(Op::End);
        }
        if handoff.is_some() {
            self.op(Op::End);
        }
        self.op(Op::I32Const(1))
            .op(Op::I32Eq)
            .op(Op::If(BlockType::Empty));
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn guard_straight_prefix(
        &mut self,
        first: &Instruction,
        last: &Instruction,
        count: u32,
    ) {
        use super::REMAINING;

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
    }
}
