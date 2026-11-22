use super::{ADDRESS, Emitter, GPR, REASON, RESULT, STORE_STATUS, VALUE};
use crate::embedding::layout::{DATA, RunState};
use crate::x86::context::Segment;
use crate::x86::instruction::{Address, AddressBase, DataOperand, RegisterView, Width};
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
// Mapping edits and source publication happen between invocations, so spans live in Wasm locals.
pub(super) struct SpanCache {
    pub(super) resolve: u32,
    base: u32,
    length: u32,
    host: u32,
    descriptor: u32,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
impl SpanCache {
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

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) enum DirectOperand {
    ScalarLoad(Width),
    ScalarStore(Width),
    VectorDword(u32),
    VectorBlock(u32),
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
        let dword = address.size == Width::W32;
        let add = if dword { Op::I32Add } else { Op::I64Add };
        let has_base = if let AddressBase::Register(base) = address.base {
            self.op(Op::LocalGet(if base == 4 {
                pending_stack.unwrap_or(GPR + 4)
            } else {
                GPR + u32::from(base)
            }));
            if dword {
                self.op(Op::I32WrapI64);
            }
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
            if dword {
                self.op(Op::I32WrapI64);
            }
            if index.scale != 1 {
                let shift = index.scale.trailing_zeros();
                if dword {
                    self.op(Op::I32Const(shift as i32)).op(Op::I32Shl);
                } else {
                    self.op(Op::I64Const(i64::from(shift))).op(Op::I64Shl);
                }
            }
            if has_base {
                self.op(add.clone());
            }
        }
        let constant = if address.base == AddressBase::NextPc {
            next_pc.wrapping_add(address.displacement as u64)
        } else {
            address.displacement as u64
        };
        if constant != 0 || address.index.is_none() {
            self.op(if dword {
                Op::I32Const(constant as i32)
            } else {
                Op::I64Const(constant as i64)
            });
            if has_base || address.index.is_some() {
                self.op(add);
            }
        }
        if dword {
            self.op(Op::I64ExtendI32U);
        } else {
            self.narrow(address.size);
        }
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
        self.retire_before_read_fault();
        match self.flags_on_read_fault {
            Some(super::loop_cfg::ReadFaultFlags::Arithmetic(kind, width)) => {
                if kind == crate::x86::instruction::Alu::Cmp {
                    self.arithmetic_result(kind, width);
                }
                self.arithmetic_flags(kind, width);
            }
            Some(super::loop_cfg::ReadFaultFlags::Shift(kind, width)) => {
                self.shift_flags(kind, width)
            }
            None => {}
        }
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if width == Width::W32
            && self.loop_gates_prechecked
            && let Some(window) = self.indexed_read_window
            && window.address == address
        {
            self.load_indexed_dword(dst, window, next_pc, exit_depth);
            return;
        }
        self.load_value(address, width, next_pc, false, exit_depth);
        self.write_loaded_value(dst, width);
    }

