use super::{ADDRESS, Emitter, FLAGS, GPR, LHS, RESULT, RHS, STORE_STATUS, VALUE, vector::memory};
use crate::x86::instruction::{Address, Alu, Location, RegisterView, Width};
use wasm_encoder::{BlockType, Instruction as Op};

impl Emitter {
    pub(super) fn exchange(
        &mut self,
        dst: Location,
        src: RegisterView,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        self.read_location(dst, width, next_pc, true, exit_depth);
        self.data_register(src, width);
        self.op(Op::LocalSet(RESULT));
        self.write_location(dst, width, exit_depth);
        self.op(Op::LocalGet(LHS));
        self.write_register(src, width);
    }

    pub(super) fn xadd(
        &mut self,
        dst: Location,
        src: RegisterView,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        self.read_location(dst, width, next_pc, true, exit_depth);
        self.data_register(src, width);
        self.op(Op::LocalSet(RHS));
        self.arithmetic_result(Alu::Add, width);
        if matches!(dst, Location::Memory(_)) {
            self.store_result(width, exit_depth);
        }
        self.op(Op::LocalGet(LHS));
        self.write_register(src, width);
        if let Location::Register(register) = dst {
            self.op(Op::LocalGet(RESULT));
            self.write_register(register, width);
        }
        self.arithmetic_flags(Alu::Add, width);
    }

    pub(super) fn compare_exchange(
        &mut self,
        dst: Location,
        src: RegisterView,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        self.read_location(dst, width, next_pc, true, exit_depth);
        self.op(Op::LocalGet(LHS)).op(Op::LocalSet(RHS));
        self.register(0, width);
        self.op(Op::LocalSet(LHS));
        self.data_register(src, width);
        self.op(Op::LocalSet(VALUE))
            .op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::I64Eq)
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(VALUE))
            .op(Op::LocalSet(RESULT));
        self.write_location(dst, width, exit_depth + 1);
        self.op(Op::Else);
        if matches!(dst, Location::Memory(_)) {
            self.op(Op::LocalGet(RHS)).op(Op::LocalSet(RESULT));
            self.store_result(width, exit_depth + 1);
        }
        self.op(Op::LocalGet(RHS));
        self.write_register(RegisterView::low(0), width);
        self.op(Op::End);
        self.arithmetic_result(Alu::Cmp, width);
        self.arithmetic_flags(Alu::Cmp, width);
    }

    fn pair32(&mut self, high: u8, low: u8) {
        self.register(high, Width::W32);
        self.op(Op::I64Const(32)).op(Op::I64Shl);
        self.register(low, Width::W32);
        self.op(Op::I64Or);
    }

    pub(super) fn compare_exchange8b(&mut self, address: Address, next_pc: u64, exit_depth: u32) {
        self.read_location(
            Location::Memory(address),
            Width::W64,
            next_pc,
            true,
            exit_depth,
        );
        self.op(Op::LocalGet(LHS)).op(Op::LocalSet(RHS));
        self.pair32(2, 0);
        self.op(Op::LocalSet(LHS));
        self.pair32(1, 3);
        self.op(Op::LocalSet(RESULT))
            .op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::I64Eq)
            .op(Op::If(BlockType::Empty));
        self.store_result(Width::W64, exit_depth + 1);
        self.op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(0x40))
            .op(Op::I64Or)
            .op(Op::LocalSet(FLAGS))
            .op(Op::Else)
            .op(Op::LocalGet(RHS))
            .op(Op::LocalSet(RESULT));
        self.store_result(Width::W64, exit_depth + 1);
        self.op(Op::LocalGet(RHS));
        self.narrow(Width::W32);
        self.write_register(RegisterView::low(0), Width::W32);
        self.op(Op::LocalGet(RHS))
            .op(Op::I64Const(32))
            .op(Op::I64ShrU);
        self.write_register(RegisterView::low(2), Width::W32);
        self.op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(!0x40))
            .op(Op::I64And)
            .op(Op::LocalSet(FLAGS))
            .op(Op::End);
    }

    pub(super) fn compare_exchange16b(&mut self, address: Address, next_pc: u64, exit_depth: u32) {
        let read = self
            .block16_for_write
            .expect("16-byte read-for-write import");
        let store = self.vector_store.expect("16-byte store import");
        self.stores = true;
        self.address(address, next_pc);
        self.op(Op::LocalTee(ADDRESS));
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if self.parallel_scalar.is_some() {
            self.enter_parallel_guarded(16);
        }
        self.op(Op::I32Const(address.segment as i32))
            .op(Op::Call(read))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.vector_failure(exit_depth + 1);

        // Keep both old halves across the mandatory store. A miss writes the
        // unchanged scratch, even when publishing it faults or invalidates code.
        self.op(Op::End)
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load(memory(0)))
            .op(Op::LocalTee(LHS))
            .op(Op::LocalGet(GPR))
            .op(Op::I64Eq)
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load(memory(8)))
            .op(Op::LocalTee(RHS))
            .op(Op::LocalGet(GPR + 2))
            .op(Op::I64Eq)
            .op(Op::I32And)
            .op(Op::If(BlockType::Result(wasm_encoder::ValType::I64)))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::LocalGet(GPR + 3))
            .op(Op::I64Store(memory(0)))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::LocalGet(GPR + 1))
            .op(Op::I64Store(memory(8)))
            .op(Op::I64Const(0x40))
            .op(Op::Else)
            .op(Op::I64Const(0))
            .op(Op::End)
            .op(Op::LocalSet(RESULT))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32Const(16))
            .op(Op::I32Const(address.segment as i32))
            .op(Op::I32Const(16))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::Call(store))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty));
        self.vector_failure(exit_depth + 1);
        self.op(Op::End)
            .op(Op::LocalGet(RESULT))
            .op(Op::I64Eqz)
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(LHS))
            .op(Op::LocalSet(GPR))
            .op(Op::LocalGet(RHS))
            .op(Op::LocalSet(GPR + 2))
            .op(Op::End)
            .op(Op::LocalGet(FLAGS))
            .op(Op::I64Const(!0x40))
            .op(Op::I64And)
            .op(Op::LocalGet(RESULT))
            .op(Op::I64Or)
            .op(Op::LocalSet(FLAGS));
    }
}
