use crate::dbt::wasm::{
    ADDRESS, Emitter, LHS, REMAINING, RESULT, RHS, STORE_STATUS, VALUE, execution::control,
};
use crate::embedding::layout::RunState;
use crate::x86::{
    context::Segment,
    float::{Format, Kind},
    instruction::{
        Address, AddressBase, Alu, Instruction, Location, MemoryAluOperands, Operation,
        VectorInstruction, VectorOperand, Width,
    },
};
use wasm_encoder::{BlockType, Instruction as Op, MemArg, ValType};

const BOUNDARY: i32 = 29;
const ATTEMPTS: i32 = crate::memory::SHARED_CAS_ATTEMPTS as i32;
pub(in crate::dbt::wasm) const VECTOR_SPAN_TRIAL_LAPS: i64 = 3;
const LANE: MemArg = MemArg {
    offset: 0,
    align: 3,
    memory_index: 1,
};

pub(in crate::dbt::wasm) fn native_lock_memo_site(instructions: &[Instruction]) -> Option<u32> {
    let mut add = None;
    let mut other = None;
    let mut multiple_others = false;
    for (index, instruction) in instructions.iter().enumerate() {
        let Operation::MemoryAlu {
            kind,
            width: Width::W64,
            operands: MemoryAluOperands::MemoryFirst(_),
            address:
                Address {
                    base: AddressBase::Register(bank),
                    index: None,
                    segment: Segment::Ds,
                    size: Width::W64,
                    ..
                },
        } = instruction.operation
        else {
            continue;
        };
        if !instruction.explicit_lock || bank == 4 {
            continue;
        }
        match kind {
            Alu::Add => {
                if add.replace((index, instruction)).is_some() {
                    return None;
                }
            }
            Alu::Sub | Alu::And | Alu::Or | Alu::Xor => {
                multiple_others |= other.replace((index, instruction)).is_some();
            }
            _ => {}
        }
    }
    // Keep an existing ADD site even when the unit also contains another native RMW.
    let (index, site) = add.or(other.filter(|_| !multiple_others))?;
    instructions
        .iter()
        .any(|instruction| {
            matches!(instruction.operation,
                Operation::Jump { target } | Operation::Jcc { target, .. }
                | Operation::CountBranch { target, .. }
                if target <= site.pc && site.pc <= instruction.pc)
        })
        .then_some(index as u32)
}

#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct Atomic {
    enter: u32,
    leave: u32,
    guard: u32,
    unguard: u32,
    token: u32,
    guarded: u32,
}