    fn write_loaded_value(&mut self, dst: RegisterView, width: Width) {
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
        if self.guarded_paged_lap
            && !for_write
            && let Some(read) = match width {
                Width::W8 => self.paged_loop_read_byte,
                Width::W32 => self.paged_loop_read_dword,
                _ => None,
            }
        {
            self.address(address, next_pc);
            self.op(Op::Call(read))
                .op(Op::LocalTee(VALUE))
                .op(Op::I64Const(-1))
                .op(Op::I64Eq);
            self.retry_guarded_paged_operand(exit_depth);
            return;
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if !for_write && width == Width::W8 && self.load_proved_byte(address, next_pc) {
            return;
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if !for_write
            && let Some((proof, host)) = self.proved_stack
            && let Some(offset) = proof.offset(address, width)
        {
            self.segment = address.segment;
            self.op(Op::LocalGet(host));
            self.direct_scalar_load(
                width,
                MemArg {
                    offset,
                    align: 0,
                    memory_index: 0,
                },
            );
            return;
        }
        self.address(address, next_pc);
        self.op(Op::LocalSet(ADDRESS));
        self.load_address_value(width, for_write, exit_depth);
    }

    fn retry_guarded_paged_operand(&mut self, exit_depth: u32) {
        let entry = self.current_index as i32;
        let (_, attempts) = self.paged_loop_guard.expect("paged loop guard");
        // The probe left this instruction unstarted. Enter its checked label
        // with the already-retired prefix still local.
        self.op(Op::If(BlockType::Empty))
            .op(Op::I32Const(2))
            .op(Op::LocalSet(attempts))
            .op(Op::I32Const(entry))
            .op(Op::LocalSet(super::ENTRY))
            .op(Op::Br(exit_depth))
            .op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn guard_indexed_read_window(
        &mut self,
        window: super::read_loop::IndexedReadWindow,
        lap: u32,
    ) {
        use super::{LHS, REMAINING, RHS};
        use crate::memory::DirectSpan;

        let field = |offset, align| MemArg {
            offset,
            align,
            memory_index: 0,
        };
        self.op(Op::I64Const(0)).op(Op::LocalSet(window.limit));
        self.op(Op::Block(BlockType::Empty))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(lap)))
            .op(Op::I64LtU)
            .op(Op::BrIf(0));
        self.address(
            Address {
                index: None,
                ..window.address
            },
            0,
        );
        self.op(Op::LocalTee(ADDRESS))
            .op(Op::I32Const(4))
            .op(Op::I32Const(window.address.segment as i32))
            .op(Op::Call(window.span.resolve))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::BrIf(0));
        self.op(Op::LocalGet(ADDRESS))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load(field(
                std::mem::offset_of!(DirectSpan, base) as u64,
                3,
            )))
            .op(Op::I64Sub)
            .op(Op::LocalSet(LHS));
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(field(
                std::mem::offset_of!(DirectSpan, host) as u64,
                2,
            )))
            .op(Op::LocalGet(LHS))
            .op(Op::I32WrapI64)
            .op(Op::I32Add)
            .op(Op::LocalSet(window.host));
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load32U(field(
                std::mem::offset_of!(DirectSpan, length) as u64,
                2,
            )))
            .op(Op::LocalGet(LHS))
            .op(Op::I64Sub)
            .op(Op::LocalSet(VALUE));
        if !self.context.is_long64() {
            let end = u64::from(self.context.segment(window.address.segment).limit) + 1;
            self.op(Op::LocalGet(VALUE))
                .op(Op::I64Const(end as i64))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::I64Sub)
                .op(Op::LocalTee(RHS))
                .op(Op::LocalGet(VALUE))
                .op(Op::LocalGet(RHS))
                .op(Op::I64LtU)
                .op(Op::Select)
                .op(Op::LocalSet(VALUE));
        }
        let shift = window.address.index.unwrap().scale.trailing_zeros();
        // The admitted anchor is readable for four bytes. Trim later starts
        // to the same mapping and segment before scaling their indices.
        self.op(Op::LocalGet(VALUE))
            .op(Op::I64Const(4))
            .op(Op::I64Sub)
            .op(Op::I64Const(i64::from(shift)))
            .op(Op::I64ShrU)
            .op(Op::I64Const(1))
            .op(Op::I64Add)
            .op(Op::LocalSet(window.limit))
            .op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn load_indexed_dword(
        &mut self,
        dst: RegisterView,
        window: super::read_loop::IndexedReadWindow,
        next_pc: u64,
        exit_depth: u32,
    ) {
        let address = window.address;
        let index = address.index.unwrap();
        self.register(index.register, address.size);
        self.op(Op::LocalTee(VALUE))
            .op(Op::LocalGet(window.limit))
            .op(Op::I64LtU)
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(window.host))
            .op(Op::LocalGet(VALUE))
            .op(Op::I32WrapI64);
        if index.scale != 1 {
            self.op(Op::I32Const(index.scale.trailing_zeros() as i32))
                .op(Op::I32Shl);
        }
        self.op(Op::I32Add);
        let memory = MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        };
        let long64 = self.context.is_long64();
        if long64 {
            // The load has already zero-extended the architectural destination.
            self.op(Op::I64Load32U(memory));
            self.write_register(dst, Width::W32);
        } else {
            self.direct_scalar_load(Width::W32, memory);
        }
        self.op(Op::Else);
        self.address(address, next_pc);
        self.op(Op::LocalSet(ADDRESS));
        self.load_address_value(Width::W32, false, exit_depth + 1);
        if long64 {
            self.write_loaded_value(dst, Width::W32);
        }
        self.op(Op::End);
        if !long64 {
            self.write_loaded_value(dst, Width::W32);
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn load_proved_byte(&mut self, address: Address, next_pc: u64) -> bool {
        let Some((proof, first_bias)) = self.proved_reads else {
            return false;
        };
        let Some((stream, first, offset)) = proof.byte_slot(address) else {
            return false;
        };
        // The certified span bounds the host address and the literal offset.
        self.address(first, next_pc);
        self.op(Op::I32WrapI64)
            .op(Op::LocalGet(first_bias + stream))
            .op(Op::I32Add);
        self.direct_scalar_load(
            Width::W8,
            MemArg {
                offset,
                align: 0,
                memory_index: 0,
            },
        );
        true
    }

    pub(super) fn load_address_value(&mut self, width: Width, for_write: bool, exit_depth: u32) {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if !for_write
            && self.direct_access(
                DirectOperand::ScalarLoad(width),
                exit_depth,
                |emitter, depth| {
                    emitter.load_address_slow(width, false, depth);
                },
            )
        {
            return;
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if !for_write && self.parallel_load_dword_window(width, exit_depth) {
            return;
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if self.parallel_load(width, for_write, exit_depth) {
            return;
        }
        self.load_address_slow(width, for_write, exit_depth);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn direct_access(
        &mut self,
        operand: DirectOperand,
        exit_depth: u32,
        slow: impl FnOnce(&mut Self, u32),
    ) -> bool {
        use crate::memory::{Access, DirectSpan as Descriptor};
        let bytes = match operand {
            DirectOperand::ScalarLoad(width) | DirectOperand::ScalarStore(width) => {
                u32::from(width.bytes())
            }
            DirectOperand::VectorDword(_) => 4,
            DirectOperand::VectorBlock(_) => 16,
        };
        let (span, access) = if matches!(operand, DirectOperand::ScalarStore(_)) {
            (self.write_span(), Access::Write)
        } else {
            let span = if self.segment == Segment::Ss {
                self.stack_read_span.or(self.read_span)
            } else {
                self.read_span
            };
            (span, Access::Read)
        };
        let Some(span) = span else {
            return false;
        };
        let last_offset = if self.context.is_long64() {
            None
        } else {
            // Activation checks the captured context, and segment-changing
            // instructions leave the unit. Offset zero proves an accessible,
            // non-expand-down identity segment through its limit.
            if self
                .context
                .translate(0, bytes, self.segment, access, false)
                .ok()
                != Some(0)
            {
                return false;
            }
            Some(i64::from(self.context.segment(self.segment).limit) + 1 - i64::from(bytes))
        };
        if matches!(
            operand,
            DirectOperand::VectorDword(_) | DirectOperand::VectorBlock(_)
        ) {
            // SIMD passes its original effective offset on the stack.
            self.op(Op::LocalSet(ADDRESS));
        }
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
        self.op(Op::I32Eqz).unlikely(Op::If(BlockType::Empty));
        let segment = last_offset.map_or(-1, |_| self.segment as i32);
        self.linear_address();
        self.op(Op::I32Const(bytes as i32))
            .op(Op::I32Const(segment))
            .op(Op::Call(span.resolve))
            .op(Op::LocalTee(span.descriptor))
            .op(Op::I32Eqz)
            .unlikely(Op::If(BlockType::Empty));
        slow(self, exit_depth + 3);
        self.op(Op::Br(2)).op(Op::End);
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
        match operand {
            DirectOperand::VectorDword(vector) => {
                self.op(Op::V128Load32Zero(memory)).op(Op::LocalSet(vector));
            }
            DirectOperand::VectorBlock(vector) => {
                self.op(Op::V128Load(memory)).op(Op::LocalSet(vector));
            }
            DirectOperand::ScalarLoad(width) => self.direct_scalar_load(width, memory),
            DirectOperand::ScalarStore(width) => {
                self.op(Op::LocalGet(RESULT));
                self.direct_scalar_store(width, memory);
                self.op(Op::I32Const(1)).op(Op::LocalSet(STORE_STATUS));
            }
        }
        self.op(Op::End);
        true
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn direct_scalar_load(&mut self, width: Width, memory: MemArg) {
        self.op(match width {
            Width::W8 => Op::I64Load8U(memory),
            Width::W16 => Op::I64Load16U(memory),
            Width::W32 => Op::I64Load32U(memory),
            Width::W64 => Op::I64Load(memory),
        })
        .op(Op::LocalSet(VALUE));
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn direct_scalar_store(&mut self, width: Width, memory: MemArg) {
        self.op(match width {
            Width::W8 => Op::I64Store8(memory),
            Width::W16 => Op::I64Store16(memory),
            Width::W32 => Op::I64Store32(memory),
            Width::W64 => Op::I64Store(memory),
        });
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

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn write_span(&self) -> Option<SpanCache> {
        self.write_spans[usize::from(self.segment == Segment::Ss)]
    }

    pub(super) fn store(
        &mut self,
        src: DataOperand,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        if self.guarded_paged_lap
            && width == Width::W8
            && address.segment == Segment::Ds
            && let Some(write) = self.paged_loop_write_byte
        {
            self.address(address, next_pc);
            self.data_operand(src, width);
            // The helper admits only writes that leave the active source valid.
            self.op(Op::I32WrapI64).op(Op::Call(write)).op(Op::I32Eqz);
            self.retry_guarded_paged_operand(exit_depth);
            return;
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some((proof, host)) = self.proved_stack
            && proof.writes
            && let Some(offset) = proof.offset(address, width)
        {
            self.segment = address.segment;
            self.op(Op::LocalGet(host));
            self.data_operand(src, width);
            self.direct_scalar_store(
                width,
                MemArg {
                    offset,
                    align: 0,
                    memory_index: 0,
                },
            );
            return;
        }
        self.address(address, next_pc);
        self.op(Op::LocalSet(ADDRESS));
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.write_span().is_some() {
            self.data_operand(src, width);
            self.op(Op::LocalSet(RESULT));
            if !self.direct_access(
                DirectOperand::ScalarStore(width),
                exit_depth,
                |emitter, depth| {
                    emitter.store_result_slow(width, depth);
                },
            ) {
                self.store_result_slow(width, exit_depth);
            }
            return;
        }
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
