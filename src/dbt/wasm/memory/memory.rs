use super::{ADDRESS, Emitter, GPR, REASON, RESULT, STORE_STATUS, VALUE};
use crate::embedding::layout::{DATA, RunState};
use crate::x86::context::Segment;
use crate::x86::instruction::{Address, AddressBase, DataOperand, RegisterView, Width};
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
// Mapping edits happen between invocations, so the cached span lives in Wasm locals.
pub(super) struct ReadSpan {
    resolve: u32,
    base: u32,
    length: u32,
    host: u32,
    descriptor: u32,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
impl ReadSpan {
    pub(super) fn new(resolve: u32, first: u32) -> Self {
        Self {
            resolve,
            base: first,
            length: first + 1,
            host: first + 2,
            descriptor: first + 3,
        }
    }
}

impl Emitter {
    pub(super) fn address(&mut self, address: Address, next_pc: u64) {
        self.address_with_stack(address, next_pc, None);
    }

    pub(super) fn address_with_stack(
        &mut self,
        address: Address,
        next_pc: u64,
        pending_stack: Option<u32>,
    ) {
        self.segment = address.segment;
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        {
            self.parallel_memo_address = address.index.is_none()
                && pending_stack.is_none()
                && !matches!(
                    self.current_operation,
                    crate::x86::instruction::Operation::String { .. }
                );
        }
        let has_base = if let AddressBase::Register(base) = address.base {
            self.op(Op::LocalGet(if base == 4 {
                pending_stack.unwrap_or(GPR + 4)
            } else {
                GPR + u32::from(base)
            }));
            true
        } else {
            false
        };
        if let Some(index) = address.index {
            self.op(Op::LocalGet(if index.register == 4 {
                pending_stack.unwrap_or(GPR + 4)
            } else {
                GPR + u32::from(index.register)
            }));
            if index.scale != 1 {
                self.op(Op::I64Const(i64::from(index.scale.trailing_zeros())))
                    .op(Op::I64Shl);
            }
            if has_base {
                self.op(Op::I64Add);
            }
        }
        let constant = if address.base == AddressBase::NextPc {
            next_pc.wrapping_add(address.displacement as u64)
        } else {
            address.displacement as u64
        };
        if constant != 0 || address.index.is_none() {
            self.op(Op::I64Const(constant as i64));
            if has_base || address.index.is_some() {
                self.op(Op::I64Add);
            }
        }
        self.narrow(address.size);
    }

    pub(super) fn linear_address(&mut self) {
        self.op(Op::LocalGet(ADDRESS));
        if self.context.is_long64() {
            let base = self.context.segment_base(self.segment);
            if base != 0 {
                self.op(Op::I64Const(base as i64)).op(Op::I64Add);
            }
        }
    }

    pub(super) fn record_fault_segment(&mut self, segment: Segment) {
        self.op(Op::LocalGet(0))
            .op(Op::I32Const(segment as i32))
            .op(Op::I32Store(MemArg {
                offset: std::mem::offset_of!(RunState, fault_segment) as u64,
                align: 2,
                memory_index: 0,
            }));
    }

    fn memory_failure(&mut self, exit_depth: u32) {
        if self.paged {
            self.record_fault_segment(self.segment);
        }
        if self.restore_string_flags {
            self.restore_repeat_flags();
        }
        if let Some(reason) = self.access_reason {
            self.op(Op::Call(reason))
                .op(Op::LocalSet(REASON))
                .op(Op::Br(exit_depth));
        } else {
            self.record_fault_segment(self.segment);
            self.exit(DATA, exit_depth);
        }
    }

    pub(super) fn lea(&mut self, dst: RegisterView, address: Address, width: Width, next_pc: u64) {
        self.address(address, next_pc);
        self.narrow(width);
        self.write_register(dst, width);
    }

    pub(super) fn load(
        &mut self,
        dst: RegisterView,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        self.load_value(address, width, next_pc, false, exit_depth);
        self.op(Op::LocalGet(VALUE));
        self.narrow(width);
        self.write_register(dst, width);
    }

    pub(super) fn load_value(
        &mut self,
        address: Address,
        width: Width,
        next_pc: u64,
        for_write: bool,
        exit_depth: u32,
    ) {
        self.address(address, next_pc);
        self.op(Op::LocalSet(ADDRESS));
        self.load_address_value(width, for_write, exit_depth);
    }