impl Atomic {
    pub(in crate::dbt::wasm) fn new(first: u32, token: u32) -> Self {
        Self {
            enter: first,
            leave: first + 1,
            guard: first + 2,
            unguard: first + 3,
            token,
            guarded: token + 1,
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct Scalar {
    resolve: u32,
    address: u32,
    attempts: u32,
    old: u32,
    shift: u32,
    mask: u32,
    payload: u32,
}

impl Scalar {
    pub(in crate::dbt::wasm) fn new(resolve: u32, first: u32) -> Self {
        Self {
            resolve,
            address: first,
            attempts: first + 1,
            old: first + 2,
            shift: first + 3,
            mask: first + 4,
            payload: first + 5,
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct VectorSpanMemo {
    key: Address,
    offset: u32,
    limit: u32,
    canonical: u32,
}

impl VectorSpanMemo {
    pub(in crate::dbt::wasm) fn new(key: Address, first: u32) -> Self {
        Self {
            key,
            offset: first,
            limit: first + 1,
            canonical: first + 2,
        }
    }

    pub(in crate::dbt::wasm) fn groups(instructions: &[Instruction]) -> [Option<Address>; 2] {
        let mut groups = [None; 2];
        for key in instructions.iter().filter_map(|i| Self::key(i.operation)) {
            if groups.contains(&Some(key)) {
                continue;
            }
            let Some(slot) = groups.iter_mut().find(|slot| slot.is_none()) else {
                return [None; 2];
            };
            *slot = Some(key);
        }
        groups
    }

    fn key(operation: Operation) -> Option<Address> {
        let address = match operation {
            Operation::Float(float)
                if float.op.scalar
                    && float.op.format == Format::F32
                    && matches!(float.op.kind, Kind::Add | Kind::Mul) =>
            {
                let Location::Memory(address) = float.src else {
                    return None;
                };
                address
            }
            Operation::Vector(VectorInstruction::Move {
                operand: VectorOperand::Memory(address),
                bytes: 4,
                aligned: false,
                store: false,
                ..
            }) => address,
            _ => return None,
        };
        if address.index.is_none() || !matches!(address.size, Width::W32 | Width::W64) {
            return None;
        }
        Some(Address {
            displacement: 0,
            ..address
        })
    }
}

#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct DwordWindow {
    resolve: u32,
    base: u32,
    limit: u32,
    host: u32,
    guard_bias: u32,
    wide: bool,
    allow_bytes: bool,
}

impl DwordWindow {
    pub(in crate::dbt::wasm) fn new(
        resolve: u32,
        first: u32,
        wide: bool,
        allow_bytes: bool,
    ) -> Self {
        Self {
            resolve,
            base: first,
            limit: first + 1,
            host: first + 2,
            guard_bias: first + 3,
            wide,
            allow_bytes,
        }
    }

    pub(in crate::dbt::wasm) fn eligible(
        operation: Operation,
        allow_bytes: bool,
        wide: bool,
    ) -> bool {
        let address = match operation {
            Operation::Load {
                address,
                width: Width::W32,
                ..
            }
            | Operation::MemoryAlu {
                address,
                width: Width::W32,
                kind: Alu::Cmp | Alu::Test,
                ..
            } => Some(address),
            _ if allow_bytes => Self::byte_address(operation),
            _ => None,
        };
        address.is_some_and(|address| Self::supports(address, wide) && address.index.is_some())
    }

    pub(in crate::dbt::wasm) fn eligible_base_byte(operation: Operation, wide: bool) -> bool {
        Self::byte_address(operation).is_some_and(|address| {
            Self::supports(address, wide)
                && address.index.is_none()
                && matches!(address.base, AddressBase::Register(_))
        })
    }

    fn supports(address: Address, wide: bool) -> bool {
        address.segment == Segment::Ds && (!wide || address.size == Width::W64)
    }

    fn byte_address(operation: Operation) -> Option<Address> {
        match operation {
            Operation::Load {
                address,
                width: Width::W8,
                ..
            }
            | Operation::Extend {
                src: Location::Memory(address),
                source_width: Width::W8,
                ..
            } => Some(address),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct ByteStoreSpan {
    resolve: u32,
    base: u32,
    limit: u32,
    host: u32,
}

impl ByteStoreSpan {
    pub(in crate::dbt::wasm) fn new(resolve: u32, first: u32) -> Self {
        Self {
            resolve,
            base: first,
            limit: first + 1,
            host: first + 2,
        }
    }

    pub(in crate::dbt::wasm) fn eligible(operation: Operation, wide: bool) -> bool {
        let address_width = if wide { Width::W64 } else { Width::W32 };
        matches!(operation, Operation::Store { address, width: Width::W8, .. }
            if address.segment == Segment::Ds && address.size == address_width && address.index.is_some())
    }
}

impl Emitter {
    pub(in crate::dbt::wasm) fn begin_parallel_instruction(
        &mut self,
        instruction: Instruction,
        exit_depth: u32,
    ) -> u32 {
        if !instruction.shared_atomic() {
            return exit_depth;
        }
        if let Some(scalar) = self.parallel_scalar {
            if matches!(instruction.operation, Operation::CompareExchange16b { .. }) {
                return exit_depth;
            }
            self.op(Op::I32Const(ATTEMPTS))
                .op(Op::LocalSet(scalar.attempts))
                .op(Op::Loop(BlockType::Empty));
            self.parallel_retry = Some(exit_depth + 1);
            return exit_depth + 1;
        }
        self.enter_parallel_atomic();
        exit_depth
    }

    pub(in crate::dbt::wasm) fn end_parallel_instruction(
        &mut self,
        instruction: Instruction,
        exit_depth: u32,
    ) -> u32 {
        if !instruction.shared_atomic() {
            return exit_depth;
        }
        if self.parallel_retry.take().is_some() {
            self.op(Op::End);
            self.leave_parallel_atomic_if_held();
            return exit_depth - 1;
        }
        self.leave_parallel_atomic_if_held();
        exit_depth
    }

    pub(in crate::dbt::wasm) fn enter_parallel_atomic(&mut self) {
        let atomic = self.parallel_atomic.expect("locked instruction imports");
        self.op(Op::Call(atomic.enter))
            .op(Op::LocalSet(atomic.token));
    }

    pub(in crate::dbt::wasm) fn enter_parallel_guarded(&mut self, width: u32) {
        let atomic = self.parallel_atomic.expect("locked instruction imports");
        let segment = self.shared_segment();
        self.linear_address();
        self.op(Op::I32Const(width as i32))
            .op(Op::I32Const(segment))
            .op(Op::Call(atomic.guard))
            .op(Op::LocalTee(atomic.guarded))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.enter_parallel_atomic();
        self.op(Op::End);
    }

    pub(in crate::dbt::wasm) fn leave_parallel_atomic_if_held(&mut self) {
        let Some(atomic) = self.parallel_atomic else {
            return;
        };
        self.op(Op::LocalGet(atomic.token))
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(atomic.token))
            .op(Op::Call(atomic.leave))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(atomic.token))
            .op(Op::End)
            .op(Op::LocalGet(atomic.guarded))
            .op(Op::If(BlockType::Empty))
            .op(Op::Call(atomic.unguard))
            .op(Op::I32Const(0))
            .op(Op::LocalSet(atomic.guarded))
            .op(Op::End);
    }

    pub(in crate::dbt::wasm) fn parallel_boundary(&mut self, exit_depth: u32) {
        self.parallel_boundary_with_flags(exit_depth, false);
    }

    pub(in crate::dbt::wasm) fn parallel_boundary_with_flags(
        &mut self,
        exit_depth: u32,
        compare: bool,
    ) {
        self.parallel_signal();
        self.op(Op::If(BlockType::Empty));
        if compare {
            self.restore_repeat_flags();
        }
        if self.loop_gates_prechecked {
            self.restore_loop_registers();
        }
        self.exit(BOUNDARY, exit_depth + 1);
        self.op(Op::End);
    }

    fn shared_address(&mut self, scalar: Scalar, width: Width, access: i32) {
        let memo = if self.parallel_memo_address && access != 2 && self.parallel_retry.is_none() {
            self.parallel_address_memos.pop()
        } else {
            None
        };
        self.resolve_shared_address(scalar, width, access, memo);
    }

    fn resolve_shared_address(
        &mut self,
        scalar: Scalar,
        width: Width,
        access: i32,
        memo: Option<(u32, u32)>,
    ) {
        let segment = self.shared_segment();
        self.linear_address();
        if let Some((address, resolved)) = memo {
            // Memo sites live only inside an invocation. A parked effect sets the
            // phase signal before its grant, so it cannot reuse an old decision.
            self.op(Op::LocalTee(scalar.payload))
                .op(Op::LocalGet(address))
                .op(Op::I64Eq)
                .op(Op::LocalGet(resolved))
                .op(Op::I32Eqz)
                .op(Op::I32Eqz)
                .op(Op::I32And)
                .op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)));
            self.parallel_signal();
            self.op(Op::I32Eqz)
                .op(Op::Else)
                .op(Op::I32Const(0))
                .op(Op::End)
                .op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                .op(Op::LocalGet(resolved))
                .op(Op::Else)
                .op(Op::LocalGet(scalar.payload))
                .op(Op::LocalTee(address));
        }
        self.op(Op::I32Const(i32::from(width.bytes())))
            .op(Op::I32Const(access))
            .op(Op::I32Const(segment))
            .op(Op::Call(scalar.resolve));
        if let Some((_, resolved)) = memo {
            self.op(Op::LocalTee(resolved)).op(Op::End);
        }
        self.op(Op::LocalSet(scalar.address));
    }

    fn parallel_signal(&mut self) {
        self.op(Op::GlobalGet(0));
        self.shared_memory_address();
        self.op(Op::I32AtomicLoad(MemArg {
            offset: 0,
            align: 2,
            memory_index: 1,
        }));
    }

    fn shared_memory_address(&mut self) {
        #[cfg(parallel_memory64)]
        if self.shared_ram_memory64 {
            self.op(Op::I64ExtendI32U);
        }
    }

    fn shared_segment(&self) -> i32 {
        if self.context.is_long64() {
            -1
        } else {
            self.segment as i32
        }
    }

    fn lane_address(&mut self, scalar: Scalar) {
        self.op(Op::LocalGet(scalar.address))
            .op(Op::I32Const(!7))
            .op(Op::I32And);
        self.shared_memory_address();
    }

    fn lane_shift(&mut self, scalar: Scalar) {
        self.op(Op::LocalGet(scalar.address))
            .op(Op::I32Const(7))
            .op(Op::I32And)
            .op(Op::I32Const(3))
            .op(Op::I32Shl)
            .op(Op::I64ExtendI32U);
    }

    pub(in crate::dbt::wasm) fn native_locked_integer(
        &mut self,
        kind: Alu,
        address: Address,
        operands: MemoryAluOperands,
        width: Width,
        next_pc: u64,
    ) -> bool {
        let (Some(scalar), Some(_), Width::W64, MemoryAluOperands::MemoryFirst(source)) =
            (self.parallel_scalar, self.parallel_retry, width, operands)
        else {
            return false;
        };
        let instruction = match kind {
            Alu::Add => Op::I64AtomicRmwAdd(LANE),
            Alu::Sub => Op::I64AtomicRmwSub(LANE),
            Alu::And => Op::I64AtomicRmwAnd(LANE),
            Alu::Or => Op::I64AtomicRmwOr(LANE),
            Alu::Xor => Op::I64AtomicRmwXor(LANE),
            _ => return false,
        };
        self.address(address, next_pc);
        self.op(Op::LocalSet(ADDRESS));
        let memo = self
            .parallel_native_lock_memo
            .filter(|(site, _, _)| *site == self.current_index)
            .map(|(_, address, resolved)| (address, resolved));
        self.resolve_shared_address(scalar, width, 2, memo);
        self.op(Op::LocalGet(scalar.address))
            .op(Op::If(BlockType::Empty));
        self.data_operand(source, width);
        self.op(Op::LocalSet(RHS)).op(Op::LocalGet(scalar.address));
        self.shared_memory_address();
        self.op(Op::LocalGet(RHS))
            .op(instruction)
            .op(Op::LocalSet(LHS));
        if matches!(kind, Alu::Add | Alu::Sub) {
            self.arithmetic_result(kind, width);
            self.arithmetic_flags(kind, width);
        } else {
            self.op(Op::LocalGet(LHS)).op(Op::LocalGet(RHS));
            self.logical_result(kind);
            self.logical_flags(width);
        }
        self.op(Op::I32Const(1))
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::Else);
        true
    }

    pub(in crate::dbt::wasm) fn parallel_load(
        &mut self,
        width: Width,
        for_write: bool,
        exit_depth: u32,
    ) -> bool {
        let Some(scalar) = self.parallel_scalar else {
            return false;
        };
        let locked = self.parallel_retry.is_some();
        if locked {
            debug_assert!(for_write);
            self.op(Op::LocalGet(scalar.attempts))
                .op(Op::If(BlockType::Empty));
        }
        self.shared_address(scalar, width, if for_write { 2 } else { 0 });
        if locked {
            self.op(Op::Else)
                .op(Op::I32Const(0))
                .op(Op::LocalSet(scalar.address))
                .op(Op::End);
        }
        self.op(Op::LocalGet(scalar.address))
            .op(Op::If(BlockType::Empty));
        self.read_shared_lane(scalar, width, locked);
        self.narrow(width);
        self.op(Op::LocalSet(VALUE)).op(Op::Else);
        if locked {
            self.enter_parallel_guarded(u32::from(width.bytes()));
        }
        self.load_address_slow(width, for_write, exit_depth + 1);
        self.op(Op::End);
        true
    }

    pub(in crate::dbt::wasm) fn parallel_load_dword_window(
        &mut self,
        width: Width,
        exit_depth: u32,
    ) -> bool {
        let operation = self.current_operation;
        let window = self
            .parallel_dword_window
            .filter(|window| DwordWindow::eligible(operation, window.allow_bytes, window.wide))
            .or_else(|| {
                self.parallel_base_byte_window
                    .filter(|window| DwordWindow::eligible_base_byte(operation, window.wide))
            });
        let (Some(window), Some(scalar)) = (window, self.parallel_scalar) else {
            return false;
        };
        debug_assert!(self.parallel_retry.is_none());
        debug_assert!(matches!(width, Width::W8 | Width::W32));
        // Byte reads reuse the dword-start bounds. The checked read handles the tail.
        let delta = if window.wide {
            scalar.old
        } else {
            STORE_STATUS
        };
        self.op(Op::Block(BlockType::Result(ValType::I32)));
        self.dword_window_delta(window, delta);
        self.op(Op::LocalGet(window.limit))
            .op(if window.wide { Op::I64GeU } else { Op::I32GeU })
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::Call(window.resolve));
        if window.wide {
            self.op(Op::LocalTee(scalar.address));
        }
        self.op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty))
            .op(Op::I32Const(0))
            .op(Op::Br(2))
            .op(Op::End);
        if window.wide {
            self.op(Op::LocalGet(scalar.address))
                .op(Op::I64ExtendI32U)
                .op(Op::LocalSet(window.limit));
            self.dword_window_field(window.base, 0, true);
        } else {
            self.dword_window_field(window.base, 0, false);
            self.dword_window_field(window.limit, 4, false);
        }
        self.dword_window_field(window.host, 8, false);
        self.dword_window_field(window.guard_bias, 12, false);
        self.dword_window_delta(window, delta);
        self.op(Op::Drop).op(Op::End);
        self.op(Op::LocalGet(delta));
        if window.wide {
            self.op(Op::I32WrapI64);
        }
        self.op(Op::LocalGet(window.host)).op(Op::I32Add);
        if width == Width::W32 {
            self.op(Op::LocalTee(scalar.address))
                .op(Op::I32Const(7))
                .op(Op::I32And)
                .op(Op::I32Const(4))
                .op(Op::I32LeU)
                .op(Op::If(BlockType::Result(ValType::I32)));
        } else {
            self.op(Op::LocalSet(scalar.address));
        }
        self.op(Op::LocalGet(scalar.address))
            .op(Op::I32Const(6))
            .op(Op::I32ShrU)
            .op(Op::I32Const(2))
            .op(Op::I32Shl)
            .op(Op::LocalGet(window.guard_bias))
            .op(Op::I32Add);
        self.shared_memory_address();
        self.op(Op::I32AtomicLoad(MemArg {
            offset: 0,
            align: 2,
            memory_index: 1,
        }))
        .op(Op::I32Eqz);
        if width == Width::W32 {
            self.op(Op::Else).op(Op::I32Const(0)).op(Op::End);
        }
        self.op(Op::End).op(Op::If(BlockType::Empty));
        self.read_shared_lane(scalar, width, false);
        self.narrow(width);
        self.op(Op::LocalSet(VALUE)).op(Op::Else);
        // A rejected window, lane, or stripe requires the checked read.
        self.load_address_slow(width, false, exit_depth + 1);
        self.op(Op::End);
        true
    }

    fn dword_window_field(&mut self, field: u32, offset: u64, wide: bool) {
        let memory = MemArg {
            offset: std::mem::offset_of!(RunState, vector_scratch) as u64 + offset,
            align: if wide { 3 } else { 2 },
            memory_index: 0,
        };
        self.op(Op::LocalGet(0))
            .op(if wide {
                Op::I64Load(memory)
            } else {
                Op::I32Load(memory)
            })
            .op(Op::LocalSet(field));
    }

    fn dword_window_delta(&mut self, window: DwordWindow, delta: u32) {
        self.op(Op::LocalGet(ADDRESS));
        if !window.wide {
            self.op(Op::I32WrapI64);
        }
        self.op(Op::LocalGet(window.base))
            .op(if window.wide { Op::I64Sub } else { Op::I32Sub })
            .op(Op::LocalTee(delta));
    }

    pub(in crate::dbt::wasm) fn parallel_vector_dword(&mut self, exit_depth: u32) -> bool {
        let (Some(helper), Some(scalar)) = (self.parallel_vector_dword, self.parallel_scalar)
        else {
            return false;
        };
        debug_assert!(self.parallel_retry.is_none());
        let span = self
            .parallel_vector_span
            .filter(|_| self.loop_gates_prechecked)
            .and_then(|(helper, memos, budget_floor)| {
                let key = VectorSpanMemo::key(self.current_operation)?;
                let memo = memos.into_iter().flatten().find(|memo| memo.key == key)?;
                Some((helper, memo, budget_floor))
            });
        let (helper, exit_depth) = if let Some((helper, memo, budget_floor)) = span {
            let (body, _) = self.structured_loop.expect("shared vector span loop");
            let lap = i64::from(body.max_lap_len);
            self.op(Op::LocalTee(scalar.payload))
                .op(Op::LocalGet(memo.offset))
                .op(Op::I64Sub)
                .op(Op::LocalTee(scalar.old))
                .op(Op::LocalGet(memo.limit))
                .op(Op::I64LtU)
                .op(Op::LocalGet(scalar.old))
                .op(Op::I64Const(3))
                .op(Op::I64And)
                .op(Op::I64Eqz)
                .op(Op::I32And)
                .op(Op::If(BlockType::Result(ValType::I64)))
                .op(Op::I64Const(lap))
                .op(Op::LocalSet(budget_floor))
                .op(Op::LocalGet(memo.canonical))
                .op(Op::LocalGet(scalar.old))
                .op(Op::I32WrapI64)
                .op(Op::I32Add)
                .op(Op::LocalSet(scalar.address));
            self.read_shared_lane(scalar, Width::W32, false);
            self.op(Op::Else);
            if !self.compact_shared_reads {
                self.rearm_vector_span_trial(
                    lap,
                    i64::from(self.current_index - body.head),
                    budget_floor,
                );
            }
            self.op(Op::LocalGet(scalar.payload));
            (helper, exit_depth + 1)
        } else {
            (helper, exit_depth)
        };
        let segment = self.segment as i32;
        self.op(Op::I32Const(segment))
            .op(Op::Call(helper))
            .op(Op::LocalTee(ADDRESS))
            .op(Op::I64Const(0))
            .op(Op::I64GtS)
            .op(Op::If(BlockType::Result(ValType::I64)))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::LocalSet(scalar.address));
        if let Some((_, memo, _)) = span {
            // A successful direct admission grants an unpromoted stripe
            // in this invocation. Slow reads still poll at their successor.
            self.op(Op::LocalGet(scalar.address))
                .op(Op::LocalSet(memo.canonical))
                .op(Op::LocalGet(scalar.payload))
                .op(Op::LocalSet(memo.offset))
                .op(Op::LocalGet(ADDRESS))
                .op(Op::I64Const(32))
                .op(Op::I64ShrU)
                .op(Op::LocalSet(memo.limit));
        }
        self.read_shared_lane(scalar, Width::W32, false);
        self.op(Op::Else)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I64Eqz)
            .op(Op::If(BlockType::Empty));
        self.vector_failure(exit_depth + 2);
        self.op(Op::End).op(Op::LocalGet(ADDRESS)).op(Op::End);
        if span.is_some() {
            self.op(Op::End);
        }
        self.narrow(Width::W32);
        self.op(Op::LocalSet(ADDRESS))
            .op(Op::V128Const(0))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I64x2ReplaceLane(0));
        true
    }

    fn rearm_vector_span_trial(&mut self, lap: i64, position: i64, budget_floor: u32) {
        debug_assert!(position < lap);
        debug_assert!(matches!(
            self.instruction_retirement,
            control::InstructionRetirement::One
        ));
        // A cleared trial can restart on a miss. Account for the already
        // retired prefix so the same gate still admits exactly three laps.
        let trial = VECTOR_SPAN_TRIAL_LAPS * lap;
        self.op(Op::LocalGet(budget_floor))
            .op(Op::I64Const(lap))
            .op(Op::I64Eq)
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(trial + lap - position))
            .op(Op::I64GeU)
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(trial - position - 1))
            .op(Op::I64Sub)
            .op(Op::LocalSet(budget_floor))
            .op(Op::End)
            .op(Op::End);
    }

    fn read_shared_lane(&mut self, scalar: Scalar, width: Width, locked: bool) {
        self.lane_address(scalar);
        self.op(Op::I64AtomicLoad(LANE));
        if locked {
            self.op(Op::LocalTee(scalar.old));
        }
        if width != Width::W64 {
            self.lane_shift(scalar);
            self.op(Op::I64ShrU);
        }
    }

    fn masked_payload(&mut self, scalar: Scalar, width: Width) {
        self.lane_shift(scalar);
        self.op(Op::LocalSet(scalar.shift))
            .op(Op::I64Const(width.mask() as i64))
            .op(Op::LocalGet(scalar.shift))
            .op(Op::I64Shl)
            .op(Op::I64Const(-1))
            .op(Op::I64Xor)
            .op(Op::LocalSet(scalar.mask))
            .op(Op::LocalGet(RESULT));
        self.narrow(width);
        self.op(Op::LocalGet(scalar.shift))
            .op(Op::I64Shl)
            .op(Op::LocalSet(scalar.payload));
    }

    fn lane_cas(&mut self, scalar: Scalar, width: Width) {
        self.lane_address(scalar);
        self.op(Op::LocalGet(scalar.old));
        if width == Width::W64 {
            self.op(Op::LocalGet(RESULT));
        } else {
            self.op(Op::LocalGet(scalar.old))
                .op(Op::LocalGet(scalar.mask))
                .op(Op::I64And)
                .op(Op::LocalGet(scalar.payload))
                .op(Op::I64Or);
        }
        self.op(Op::I64AtomicRmwCmpxchg(LANE));
    }

    pub(in crate::dbt::wasm) fn parallel_store(&mut self, width: Width, exit_depth: u32) -> bool {
        let Some(scalar) = self.parallel_scalar else {
            return false;
        };
        self.stores = true;
        let retry = self.parallel_retry;
        if retry.is_none() {
            if let Some(span) = self.parallel_byte_store_span
                && ByteStoreSpan::eligible(self.current_operation, self.context.is_long64())
            {
                debug_assert_eq!(width, Width::W8);
                self.byte_store_address(scalar, span);
            } else {
                self.shared_address(scalar, width, 1);
            }
        }
        self.op(Op::LocalGet(scalar.address))
            .op(Op::If(BlockType::Empty))
            .op(Op::I32Const(1))
            .op(Op::LocalSet(STORE_STATUS));
        if width == Width::W64 && retry.is_none() {
            self.lane_address(scalar);
            self.op(Op::LocalGet(RESULT)).op(Op::I64AtomicStore(LANE));
        } else {
            if width != Width::W64 {
                self.masked_payload(scalar, width);
            }
            if let Some(retry_depth) = retry {
                self.lane_cas(scalar, width);
                self.op(Op::LocalGet(scalar.old))
                    .op(Op::I64Ne)
                    .op(Op::If(BlockType::Empty))
                    .op(Op::LocalGet(scalar.attempts))
                    .op(Op::I32Const(1))
                    .op(Op::I32Sub)
                    .op(Op::LocalSet(scalar.attempts))
                    .op(Op::Br(exit_depth - retry_depth + 2))
                    .op(Op::End);
            } else {
                self.masked_store_retry(scalar, width, exit_depth + 1);
            }
        }
        self.op(Op::Else);
        self.store_result_slow(width, exit_depth + 1);
        self.op(Op::End);
        true
    }

    fn byte_store_address(&mut self, scalar: Scalar, span: ByteStoreSpan) {
        // Source watches and stripe promotion require a closed phase. If an
        // earlier helper parked this invocation, the instruction's phase poll
        // exits before the cache can be reused. Filling here never parks.
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::LocalGet(span.base))
            .op(Op::I64Sub)
            .op(Op::LocalTee(scalar.old))
            .op(Op::LocalGet(span.limit))
            .op(Op::I64GeU)
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::Call(span.resolve))
            .op(Op::LocalTee(scalar.payload))
            .op(Op::I64Eqz)
            .op(Op::If(BlockType::Empty))
            .op(Op::I32Const(0))
            .op(Op::Br(2))
            .op(Op::End)
            .op(Op::LocalGet(ADDRESS))
            .op(Op::LocalSet(span.base))
            .op(Op::LocalGet(scalar.payload))
            .op(Op::I64Const(32))
            .op(Op::I64ShrU)
            .op(Op::LocalSet(span.limit))
            .op(Op::LocalGet(scalar.payload))
            .op(Op::I32WrapI64)
            .op(Op::LocalSet(span.host))
            .op(Op::I64Const(0))
            .op(Op::LocalSet(scalar.old))
            .op(Op::End)
            .op(Op::LocalGet(span.host))
            .op(Op::LocalGet(scalar.old))
            .op(Op::I32WrapI64)
            .op(Op::I32Add)
            .op(Op::End)
            .op(Op::LocalSet(scalar.address));
    }

    fn masked_store_retry(&mut self, scalar: Scalar, width: Width, exit_depth: u32) {
        self.lane_address(scalar);
        self.op(Op::I64AtomicLoad(LANE))
            .op(Op::LocalSet(scalar.old))
            .op(Op::I32Const(ATTEMPTS))
            .op(Op::LocalSet(scalar.attempts))
            .op(Op::Block(BlockType::Empty))
            .op(Op::Loop(BlockType::Empty));
        self.lane_cas(scalar, width);
        self.op(Op::LocalTee(scalar.shift))
            .op(Op::LocalGet(scalar.old))
            .op(Op::I64Eq)
            .op(Op::BrIf(1))
            .op(Op::LocalGet(scalar.shift))
            .op(Op::LocalSet(scalar.old))
            .op(Op::LocalGet(scalar.attempts))
            .op(Op::I32Const(1))
            .op(Op::I32Sub)
            .op(Op::LocalTee(scalar.attempts))
            .op(Op::BrIf(0))
            .op(Op::End);
        self.store_result_slow(width, exit_depth + 1);
        self.op(Op::End);
    }
}
