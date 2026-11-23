#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::indexed::IndexedOperand;
use super::{ADDRESS, Emitter, GPR, REASON, RESULT, STORE_STATUS, VALUE};
use crate::embedding::layout::{DATA, RunState};
use crate::x86::context::Segment;
use crate::x86::instruction::{Address, AddressBase, DataOperand, RegisterView, Width};
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) const PAGED_READ_REPLAY: i32 = -1;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
// The replayed lap counts first; the next seven headers run checked.
const PAGED_READ_COOLDOWN: i32 = PAGED_READ_REPLAY - 7;

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
// Hot hits use Wasm locals. An active flat read can seed the next invocation.
pub(super) struct SpanCache {
    pub(super) resolve: u32,
    base: u32,
    length: u32,
    host: u32,
    descriptor: u32,
    dword_bound: bool,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(super) struct PagedReadWindow {
    pub(super) resolve: u32,
    pub(super) base: u32,
    pub(super) limit: u32,
    pub(super) host: u32,
    pub(super) last_early_page: Option<u32>,
    pub(super) retouch: Option<VectorWindowRetouch>,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(super) struct VectorWindowRetouch {
    pub(super) alignment: (u8, i64),
    pub(super) resolve: u32,
    pub(super) touch: u32,
    pub(super) dirty: u32,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(super) struct PagedDwordReadPair {
    pub(super) first: u32,
    pub(super) second: u32,
    pub(super) base: u8,
    pub(super) resolve: u32,
    pub(super) host: u32,
    pub(super) resolver_mode: u32,
    pub(super) handoff: Option<DwordPairHandoff>,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(super) struct DwordPairHandoff {
    pub(super) read: u32,
    pub(super) address: u32,
    pub(super) ready: u32,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(super) struct PagedByteWritePair {
    pub(super) load: u32,
    pub(super) store: u32,
    pub(super) read: u32,
    pub(super) write: u32,
    pub(super) slot: u32,
    pub(super) host: Option<u32>,
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
            dword_bound: false,
        }
    }

    pub(super) fn with_dword_bound(mut self, enabled: bool) -> Self {
        self.dword_bound = enabled;
        self
    }
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) enum DirectOperand {
    ScalarLoad(Width),
    ScalarStore(Width),
    VectorDword(u32),
    VectorBlock(u32),
    VectorBlockStore {
        value: u32,
        address: Address,
        next_pc: u64,
    },
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(super) enum RmwAccess {
    Proved(u32, MemArg),
    Cached,
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        {
            self.stack_pointer_relative =
                matches!(address.base, AddressBase::Register(4)) && address.index.is_none();
        }
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
        let add = |emitter: &mut Self| {
            if dword {
                emitter.op(Op::I32Add);
            } else {
                emitter.op(Op::I64Add);
            }
        };
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
                add(self);
            }
        }
        let constant = if address.base == AddressBase::NextPc {
            next_pc.wrapping_add(address.displacement as u64)
        } else {
            address.displacement as u64
        };
        if constant != 0 || address.index.is_none() {
            if dword {
                self.op(Op::I32Const(constant as i32));
            } else {
                self.op(Op::I64Const(constant as i64));
            }
            if has_base || address.index.is_some() {
                add(self);
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

    pub(super) fn restore_preceding_read_state(&mut self) {
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
    }

    fn memory_failure(&mut self, exit_depth: u32) {
        self.restore_preceding_read_state();
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.guarded_paged_lap
            && self.loop_gates_prechecked
            && width == Width::W32
            && self
                .structured_loop
                .is_some_and(|(body, _)| body.straight_paged_reads())
            && let Some((source, [_, limit, host])) = self.direct_read_range
            && address == source
        {
            let index = address.index.unwrap().register;
            self.register(index, address.size);
            self.op(Op::LocalGet(limit))
                .op(Op::I64LtU)
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(host));
            self.register(index, address.size);
            self.op(Op::I32WrapI64)
                .op(Op::I32Const(2))
                .op(Op::I32Shl)
                .op(Op::I32Add)
                .op(Op::I64Load32U(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
            self.write_register(dst, width);
            self.op(Op::Else);
            self.load_value(address, width, next_pc, false, exit_depth + 1);
            self.write_loaded_value(dst, width);
            self.op(Op::End);
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some((read, indexed, inline_fallback)) = self.paged_prefix_read {
            debug_assert!(!for_write);
            debug_assert!(
                address.segment == Segment::Ds
                    || width == Width::W64
                        && self.context.is_long64()
                        && address.segment == Segment::Ss
            );
            self.address(address, next_pc);
            if indexed {
                self.indexed_prefix_operand_host(read, width.bytes());
            } else {
                let check_alignment = self.context.cpl() == 3;
                if check_alignment {
                    self.op(Op::LocalTee(ADDRESS))
                        .op(Op::I64Const(i64::from(width.bytes() - 1)))
                        .op(Op::I64And)
                        .op(Op::I64Eqz)
                        .op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                        .op(Op::LocalGet(ADDRESS));
                }
                let segment = if width == Width::W32 {
                    crate::embedding::layout::paging::PREFIX_DWORD_READ_SEGMENT
                } else {
                    // Long mode ignores the original DS or SS base.
                    Segment::Ds as u32
                };
                self.op(Op::I32Const(i32::from(width.bytes())))
                    .op(Op::I32Const(segment as i32))
                    .op(Op::Call(read));
                if check_alignment {
                    self.op(Op::Else).op(Op::I32Const(0)).op(Op::End);
                }
            }
            self.op(Op::LocalTee(STORE_STATUS))
                .op(Op::I32Eqz)
                .op(Op::If(BlockType::Empty));
            // Earlier instructions already retired. Branch to the original
            // label of this read so its fault/observation order stays exact.
            if inline_fallback {
                self.op(Op::Br(1));
            } else {
                self.route(self.current_pc, exit_depth, exit_depth + 1);
            }
            self.op(Op::End).op(Op::LocalGet(STORE_STATUS));
            if !indexed {
                self.op(Op::I32Load(MemArg {
                    offset: std::mem::offset_of!(crate::memory::DirectSpan, host) as u64,
                    align: 0,
                    memory_index: 0,
                }));
            }
            let operand = MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            };
            self.op(match width {
                Width::W8 => Op::I64Load8U(operand),
                Width::W16 => Op::I64Load16U(operand),
                Width::W32 => Op::I64Load32U(operand),
                Width::W64 => Op::I64Load(operand),
            })
            .op(Op::LocalSet(VALUE));
            return;
        }
        if !for_write && self.load_guarded_paged_value(address, width, next_pc, exit_depth) {
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

    pub(super) fn load_guarded_paged_value(
        &mut self,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) -> bool {
        if !self.guarded_paged_lap {
            return false;
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let guarded_stack_preamble = self.paged_preamble.zip(self.structured_loop).is_some_and(
            |((preamble, _, _), (body, _))| preamble.stack_reads && preamble.body.head == body.head,
        );
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        let guarded_stack_preamble = false;
        let read = match (width, address.segment) {
            (Width::W8, Segment::Ds) => self.paged_loop_read_byte,
            (Width::W32, Segment::Ds) => self.paged_loop_read_dword,
            // The graph or separate preamble guard proved a live flat SS.
            (Width::W32, Segment::Ss)
                if self.paged_loop_write_dword.is_some() || guarded_stack_preamble =>
            {
                self.paged_loop_read_dword
            }
            _ => None,
        };
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let pair = self
            .paged_byte_write_pair
            .filter(|pair| pair.load == self.current_index);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let read = pair.map(|pair| pair.read).or(read);
        let Some(read) = read else {
            return false;
        };
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(group) = self.indexed_write()
            && (self.current_index == group.shape.index || self.current_index == group.shape.lookup)
        {
            self.load_indexed_write(group, address, width, next_pc, exit_depth);
            return true;
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(pair) = self.paged_dword_read_pair
            && (self.current_index == pair.first || self.current_index == pair.second)
        {
            self.load_paired_paged_dword(pair, address, next_pc, read, exit_depth);
            return true;
        }
        self.address(address, next_pc);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if width == Width::W32
            && let Some(pair) = self.paged_dword_read_pair
            && address.size == Width::W64
            && address.segment == Segment::Ds
            && address.base == AddressBase::Register(pair.base)
            && address.index.is_none()
        {
            self.load_paged_pair_window(pair, read, exit_depth);
            return true;
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if width == Width::W32
            && address.segment == Segment::Ds
            && let Some(window) = self.paged_dword_window
            && (self.paged_dword_window_touch.is_none()
                || address.index.is_some_and(|index| index.scale == 4))
        {
            self.load_windowed_paged_dword(window, read, exit_depth);
            return true;
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.direct_loop_reads && width == Width::W32 && address.segment == Segment::Ds {
            self.load_loop_read_page(read);
        } else {
            self.op(Op::Call(read));
        }
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        self.op(Op::Call(read));
        self.op(Op::LocalTee(VALUE))
            .op(Op::I64Const(-1))
            .op(Op::I64Eq);
        self.retry_guarded_paged_operand(exit_depth, true);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(pair) = pair {
            self.op(Op::LocalGet(VALUE))
                .op(Op::I32WrapI64)
                .op(Op::I32Const(8))
                .op(Op::I32ShrU)
                .op(Op::LocalSet(pair.slot));
            if let Some(host) = pair.host {
                self.op(Op::LocalGet(VALUE))
                    .op(Op::I64Const(32))
                    .op(Op::I64ShrU)
                    .op(Op::I32WrapI64)
                    .op(Op::LocalSet(host));
            }
            self.op(Op::LocalGet(VALUE))
                .op(Op::I64Const(255))
                .op(Op::I64And)
                .op(Op::LocalSet(VALUE));
        }
        true
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn refresh_direct_read_range(
        &mut self,
        (address, [origin, limit, host]): (Address, [u32; 3]),
    ) {
        use crate::embedding::layout::{StateStorage, paging::LoopReadRange};
        use std::mem::offset_of;
        let field = |offset: usize, align| MemArg {
            offset: (offset_of!(StateStorage, loop_read_range) + offset) as u64,
            align,
            memory_index: 0,
        };
        let length = field(offset_of!(LoopReadRange, dwords), 2);
        self.address(
            Address {
                index: None,
                ..address
            },
            0,
        );
        self.op(Op::LocalTee(origin))
            .op(Op::LocalGet(0))
            .op(Op::I64Load(field(offset_of!(LoopReadRange, base), 3)))
            .op(Op::I64Sub)
            .op(Op::I64Const(2))
            .op(Op::I64Rotr)
            .op(Op::LocalSet(limit))
            .op(Op::LocalGet(0))
            .op(Op::I64Load32U(length))
            .op(Op::LocalGet(limit))
            .op(Op::I64GtU)
            .op(Op::If(BlockType::Result(wasm_encoder::ValType::I64)))
            .op(Op::LocalGet(0))
            .op(Op::I64Load32U(length))
            .op(Op::LocalGet(limit))
            .op(Op::I64Sub)
            .op(Op::Else)
            .op(Op::I64Const(0))
            .op(Op::End)
            .op(Op::LocalSet(limit))
            .op(Op::LocalGet(origin))
            .op(Op::I32WrapI64)
            .op(Op::LocalGet(0))
            .op(Op::I32Load(field(offset_of!(LoopReadRange, host_bias), 2)))
            .op(Op::I32Add)
            .op(Op::LocalSet(host));
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn load_loop_read_page(&mut self, read: u32) {
        use crate::embedding::layout::{
            StateStorage,
            paging::{LOOP_READ_PAGES, LoopReadPage},
        };
        use std::mem::{offset_of, size_of};
        use wasm_encoder::ValType;

        let field = |offset: usize, align| MemArg {
            offset: offset as u64,
            align,
            memory_index: 0,
        };
        let range = self.direct_read_range.filter(|_| {
            self.structured_loop
                .is_some_and(|(body, _)| body.straight_paged_reads())
        });
        self.op(Op::LocalSet(ADDRESS))
            .op(Op::Block(BlockType::Result(ValType::I64)));
        if range.is_none() {
            use crate::embedding::layout::paging::LoopReadRange;
            let range = offset_of!(StateStorage, loop_read_range);
            self.op(Op::LocalGet(ADDRESS))
                .op(Op::LocalGet(0))
                .op(Op::I64Load(field(
                    range + offset_of!(LoopReadRange, base),
                    3,
                )))
                .op(Op::I64Sub)
                // Rotate the alignment bits above the dword offset for one bounds check.
                .op(Op::I64Const(2))
                .op(Op::I64Rotr)
                .op(Op::LocalGet(0))
                .op(Op::I64Load32U(field(
                    range + offset_of!(LoopReadRange, dwords),
                    2,
                )))
                .op(Op::I64LtU)
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(0))
                .op(Op::I32Load(field(
                    range + offset_of!(LoopReadRange, host_bias),
                    2,
                )))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::I32WrapI64)
                .op(Op::I32Add)
                .op(Op::I64Load32U(field(0, 2)))
                .op(Op::Br(1))
                .op(Op::End);
        }
        self.op(Op::Block(BlockType::Empty))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(field(
                offset_of!(StateStorage, loop_read_table),
                2,
            )))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Const(3))
            .op(Op::I32And)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Const(12))
            .op(Op::I32ShrU)
            .op(Op::I32Const(LOOP_READ_PAGES as i32 - 1))
            .op(Op::I32And)
            .op(Op::I32Const(size_of::<LoopReadPage>() as i32))
            .op(Op::I32Mul)
            .op(Op::I32Add)
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I64Load(field(offset_of!(LoopReadPage, tag), 3)))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I64Const(!4095))
            .op(Op::I64And)
            .op(Op::LocalGet(0))
            .op(Op::I64Load(field(
                offset_of!(StateStorage, loop_read_generation),
                3,
            )))
            .op(Op::I64Or)
            .op(Op::I64Ne)
            .op(Op::BrIf(0))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(field(offset_of!(LoopReadPage, host_bias), 2)))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Add)
            .op(Op::I64Load32U(field(0, 2)))
            .op(Op::Br(1))
            .op(Op::End)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::Call(read));
        if let Some(range) = range {
            self.refresh_direct_read_range(range);
        }
        self.op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn load_paged_pair_window(&mut self, pair: PagedDwordReadPair, read: u32, exit_depth: u32) {
        use wasm_encoder::ValType;

        let handoff = pair
            .handoff
            .filter(|handoff| handoff.read == self.current_index);
        let indexed = handoff.zip(self.indexed_paged_read);
        self.op(Op::LocalSet(ADDRESS))
            .op(Op::LocalGet(pair.host))
            .op(Op::If(BlockType::Result(ValType::I32)));
        if let Some((handoff, locals)) = indexed {
            // One published RAM page must cover the tail and both aligned operands
            // carried back to the head. Keep the original clipped-window resolver on misses.
            self.op(Op::Block(BlockType::Result(ValType::I32)))
                .op(Op::Block(BlockType::Empty))
                .op(Op::LocalGet(handoff.address))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::I64Xor)
                .op(Op::I64Const(!4095 | 3))
                .op(Op::I64And)
                .op(Op::I64Eqz)
                .op(Op::I32Eqz)
                .op(Op::BrIf(0))
                .op(Op::LocalGet(handoff.address))
                .op(Op::I32WrapI64)
                .op(Op::I32Const(4095))
                .op(Op::I32And)
                .op(Op::I32Const(4088))
                .op(Op::I32GtU)
                .op(Op::BrIf(0));
            self.indexed_read_host(locals, 4, true, IndexedOperand::Read);
            self.op(Op::LocalTee(STORE_STATUS))
                .op(Op::I32Eqz)
                .op(Op::BrIf(0))
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::LocalGet(handoff.address))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::I64Sub)
                .op(Op::I32WrapI64)
                .op(Op::I32Add)
                .op(Op::LocalSet(pair.host))
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::Br(1))
                .op(Op::End);
        }
        self.op(Op::LocalGet(ADDRESS))
            .op(Op::I32Const(4))
            .op(Op::I32Const(
                crate::embedding::layout::PAGED_PAIR_WINDOW as i32,
            ))
            .op(Op::Call(pair.resolve));
        if indexed.is_some() {
            self.op(Op::End);
        }
        self.op(Op::Else)
            .op(Op::I32Const(0))
            .op(Op::End)
            .op(Op::I64ExtendI32U)
            .op(Op::LocalTee(VALUE))
            .op(Op::I32WrapI64);
        if let Some(handoff) = handoff {
            // This successful tail read leaves the pair's data and execute entries newest.
            self.op(Op::I32Const(0))
                .op(Op::I32Ne)
                .op(Op::LocalTee(handoff.ready));
        }
        self.op(Op::If(BlockType::Result(ValType::I64)))
            .op(Op::LocalGet(VALUE))
            .op(Op::I32WrapI64)
            .op(Op::I64Load32U(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }))
            .op(Op::Else)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::Call(read))
            .op(Op::LocalTee(VALUE))
            .op(Op::I64Const(-1))
            .op(Op::I64Eq);
        self.retry_guarded_paged_operand(exit_depth + 1, true);
        self.op(Op::LocalGet(VALUE))
            .op(Op::End)
            .op(Op::LocalSet(VALUE));
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn load_paired_paged_dword(
        &mut self,
        pair: PagedDwordReadPair,
        address: Address,
        next_pc: u64,
        read: u32,
        exit_depth: u32,
    ) {
        use crate::memory::DirectSpan;
        use wasm_encoder::ValType;

        let first = self.current_index == pair.first;
        let handoff = pair.handoff.filter(|_| first);
        if first {
            // The helper admits one local translation and one backing for both
            // aligned dwords. Always overwrite the local, including on a miss.
            if handoff.is_some() {
                self.op(Op::Block(BlockType::Result(ValType::I64)));
            }
            self.address(address, next_pc);
            self.op(Op::LocalTee(ADDRESS));
            if let Some(handoff) = handoff {
                self.op(Op::LocalGet(handoff.address))
                    .op(Op::I64Eq)
                    .op(Op::LocalGet(handoff.ready))
                    .op(Op::I32And)
                    .op(Op::If(BlockType::Empty))
                    .op(Op::LocalGet(pair.host))
                    .op(Op::I64Load32U(MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }))
                    .op(Op::Br(1))
                    .op(Op::End)
                    .op(Op::LocalGet(ADDRESS));
            }
            self.op(Op::I32WrapI64)
                .op(Op::I32Const(3))
                .op(Op::I32And)
                .op(Op::I32Eqz)
                .op(Op::If(BlockType::Result(ValType::I32)));
            let indexed = handoff.zip(self.indexed_paged_read);
            if let Some((handoff, locals)) = indexed {
                // A skipped tail leaves the data TLB entry behind the index and code.
                // Revalidate and retouch it before reusing this pair at the same address.
                self.op(Op::Block(BlockType::Result(ValType::I32)))
                    .op(Op::Block(BlockType::Empty))
                    .op(Op::LocalGet(pair.host))
                    .op(Op::I32Eqz)
                    .op(Op::BrIf(0))
                    .op(Op::LocalGet(ADDRESS))
                    .op(Op::LocalGet(handoff.address))
                    .op(Op::I64Ne)
                    .op(Op::BrIf(0));
                self.indexed_read_host(locals, 8, true, IndexedOperand::Read);
                self.op(Op::LocalTee(STORE_STATUS))
                    .op(Op::I32Eqz)
                    .op(Op::BrIf(0))
                    .op(Op::LocalGet(STORE_STATUS))
                    .op(Op::Br(1))
                    .op(Op::End);
            }
            self.op(Op::LocalGet(ADDRESS))
                .op(Op::I32Const(8))
                .op(Op::LocalGet(pair.resolver_mode))
                .op(Op::Call(pair.resolve))
                .op(Op::I32Const(
                    crate::embedding::layout::PAGED_PAIR_CONTINUE as i32,
                ))
                .op(Op::LocalSet(pair.resolver_mode))
                .op(Op::LocalTee(STORE_STATUS))
                .op(Op::If(BlockType::Result(ValType::I32)))
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::I32Load(MemArg {
                    offset: std::mem::offset_of!(DirectSpan, host) as u64,
                    align: 2,
                    memory_index: 0,
                }))
                .op(Op::Else)
                .op(Op::I32Const(0))
                .op(Op::End);
            if indexed.is_some() {
                self.op(Op::End);
            }
            self.op(Op::Else).op(Op::I32Const(0)).op(Op::End);
            if let Some(handoff) = handoff {
                self.op(Op::LocalGet(ADDRESS))
                    .op(Op::LocalSet(handoff.address));
            }
            self.op(Op::LocalSet(pair.host));
        }
        self.op(Op::LocalGet(pair.host))
            .op(Op::If(BlockType::Result(ValType::I64)));
        self.op(Op::LocalGet(pair.host));
        // The intervening ADD updates only its destination and flags. The
        // second load still executes at the second instruction's own position.
        self.op(Op::I64Load32U(MemArg {
            offset: if first { 0 } else { 4 },
            align: 0,
            memory_index: 0,
        }))
        .op(Op::Else);
        if first {
            self.op(Op::LocalGet(ADDRESS));
        } else {
            self.address(address, next_pc);
        }
        self.op(Op::Call(read))
            .op(Op::LocalTee(VALUE))
            .op(Op::I64Const(-1))
            .op(Op::I64Eq);
        self.retry_guarded_paged_operand(exit_depth + 1 + u32::from(handoff.is_some()), true);
        self.op(Op::LocalGet(VALUE)).op(Op::End);
        if handoff.is_some() {
            self.op(Op::End);
        }
        self.op(Op::LocalSet(VALUE));
        if let Some(handoff) = handoff {
            self.op(Op::I32Const(0)).op(Op::LocalSet(handoff.ready));
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn load_windowed_paged_dword(&mut self, window: PagedReadWindow, read: u32, exit_depth: u32) {
        use crate::memory::DirectSpan;

        let field = |offset: usize, align| MemArg {
            offset: offset as u64,
            align,
            memory_index: 0,
        };
        self.op(Op::LocalTee(ADDRESS))
            .op(Op::LocalGet(window.base))
            .op(Op::I64Sub)
            .op(Op::LocalTee(VALUE))
            .op(Op::LocalGet(window.limit))
            .op(Op::I64LtU)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::I32Const(3))
            .op(Op::I32And)
            .op(Op::I32Eqz)
            .op(Op::I32And);
        if let Some(touch) = self.paged_dword_window_touch {
            self.op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::Call(touch))
                .op(Op::Else)
                .op(Op::I32Const(0))
                .op(Op::End);
        }
        self.op(Op::If(BlockType::Result(wasm_encoder::ValType::I64)))
            .op(Op::LocalGet(VALUE));
        self.paged_dword_window_load(window.host);
        self.op(Op::Else)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::Call(window.resolve))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Result(wasm_encoder::ValType::I64)))
            // The scalar fallback may touch a different retained entry.
            .op(Op::I64Const(0))
            .op(Op::LocalSet(window.limit))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::Call(read))
            .op(Op::LocalTee(VALUE))
            .op(Op::I64Const(-1))
            .op(Op::I64Eq);
        self.retry_guarded_paged_operand(exit_depth + 2, true);
        self.op(Op::LocalGet(VALUE))
            .op(Op::Else)
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load(field(
                std::mem::offset_of!(DirectSpan, base),
                3,
            )))
            .op(Op::LocalSet(window.base))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load32U(field(
                std::mem::offset_of!(DirectSpan, length),
                2,
            )))
            .op(Op::I64Const(3))
            .op(Op::I64Sub)
            .op(Op::LocalSet(window.limit))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(field(
                std::mem::offset_of!(DirectSpan, host),
                2,
            )))
            .op(Op::LocalSet(window.host))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::LocalGet(window.base))
            .op(Op::I64Sub);
        self.paged_dword_window_load(window.host);
        self.op(Op::End).op(Op::End).op(Op::LocalSet(VALUE));
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn paged_dword_window_load(&mut self, host: u32) {
        self.op(Op::I32WrapI64)
            .op(Op::LocalGet(host))
            .op(Op::I32Add)
            .op(Op::I64Load32U(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
    }

    pub(super) fn retry_guarded_paged_operand(&mut self, exit_depth: u32, _replay: bool) {
        let entry = self.current_index as i32;
        let (_, attempts) = self.paged_loop_guard.expect("paged loop guard");
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let status = if _replay && self.paged_read_rearm.is_some() {
            PAGED_READ_REPLAY
        } else {
            2
        };
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        let status = 2;
        // The probe left this instruction unstarted. Settle the preceding
        // instructions before entering its checked label.
        self.op(Op::If(BlockType::Empty));
        self.restore_preceding_read_state();
        self.clear_quiet_fetch();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.paged_preamble_latch
            && let Some((_, handoff, inherited)) = self.paged_preamble
        {
            self.op(Op::I32Const(0))
                .op(Op::LocalSet(handoff))
                .op(Op::I32Const(0))
                .op(Op::LocalSet(inherited));
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let preamble = self
            .paged_preamble
            .zip(self.structured_loop)
            .filter(|((preamble, _, _), (body, _))| preamble.body.head == body.head)
            .map(|(preamble, _)| preamble);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some((preamble, handoff, _)) = preamble
            && self.current_index == preamble.body.head
        {
            // Re-enter the original head once. A refused first operand
            // would otherwise retry the same clone without retiring.
            self.op(Op::I32Const(2)).op(Op::LocalSet(handoff));
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let preamble = preamble.is_some();
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        let preamble = false;
        // A refused preamble read has its own checked successor path. It
        // cannot arm or disable the selected inner graph's replay latch.
        if !preamble {
            self.op(Op::I32Const(status)).op(Op::LocalSet(attempts));
        }
        self.op(Op::I32Const(entry))
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
    pub(super) fn load_retained_flat_spans(
        &mut self,
        reads: [Option<SpanCache>; 3],
        writes: [Option<SpanCache>; 2],
    ) {
        use crate::embedding::layout::StateStorage;
        use crate::memory::DirectSpan;
        use std::mem::{offset_of, size_of};

        let read_offset = offset_of!(StateStorage, flat_reads);
        for (slot, cache) in [
            (read_offset, reads[0]),
            (read_offset + size_of::<DirectSpan>(), reads[1]),
            (offset_of!(StateStorage, flat_stack_pointer_read), reads[2]),
            (offset_of!(StateStorage, flat_data_write), writes[0]),
            (offset_of!(StateStorage, flat_stack_write), writes[1]),
        ] {
            let Some(cache) = cache else {
                continue;
            };
            let field = |offset, align| MemArg {
                offset: (slot + offset) as u64,
                align,
                memory_index: 0,
            };
            self.op(Op::LocalGet(0))
                .op(Op::I64Load(field(offset_of!(DirectSpan, base), 3)))
                .op(Op::LocalSet(cache.base))
                .op(Op::LocalGet(0))
                .op(Op::I64Load32U(field(offset_of!(DirectSpan, length), 2)))
                .op(Op::LocalSet(cache.length))
                .op(Op::LocalGet(0))
                .op(Op::I32Load(field(offset_of!(DirectSpan, host), 2)))
                .op(Op::LocalSet(cache.host));
            self.publish_dword_span_bound(cache);
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn direct_access(
        &mut self,
        operand: DirectOperand,
        exit_depth: u32,
        slow: impl FnOnce(&mut Self, u32),
    ) -> bool {
        use crate::memory::Access;
        let bytes = match operand {
            DirectOperand::ScalarLoad(width) | DirectOperand::ScalarStore(width) => {
                u32::from(width.bytes())
            }
            DirectOperand::VectorDword(_) => 4,
            DirectOperand::VectorBlock(_) | DirectOperand::VectorBlockStore { .. } => 16,
        };
        let store = matches!(
            operand,
            DirectOperand::ScalarStore(_) | DirectOperand::VectorBlockStore { .. }
        );
        if self.segment == Segment::Ss
            && let Some((host, access)) = self.flat_stack_access
            && let Some(offset) = match operand {
                DirectOperand::ScalarLoad(Width::W32) => access.read,
                DirectOperand::ScalarStore(Width::W32) => access.write,
                _ => None,
            }
        {
            let arg = MemArg {
                offset: offset as u64,
                align: 0,
                memory_index: 0,
            };
            self.op(Op::LocalGet(host));
            if store {
                self.op(Op::LocalGet(RESULT));
                self.direct_scalar_store(Width::W32, arg);
            } else {
                self.direct_scalar_load(Width::W32, arg);
            }
            return true;
        }
        if self.segment == Segment::Ds
            && let Some(bias) = self.flat_data_bias
            && matches!(
                operand,
                DirectOperand::ScalarLoad(Width::W32) | DirectOperand::ScalarStore(Width::W32)
            )
        {
            let arg = MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            };
            self.flat_data_pointer(bias);
            if store {
                self.op(Op::LocalGet(RESULT));
                self.direct_scalar_store(Width::W32, arg);
            } else {
                self.direct_scalar_load(Width::W32, arg);
            }
            return true;
        }
        let stack_pointer = !store
            && self.segment == Segment::Ss
            && self.stack_pointer_relative
            && self.stack_read_span.is_some()
            && self.flat_stack_pointer_span.is_some();
        let (span, access) = if store {
            (self.write_span(), Access::Write)
        } else {
            let span = if stack_pointer {
                self.flat_stack_pointer_span
            } else if self.segment == Segment::Ss {
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
        let flat32 = last_offset.is_some()
            && self.context.mode() == crate::x86::context::Mode::Protected
            && self.context.code32()
            && self.context.segment(self.segment).limit == u32::MAX;
        if matches!(
            operand,
            DirectOperand::VectorDword(_) | DirectOperand::VectorBlock(_)
        ) {
            // SIMD passes its original effective offset on the stack.
            self.op(Op::LocalSet(ADDRESS));
        }
        if let DirectOperand::VectorBlockStore {
            address, next_pc, ..
        } = operand
        {
            self.address(address, next_pc);
            self.op(Op::LocalSet(ADDRESS));
        }
        self.op(Op::Block(BlockType::Empty));
        self.linear_address();
        if flat32 {
            self.flat_span_contains(span, bytes);
        } else {
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
        }
        self.op(Op::I32Eqz).unlikely(Op::If(BlockType::Empty));
        let segment = if stack_pointer {
            crate::embedding::layout::FLAT_STACK_POINTER_READ
        } else {
            last_offset.map_or(-1, |_| self.segment as i32)
        };
        self.linear_address();
        self.op(Op::I32Const(bytes as i32))
            .op(Op::I32Const(segment))
            .op(Op::Call(span.resolve))
            .op(Op::LocalTee(span.descriptor))
            .op(Op::I32Eqz)
            .unlikely(Op::If(BlockType::Empty));
        slow(self, exit_depth + 3);
        self.op(Op::Br(2)).op(Op::End);
        self.load_span_descriptor(span);
        self.linear_address();
        self.op(Op::LocalGet(span.base))
            .op(Op::I64Sub)
            .op(Op::LocalSet(VALUE))
            .op(Op::End)
            .op(Op::LocalGet(span.host))
            .op(Op::LocalGet(VALUE))
            .op(Op::I32WrapI64)
            .op(Op::I32Add);
        let memory = MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        };
        match operand {
            DirectOperand::VectorDword(vector) => {
                self.op(Op::V128Load32Zero(memory)).op(Op::LocalSet(vector));
            }
            DirectOperand::VectorBlock(vector) => {
                self.op(Op::V128Load(memory)).op(Op::LocalSet(vector));
            }
            DirectOperand::VectorBlockStore { value, .. } => {
                self.op(Op::LocalGet(value))
                    .op(Op::V128Store(memory))
                    .op(Op::I32Const(1))
                    .op(Op::LocalSet(STORE_STATUS));
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
    pub(super) fn flat_copy_span_host(&mut self, address: u32, width: u32, span: SpanCache) {
        use wasm_encoder::ValType;

        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::LocalGet(address));
        self.flat_span_contains(span, width);
        self.op(Op::I32Eqz)
            .unlikely(Op::If(BlockType::Empty))
            .op(Op::LocalGet(address))
            .op(Op::I32Const(width as i32))
            .op(Op::I32Const(Segment::Ds as i32))
            .op(Op::Call(span.resolve))
            .op(Op::LocalTee(span.descriptor))
            .op(Op::I32Eqz)
            .unlikely(Op::If(BlockType::Empty))
            .op(Op::I32Const(0))
            .op(Op::Br(2))
            .op(Op::End);
        self.load_span_descriptor(span);
        self.op(Op::LocalGet(address))
            .op(Op::LocalGet(span.base))
            .op(Op::I64Sub)
            .op(Op::LocalSet(VALUE))
            .op(Op::End)
            .op(Op::LocalGet(span.host))
            .op(Op::LocalGet(VALUE))
            .op(Op::I32WrapI64)
            .op(Op::I32Add)
            .op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn flat_copy_bytes_left(&mut self, address: u32, span: SpanCache) {
        self.op(Op::LocalGet(span.length))
            .op(Op::LocalGet(address))
            .op(Op::LocalGet(span.base))
            .op(Op::I64Sub)
            .op(Op::I64Sub);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn flat_span_bias(&mut self, span: SpanCache) {
        self.op(Op::LocalGet(span.host))
            .op(Op::LocalGet(span.base))
            .op(Op::I32WrapI64)
            .op(Op::I32Sub);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn flat_data_pointer(&mut self, bias: u32) {
        self.op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::LocalGet(bias))
            .op(Op::I32Add);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn flat_span_pointer(&mut self, span: SpanCache) {
        self.op(Op::LocalGet(span.host))
            .op(Op::LocalGet(VALUE))
            .op(Op::I32WrapI64)
            .op(Op::I32Add);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn flat_span_contains(&mut self, span: SpanCache, bytes: u32) {
        // A retained span cannot cross 4 GiB. An offset below its base
        // wraps to an unsigned delta at or beyond the end of that span.
        self.op(Op::I32WrapI64)
            .op(Op::LocalGet(span.base))
            .op(Op::I32WrapI64)
            .op(Op::I32Sub)
            .op(Op::I64ExtendI32U)
            .op(Op::LocalTee(VALUE));
        if bytes == 4 && span.dword_bound {
            self.op(Op::I32WrapI64)
                .op(Op::LocalGet(span.descriptor))
                .op(Op::I32LtU);
            return;
        }
        if bytes > 1 {
            self.op(Op::I64Const(i64::from(bytes - 1))).op(Op::I64Add);
        }
        self.op(Op::LocalGet(span.length)).op(Op::I64LtU);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn load_span_descriptor(&mut self, span: SpanCache) {
        use crate::memory::DirectSpan as Descriptor;

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
        self.publish_dword_span_bound(span);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn publish_dword_span_bound(&mut self, span: SpanCache) {
        if !span.dword_bound {
            return;
        }
        // Once the descriptor has been read, its temporary holds the
        // saturating dword offset bound until the next span refill.
        self.op(Op::LocalGet(span.length))
            .op(Op::I32WrapI64)
            .op(Op::I32Const(3))
            .op(Op::I32Sub)
            .op(Op::I32Const(0))
            .op(Op::LocalGet(span.length))
            .op(Op::I64Const(3))
            .op(Op::I64GtU)
            .op(Op::Select)
            .op(Op::LocalSet(span.descriptor));
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn direct_scalar_load(&mut self, width: Width, memory: MemArg) {
        self.op(match width {
            Width::W8 => Op::I64Load8U(memory),
            Width::W16 => Op::I64Load16U(memory),
            Width::W32 => Op::I64Load32U(memory),
            Width::W64 => Op::I64Load(memory),
        })
        .op(Op::LocalSet(VALUE));
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn direct_scalar_store(&mut self, width: Width, memory: MemArg) {
        self.op(match width {
            Width::W8 => Op::I64Store8(memory),
            Width::W16 => Op::I64Store16(memory),
            Width::W32 => Op::I64Store32(memory),
            Width::W64 => Op::I64Store(memory),
        });
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn load_cached_rmw(
        &mut self,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) -> Option<RmwAccess> {
        use crate::memory::Access;

        if address.segment == Segment::Ds
            && width == Width::W32
            && let Some(bias) = self.flat_data_bias
        {
            self.address(address, next_pc);
            self.op(Op::LocalSet(ADDRESS));
            self.flat_data_pointer(bias);
            self.op(Op::LocalTee(STORE_STATUS));
            let memory = MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            };
            self.direct_scalar_load(width, memory);
            return Some(RmwAccess::Proved(STORE_STATUS, memory));
        }
        if self.uses_indexed_scalar_rmw() {
            self.address(address, next_pc);
            self.op(Op::LocalSet(ADDRESS));
            let guarded = self.can_reuse_paged_rmw_fetch(next_pc);
            let index = self.indexed_paged_read.unwrap();
            if guarded {
                use crate::embedding::layout::debug::CONTINUE_ACTIVE;
                use wasm_encoder::ValType;

                self.op(Op::Block(BlockType::Result(ValType::I32)));
                self.newest_prefix_fetch(self.current_pc, true, false);
                self.op(Op::If(BlockType::Empty));
                self.indexed_read_host(
                    index,
                    width.bytes(),
                    false,
                    IndexedOperand::ReadModifyWrite,
                );
                let boundary_result = self.boundary_result.unwrap();
                self.op(Op::LocalTee(STORE_STATUS))
                    .op(Op::If(BlockType::Empty))
                    .op(Op::I32Const(CONTINUE_ACTIVE))
                    .op(Op::LocalSet(boundary_result))
                    .op(Op::LocalGet(STORE_STATUS))
                    .op(Op::Br(2))
                    .op(Op::End)
                    .op(Op::End);
                // A refusal has neither loaded nor updated guest memory.
                self.begin(self.current_operation, next_pc, exit_depth + 1);
            }
            self.indexed_read_host(index, width.bytes(), false, IndexedOperand::ReadModifyWrite);
            if guarded {
                self.op(Op::End);
            }
            self.op(Op::LocalTee(STORE_STATUS))
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(STORE_STATUS));
            self.direct_scalar_load(
                width,
                MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                },
            );
            self.op(Op::Else);
            self.load_address_slow(width, true, exit_depth + 1);
            self.op(Op::End);
            return Some(RmwAccess::Cached);
        }

        if let Some((proof, host)) = self.proved_stack
            && proof.writes
            && let Some(offset) = proof.offset(address, width)
        {
            let memory = MemArg {
                offset,
                align: 0,
                memory_index: 0,
            };
            self.op(Op::LocalGet(host));
            self.direct_scalar_load(width, memory);
            return Some(RmwAccess::Proved(host, memory));
        }
        let read = match address.segment {
            Segment::Ss => self.stack_read_span,
            Segment::Ds if self.linear_call_spans => self.read_span,
            _ => return None,
        }?;
        let stack = address.segment == Segment::Ss;
        let write = self.write_spans[usize::from(stack)]?;
        if self.observed
            || self.context.is_long64()
            || address.size != Width::W32
            || width != Width::W32
            || self
                .context
                .translate(0, 4, address.segment, Access::Write, stack)
                .ok()
                != Some(0)
        {
            return None;
        }
        let last = i64::from(self.context.segment(address.segment).limit) - 3;
        self.address(address, next_pc);
        self.op(Op::LocalTee(ADDRESS))
            .op(Op::I64Const(last))
            .op(Op::I64LeU);
        // Both caches describe a whole mapping. Require the same mapping to
        // cover the operand with both permissions. Unfilled caches have
        // length zero, and the write proof already excludes code aliases.
        self.op(Op::LocalGet(read.base))
            .op(Op::LocalGet(write.base))
            .op(Op::I64Eq)
            .op(Op::I32And)
            .op(Op::LocalGet(read.length))
            .op(Op::LocalGet(write.length))
            .op(Op::I64Eq)
            .op(Op::I32And)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::LocalGet(write.base))
            .op(Op::I64Sub)
            .op(Op::LocalTee(VALUE))
            .op(Op::LocalGet(write.length))
            .op(Op::I64LtU)
            .op(Op::I32And)
            .op(Op::LocalGet(VALUE))
            .op(Op::I64Const(3))
            .op(Op::I64Add)
            .op(Op::LocalGet(write.length))
            .op(Op::I64LtU)
            .op(Op::I32And);
        self.op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(write.host))
            .op(Op::LocalGet(VALUE))
            .op(Op::I32WrapI64)
            .op(Op::I32Add)
            .op(Op::LocalTee(STORE_STATUS));
        self.direct_scalar_load(
            width,
            MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            },
        );
        self.op(Op::Else)
            .op(Op::I32Const(0))
            .op(Op::LocalSet(STORE_STATUS));
        self.load_address_slow(width, true, exit_depth + 1);
        self.op(Op::End);
        Some(RmwAccess::Cached)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn store_cached_rmw(&mut self, proof: RmwAccess, width: Width, exit_depth: u32) {
        if let RmwAccess::Proved(host, memory) = proof {
            self.op(Op::LocalGet(host)).op(Op::LocalGet(RESULT));
            self.direct_scalar_store(width, memory);
            return;
        }
        // No other guest access or callback runs between the proved load and
        // this store. On the checked path, keep the original status.
        self.stores = true;
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::LocalGet(RESULT));
        self.direct_scalar_store(
            width,
            MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            },
        );
        self.op(Op::I32Const(1))
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::Else);
        self.store_result_slow(width, exit_depth + 1);
        self.op(Op::End);
    }

    pub(super) fn load_address_slow(&mut self, width: Width, for_write: bool, exit_depth: u32) {
        self.linear_address();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let indexed = !for_write && self.uses_indexed_scalar_read(self.current_operation);
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        let indexed = false;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if indexed {
            self.op(Op::LocalSet(ADDRESS));
            self.indexed_read_host(
                self.indexed_paged_read.unwrap(),
                width.bytes(),
                false,
                IndexedOperand::Read,
            );
            self.op(Op::LocalTee(STORE_STATUS))
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(STORE_STATUS));
            self.direct_scalar_load(
                width,
                MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                },
            );
            if self.paged_return_uses_read_result() {
                self.op(Op::I32Const(
                    crate::embedding::layout::QUIET_RETURN_READ as i32,
                ))
                .op(Op::LocalSet(STORE_STATUS));
            }
            self.op(Op::Else).op(Op::LocalGet(ADDRESS));
        }
        let read_exit = exit_depth + u32::from(indexed);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if !for_write
            && width == Width::W64
            && self.segment == Segment::Ss
            && self.paged_return_uses_read_result()
        {
            use crate::embedding::layout::{PAGED_RETURN_QWORD, ReturnRead};
            self.used_paged_quiet_completion = true;
            let packed = if indexed {
                PAGED_RETURN_QWORD | crate::embedding::layout::paging::INDEXED_SCALAR_READ
            } else {
                PAGED_RETURN_QWORD
            };
            self.op(Op::I32Const(packed as i32))
                .op(Op::Call(0))
                .op(Op::LocalTee(VALUE))
                .op(Op::I32WrapI64)
                .op(Op::I32Load(MemArg {
                    offset: std::mem::offset_of!(ReturnRead, status) as u64,
                    align: 2,
                    memory_index: 0,
                }))
                .op(Op::LocalTee(STORE_STATUS))
                .op(Op::I32Eqz)
                .op(Op::If(BlockType::Empty));
            self.memory_failure(read_exit + 1);
            self.op(Op::End)
                .op(Op::LocalGet(VALUE))
                .op(Op::I32WrapI64)
                .op(Op::I64Load(MemArg {
                    offset: std::mem::offset_of!(ReturnRead, value) as u64,
                    align: 3,
                    memory_index: 0,
                }))
                .op(Op::LocalSet(VALUE));
            if indexed {
                self.op(Op::End);
            }
            return;
        }
        let packed = u32::from(width.bytes());
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let packed = if indexed {
            packed | crate::embedding::layout::paging::INDEXED_SCALAR_READ
        } else {
            packed
        };
        self.op(Op::I32Const(packed as i32));
        if !self.context.is_long64() {
            self.op(Op::I32Const(self.segment as i32));
        }
        self.op(Op::Call(if for_write { 3 } else { 0 }))
            .op(Op::LocalSet(VALUE))
            .op(Op::Call(2))
            .op(Op::If(BlockType::Empty));
        self.memory_failure(read_exit + 1);
        self.op(Op::End);
        if indexed {
            self.op(Op::End);
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if !for_write && let Some(ready) = self.checked_paged_read_rearm(width) {
            self.rearm_paged_read(width, ready);
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn checked_paged_read_rearm(&self, width: Width) -> Option<u32> {
        let (ready, order) = self.paged_read_rearm?;
        let segment = match (width, self.segment) {
            (Width::W8 | Width::W32, Segment::Ds) => true,
            (Width::W32, Segment::Ss) => self.paged_loop_write_dword.is_some(),
            _ => false,
        };
        (!self.guarded_paged_lap && segment && order.position(self.current_index).is_some())
            .then_some(ready)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn rearm_paged_read(&mut self, width: Width, ready: u32) {
        self.rearm_paged_access(width.bytes(), ready);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn rearm_paged_access(&mut self, bytes: u8, ready: u32) {
        let (_, attempts) = self.paged_loop_guard.expect("paged loop guard");
        let cooldown = if bytes == 16 && self.paged_copy_window.is_some() {
            // A checked copy lap can establish a new window past a page boundary.
            PAGED_READ_REPLAY - 1
        } else {
            PAGED_READ_COOLDOWN
        };
        self.op(Op::LocalGet(attempts))
            .op(Op::I32Const(PAGED_READ_REPLAY))
            .op(Op::I32Eq)
            .op(Op::If(BlockType::Empty))
            .op(Op::I32Const(cooldown))
            .op(Op::I32Const(2));
        self.linear_address();
        self.op(Op::I32Const(i32::from(bytes)))
            .op(Op::Call(ready))
            .op(Op::Select)
            .op(Op::LocalSet(attempts));
        if bytes == 16
            && let Some(window) = self.paged_vector_window
            && let Some(last_early_page) = window.last_early_page
        {
            // A checked vector may reach the next page by starting there or
            // straddling its boundary. Recheck the guard once per such page.
            self.op(Op::LocalGet(ADDRESS))
                .op(Op::I32WrapI64)
                .op(Op::I32Const(4095))
                .op(Op::I32And)
                .op(Op::I32Const(1))
                .op(Op::I32Sub)
                .op(Op::I32Const(4096 - 17))
                .op(Op::I32GtU)
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(attempts))
                .op(Op::I32Const(PAGED_READ_COOLDOWN))
                .op(Op::I32Eq)
                .op(Op::LocalGet(window.host))
                .op(Op::I32Const(0))
                .op(Op::I32Ne)
                .op(Op::I32And)
                .op(Op::LocalGet(ADDRESS))
                .op(Op::I64Const(15))
                .op(Op::I64Add)
                .op(Op::I64Const(-4096))
                .op(Op::I64And)
                .op(Op::LocalTee(VALUE))
                .op(Op::LocalGet(window.base))
                .op(Op::I64Const(-4096))
                .op(Op::I64And)
                .op(Op::I64Sub)
                .op(Op::I64Const(4096))
                .op(Op::I64Eq)
                .op(Op::I32And)
                .op(Op::LocalGet(VALUE))
                .op(Op::LocalGet(last_early_page))
                .op(Op::I64GtU)
                .op(Op::I32And)
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(VALUE))
                .op(Op::LocalSet(last_early_page))
                .op(Op::I32Const(0))
                .op(Op::LocalSet(attempts))
                .op(Op::End)
                .op(Op::End);
        }
        self.op(Op::End);
    }

    pub(super) fn store_result(&mut self, width: Width, exit_depth: u32) {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let stack_store = match self.current_operation {
            crate::x86::instruction::Operation::Push { .. } => {
                self.linear_call_spans
                    || (self.sampled_region
                        && !self.paged
                        && width == Width::W32
                        && self.context.stack_width() == Width::W32)
            }
            crate::x86::instruction::Operation::Call { .. } => self.linear_call_spans,
            _ => false,
        };
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if stack_store
            && self.direct_access(
                DirectOperand::ScalarStore(width),
                exit_depth,
                |emitter, depth| {
                    emitter.store_result_slow(width, depth);
                },
            )
        {
            return;
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if self.parallel_store(width, exit_depth) {
            return;
        }
        self.store_result_slow(width, exit_depth);
    }

    pub(super) fn store_result_slow(&mut self, width: Width, exit_depth: u32) {
        self.stores = true;
        self.linear_address();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let fill_index = self.uses_indexed_scalar_write(self.current_operation);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let indexed = fill_index && !self.uses_indexed_scalar_rmw();
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        let indexed = false;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if indexed {
            self.op(Op::LocalSet(ADDRESS));
            self.indexed_read_host(
                self.indexed_paged_read.unwrap(),
                width.bytes(),
                false,
                IndexedOperand::Write,
            );
            self.op(Op::LocalTee(STORE_STATUS))
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::LocalGet(RESULT));
            self.direct_scalar_store(
                width,
                MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                },
            );
            self.op(Op::I32Const(1))
                .op(Op::LocalSet(STORE_STATUS))
                .op(Op::Else)
                .op(Op::LocalGet(ADDRESS));
        } else if fill_index {
            // A checked RMW consumes its prepared write before filling the slot.
            self.op(Op::LocalTee(ADDRESS));
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        self.require_checked_access_finish();
        let bytes = i32::from(width.bytes());
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let store_width = if self.paged
            && self.context.is_long64()
            && self.segment == Segment::Ss
            && width == Width::W64
            && matches!(
                self.current_operation,
                crate::x86::instruction::Operation::Call { .. }
            ) {
            crate::embedding::layout::PAGED_STACK_QWORD as i32
        } else {
            bytes
        };
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        let store_width = bytes;
        self.op(Op::I32Const(store_width)).op(Op::LocalGet(RESULT));
        if !self.context.is_long64() {
            self.op(Op::I32Const(self.segment as i32));
        }
        self.op(Op::Call(1))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.memory_failure(exit_depth + 1 + u32::from(indexed));
        self.op(Op::End);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if fill_index {
            let fill = self.indexed_store_fill.unwrap();
            self.op(Op::LocalGet(STORE_STATUS))
                .op(Op::I32Const(1))
                .op(Op::I32Eq)
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::I32Const(bytes))
                .op(Op::Call(fill))
                .op(Op::End);
            if indexed {
                self.op(Op::End);
            }
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn write_span(&self) -> Option<SpanCache> {
        self.write_spans[usize::from(self.segment == Segment::Ss)]
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn try_guarded_store(
        &mut self,
        instruction: crate::x86::instruction::Instruction,
        exit_depth: u32,
    ) -> bool {
        use crate::embedding::layout::debug::CONTINUE_ACTIVE;
        use crate::x86::instruction::Operation;
        use wasm_encoder::ValType;

        let Operation::Store {
            src,
            address,
            width,
        } = instruction.operation
        else {
            return false;
        };
        if self.guarded_paged_lap
            || !self.retouch_prefix_fetch
            || !self.can_complete_paged_access_quietly(instruction.operation)
            || instruction.next_pc <= instruction.pc
            || (instruction.next_pc - 1) >> 12 != instruction.pc >> 12
        {
            return false;
        }

        self.newest_prefix_fetch(instruction.pc, true, false);
        self.op(Op::If(BlockType::Result(ValType::I32)));
        self.address(address, instruction.next_pc);
        self.op(Op::LocalSet(ADDRESS));
        self.linear_address();
        self.op(Op::LocalSet(ADDRESS));
        self.indexed_read_host(
            self.indexed_paged_read.unwrap(),
            width.bytes(),
            false,
            IndexedOperand::Write,
        );
        self.op(Op::Else)
            .op(Op::I32Const(0))
            .op(Op::End)
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(STORE_STATUS));
        self.data_operand(src, width);
        self.direct_scalar_store(
            width,
            MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            },
        );
        let result = self.boundary_result.unwrap();
        self.op(Op::I32Const(CONTINUE_ACTIVE))
            .op(Op::LocalSet(result))
            .op(Op::I32Const(1))
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::Else);
        self.begin(instruction.operation, instruction.next_pc, exit_depth + 1);
        self.store(src, address, width, instruction.next_pc, exit_depth + 1);
        self.op(Op::End);
        true
    }

    pub(super) fn store(
        &mut self,
        src: DataOperand,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some((write, indexed)) = self.paged_prefix_store {
            debug_assert!(width == Width::W32 && address.segment == Segment::Ds);
            self.address(address, next_pc);
            if indexed {
                self.op(Op::LocalSet(ADDRESS));
                self.indexed_read_host(
                    self.indexed_paged_read.unwrap(),
                    width.bytes(),
                    true,
                    IndexedOperand::Write,
                );
                self.op(Op::LocalTee(STORE_STATUS))
                    .op(Op::If(BlockType::Empty))
                    .op(Op::LocalGet(STORE_STATUS));
                self.data_operand(src, width);
                self.direct_scalar_store(
                    width,
                    MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    },
                );
                self.op(Op::Else).op(Op::LocalGet(ADDRESS));
            }
            self.data_operand(src, width);
            self.op(Op::I32WrapI64)
                .op(Op::Call(write))
                .op(Op::I32Eqz)
                .op(Op::If(BlockType::Empty));
            let depth = exit_depth + u32::from(indexed);
            self.route(self.current_pc, depth, depth + 1);
            self.op(Op::End);
            if indexed {
                self.op(Op::End);
            }
            return;
        }
        let paged_write = match (width, address.segment) {
            (Width::W8, Segment::Ds) => self.paged_loop_write_byte,
            (Width::W32, Segment::Ss) => self.paged_loop_write_dword,
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            (Width::W32, Segment::Ds)
                if self.paged_loop_read_block.is_some()
                    || self
                        .structured_loop
                        .is_some_and(|(body, _)| body.paged_scalar_writes()) =>
            {
                self.paged_loop_write_dword
            }
            _ => None,
        };
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let pair = self
            .paged_byte_write_pair
            .filter(|pair| pair.store == self.current_index);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let host = pair.and_then(|pair| pair.host).or_else(|| {
            self.indexed_write()
                .filter(|group| group.shape.store == self.current_index)
                .map(|group| group.host)
        });
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let paged_write = pair.map(|pair| pair.write).or(paged_write);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let write_ready = self
            .paged_write_ready
            .filter(|_| matches!((width, address.segment), (Width::W32, Segment::Ds)));
        if self.guarded_paged_lap
            && let Some(write) = paged_write
        {
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            if let Some(host) = host {
                self.op(Op::LocalGet(host))
                    .op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                    .op(Op::LocalGet(host));
                self.data_operand(src, width);
                self.direct_scalar_store(
                    width,
                    MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    },
                );
                self.op(Op::I32Const(1)).op(Op::Else);
            }
            self.address(address, next_pc);
            self.data_operand(src, width);
            // The helper admits only writes that leave the active source valid.
            self.op(Op::I32WrapI64);
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            if let Some(pair) = pair {
                self.op(Op::LocalGet(pair.slot));
            }
            self.op(Op::Call(write));
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            if host.is_some() {
                self.op(Op::End);
            }
            self.op(Op::I32Eqz);
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            let replay = write_ready.is_some();
            #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
            let replay = false;
            self.retry_guarded_paged_operand(exit_depth, replay);
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let indexed = self.uses_indexed_scalar_write(self.current_operation);
        #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
        let indexed = false;
        if indexed {
            self.data_operand(src, width);
            self.op(Op::LocalSet(RESULT));
            self.store_result_slow(width, exit_depth);
        } else {
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(ready) = write_ready.filter(|_| {
            self.paged_read_rearm
                .is_some_and(|(_, order)| order.position(self.current_index).is_some())
        }) {
            self.rearm_paged_access(width.bytes(), ready);
        }
    }
}