    pub(super) fn load_address_value(&mut self, width: Width, for_write: bool, exit_depth: u32) {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if !for_write && self.direct_load(width, exit_depth) {
            return;
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if self.parallel_load(width, for_write, exit_depth) {
            return;
        }
        self.load_address_slow(width, for_write, exit_depth);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn direct_load(&mut self, width: Width, exit_depth: u32) -> bool {
        let Some(span) = self.read_span else {
            return false;
        };
        let bytes = u32::from(width.bytes());
        let last_offset = if self.context.is_long64() {
            None
        } else {
            // Activation checks the captured context, and segment-changing
            // instructions leave the unit. Offset zero proves a readable,
            // non-expand-down identity segment through its limit.
            if self
                .context
                .translate(0, bytes, self.segment, crate::memory::Access::Read, false)
                .ok()
                != Some(0)
            {
                return false;
            }
            Some(i64::from(self.context.segment(self.segment).limit) + 1 - i64::from(bytes))
        };
        self.op(Op::Block(BlockType::Empty));
        self.linear_address();
        self.op(Op::LocalGet(span.base))
            .op(Op::I64Sub)
            .op(Op::LocalTee(VALUE))
            .op(Op::LocalGet(span.length))
            .op(Op::I64LtU);
        if bytes > 1 {
            self.op(Op::LocalGet(VALUE))
                .op(Op::I64Const(i64::from(bytes - 1)))
                .op(Op::I64Add)
                .op(Op::LocalGet(span.length))
                .op(Op::I64LtU)
                .op(Op::I32And);
        }
        if let Some(limit) = last_offset {
            self.op(Op::LocalGet(ADDRESS))
                .op(Op::I64Const(limit))
                .op(Op::I64LeU)
                .op(Op::I32And);
        }
        self.op(Op::I32Eqz).op(Op::If(BlockType::Empty));
        let segment = last_offset.map_or(-1, |_| self.segment as i32);
        self.linear_address();
        self.op(Op::I32Const(bytes as i32))
            .op(Op::I32Const(segment))
            .op(Op::Call(span.resolve))
            .op(Op::LocalTee(span.descriptor))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.load_address_slow(width, false, exit_depth + 3);
        self.op(Op::Br(2)).op(Op::End);
        use crate::memory::ReadSpan as Descriptor;
        let field = |offset, align| MemArg {
            offset,
            align,
            memory_index: 0,
        };
        self.op(Op::LocalGet(span.descriptor))
            .op(Op::I64Load(field(
                std::mem::offset_of!(Descriptor, base) as u64,
                3,
            )))
            .op(Op::LocalSet(span.base))
            .op(Op::LocalGet(span.descriptor))
            .op(Op::I64Load32U(field(
                std::mem::offset_of!(Descriptor, length) as u64,
                2,
            )))
            .op(Op::LocalSet(span.length))
            .op(Op::LocalGet(span.descriptor))
            .op(Op::I32Load(field(
                std::mem::offset_of!(Descriptor, host) as u64,
                2,
            )))
            .op(Op::LocalSet(span.host));
        self.linear_address();
        self.op(Op::LocalGet(span.base))
            .op(Op::I64Sub)
            .op(Op::LocalSet(VALUE))
            .op(Op::End)
            .op(Op::LocalGet(span.host))
            .op(Op::LocalGet(VALUE))
            .op(Op::I32WrapI64)
            .op(Op::I32Add);
        let memory = field(0, 0);
        self.op(match width {
            Width::W8 => Op::I64Load8U(memory),
            Width::W16 => Op::I64Load16U(memory),
            Width::W32 => Op::I64Load32U(memory),
            Width::W64 => Op::I64Load(memory),
        })
        .op(Op::LocalSet(VALUE))
        .op(Op::End);
        true
    }

    pub(super) fn load_address_slow(&mut self, width: Width, for_write: bool, exit_depth: u32) {
        self.linear_address();
        self.op(Op::I32Const(i32::from(width.bytes())));
        if !self.context.is_long64() {
            self.op(Op::I32Const(self.segment as i32));
        }
        self.op(Op::Call(if for_write { 3 } else { 0 }))
            .op(Op::LocalSet(VALUE))
            .op(Op::Call(2))
            .op(Op::If(BlockType::Empty));
        self.memory_failure(exit_depth + 1);
        self.op(Op::End);
    }

    pub(super) fn store_result(&mut self, width: Width, exit_depth: u32) {
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if self.parallel_store(width, exit_depth) {
            return;
        }
        self.store_result_slow(width, exit_depth);
    }

    pub(super) fn store_result_slow(&mut self, width: Width, exit_depth: u32) {
        self.stores = true;
        self.linear_address();
        self.op(Op::I32Const(i32::from(width.bytes())))
            .op(Op::LocalGet(RESULT));
        if !self.context.is_long64() {
            self.op(Op::I32Const(self.segment as i32));
        }
        self.op(Op::Call(1))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.memory_failure(exit_depth + 1);
        self.op(Op::End);
    }

    pub(super) fn store(
        &mut self,
        src: DataOperand,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        self.address(address, next_pc);
        self.op(Op::LocalSet(ADDRESS));
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if self.parallel_scalar.is_some() {
            self.data_operand(src, width);
            self.op(Op::LocalSet(RESULT));
            self.store_result(width, exit_depth);
            return;
        }
        self.linear_address();
        self.op(Op::I32Const(i32::from(width.bytes())));
        self.data_operand(src, width);
        if !self.context.is_long64() {
            self.op(Op::I32Const(self.segment as i32));
        }
        self.stores = true;
        self.op(Op::Call(1))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.memory_failure(exit_depth + 1);
        self.op(Op::End);
    }
}
