#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::indexed::IndexedOperand;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::dbt::wasm::{ADDRESS, STORE_STATUS};
use crate::dbt::wasm::{ENTRY, Emitter, VALUE};
use crate::x86::context::Segment;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::x86::instruction::AddressBase;
use crate::x86::instruction::{Address, Width};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use wasm_encoder::MemArg;
use wasm_encoder::{BlockType, Instruction as Op};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) const PAGED_READ_REPLAY: i32 = -1;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
// The replayed lap counts first; the next seven headers run checked.
const PAGED_READ_COOLDOWN: i32 = PAGED_READ_REPLAY - 7;

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct PagedReadWindow {
    pub(in crate::dbt::wasm) resolve: u32,
    pub(in crate::dbt::wasm) base: u32,
    pub(in crate::dbt::wasm) limit: u32,
    pub(in crate::dbt::wasm) host: u32,
    pub(in crate::dbt::wasm) last_early_page: Option<u32>,
    pub(in crate::dbt::wasm) retouch: Option<VectorWindowRetouch>,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct VectorWindowRetouch {
    pub(in crate::dbt::wasm) alignment: (u8, i64),
    pub(in crate::dbt::wasm) resolve: u32,
    pub(in crate::dbt::wasm) touch: u32,
    pub(in crate::dbt::wasm) dirty: u32,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct PagedDwordReadPair {
    pub(in crate::dbt::wasm) first: u32,
    pub(in crate::dbt::wasm) second: u32,
    pub(in crate::dbt::wasm) base: u8,
    pub(in crate::dbt::wasm) resolve: u32,
    pub(in crate::dbt::wasm) host: u32,
    pub(in crate::dbt::wasm) resolver_mode: u32,
    pub(in crate::dbt::wasm) handoff: Option<DwordPairHandoff>,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct DwordPairHandoff {
    pub(in crate::dbt::wasm) read: u32,
    pub(in crate::dbt::wasm) address: u32,
    pub(in crate::dbt::wasm) ready: u32,
}

impl Emitter {
    pub(in crate::dbt::wasm) fn load_guarded_paged_value(
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
    pub(in crate::dbt::wasm) fn refresh_direct_read_range(
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

    pub(in crate::dbt::wasm) fn retry_guarded_paged_operand(
        &mut self,
        exit_depth: u32,
        _replay: bool,
    ) {
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
            .op(Op::LocalSet(ENTRY))
            .op(Op::Br(exit_depth))
            .op(Op::End);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(in crate::dbt::wasm) fn checked_paged_read_rearm(&self, width: Width) -> Option<u32> {
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
    pub(in crate::dbt::wasm) fn rearm_paged_read(&mut self, width: Width, ready: u32) {
        self.rearm_paged_access(width.bytes(), ready);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(in crate::dbt::wasm) fn rearm_paged_access(&mut self, bytes: u8, ready: u32) {
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
}
