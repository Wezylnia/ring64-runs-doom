#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::ENTRY;
use super::{Emitter, GPR, REASON, loop_cfg};
use crate::embedding::layout::DISPATCH;
use crate::x86::instruction::{Alu, Instruction, Operation};
use wasm_encoder::{BlockType, Instruction as Op};

impl Emitter {
    pub(super) fn instruction(
        &mut self,
        instruction: Instruction,
        next: Option<&Instruction>,
        loop_depth: u32,
        write_segment_base: Option<u32>,
        compare_source: Option<loop_cfg::ConditionSource>,
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
            Operation::X87(x87) => self.x87(x87, instruction.pc, instruction.next_pc, exit_depth),
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
        self.route(instruction.next_pc, loop_depth, exit_depth);
    }
}
