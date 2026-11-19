use super::{ADDRESS, Emitter, LHS, RESULT, RHS, STORE_STATUS, VALUE};
use crate::x86::instruction::{Address, Alu, Instruction, MemoryAluOperands, Operation, Width};
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

const BOUNDARY: i32 = 29;
const ATTEMPTS: i32 = crate::memory::SHARED_CAS_ATTEMPTS as i32;
const LANE: MemArg = MemArg {
    offset: 0,
    align: 3,
    memory_index: 1,
};

#[derive(Clone, Copy)]
pub(super) struct Atomic {
    enter: u32,
    leave: u32,
    guard: u32,
    unguard: u32,
    token: u32,
    guarded: u32,
}

impl Atomic {
    pub(super) fn new(first: u32, token: u32) -> Self {
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
pub(super) struct Scalar {
    resolve: u32,
    address: u32,
    attempts: u32,
    old: u32,
    shift: u32,
    mask: u32,
    payload: u32,
}

impl Scalar {
    pub(super) fn new(resolve: u32, first: u32) -> Self {
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

impl Emitter {
    pub(super) fn begin_parallel_instruction(
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

    pub(super) fn end_parallel_instruction(
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

    pub(super) fn enter_parallel_atomic(&mut self) {
        let atomic = self.parallel_atomic.expect("locked instruction imports");
        self.op(Op::Call(atomic.enter))
            .op(Op::LocalSet(atomic.token));
    }

    pub(super) fn enter_parallel_guarded(&mut self, width: u32) {
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

    pub(super) fn leave_parallel_atomic_if_held(&mut self) {
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

    pub(super) fn parallel_boundary(&mut self, exit_depth: u32) {
        self.parallel_boundary_with_flags(exit_depth, false);
    }

    pub(super) fn parallel_boundary_with_flags(&mut self, exit_depth: u32, compare: bool) {
        self.parallel_signal();
        self.op(Op::If(BlockType::Empty));
        if compare {
            self.restore_repeat_flags();
        }
        self.exit(BOUNDARY, exit_depth + 1);
        self.op(Op::End);
    }

    fn shared_address(&mut self, scalar: Scalar, width: Width, access: i32) {
        let memo = if access != 2 && self.parallel_retry.is_none() {
            self.parallel_address_memos.pop()
        } else {
            None
        };
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
                .op(Op::I32And);
            self.parallel_signal();
            self.op(Op::I32Eqz)
                .op(Op::I32And)
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
        self.op(Op::GlobalGet(0)).op(Op::I32AtomicLoad(MemArg {
            offset: 0,
            align: 2,
            memory_index: 1,
        }));
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
    }

    fn lane_shift(&mut self, scalar: Scalar) {
        self.op(Op::LocalGet(scalar.address))
            .op(Op::I32Const(7))
            .op(Op::I32And)
            .op(Op::I32Const(3))
            .op(Op::I32Shl)
            .op(Op::I64ExtendI32U);
    }

    pub(super) fn native_locked_integer(
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
        self.shared_address(scalar, width, 2);
        self.op(Op::LocalGet(scalar.address))
            .op(Op::If(BlockType::Empty));
        self.data_operand(source, width);
        self.op(Op::LocalSet(RHS))
            .op(Op::LocalGet(scalar.address))
            .op(Op::LocalGet(RHS))
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

    pub(super) fn parallel_load(&mut self, width: Width, for_write: bool, exit_depth: u32) -> bool {
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
        self.lane_address(scalar);
        self.op(Op::I64AtomicLoad(LANE));
        if locked {
            self.op(Op::LocalTee(scalar.old));
        }
        if width != Width::W64 {
            self.lane_shift(scalar);
            self.op(Op::I64ShrU);
            self.narrow(width);
        }
        self.op(Op::LocalSet(VALUE)).op(Op::Else);
        if locked {
            self.enter_parallel_guarded(u32::from(width.bytes()));
        }
        self.load_address_slow(width, for_write, exit_depth + 1);
        self.op(Op::End);
        true
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

    pub(super) fn parallel_store(&mut self, width: Width, exit_depth: u32) -> bool {
        let Some(scalar) = self.parallel_scalar else {
            return false;
        };
        self.stores = true;
        let retry = self.parallel_retry;
        if retry.is_none() {
            self.shared_address(scalar, width, 1);
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
