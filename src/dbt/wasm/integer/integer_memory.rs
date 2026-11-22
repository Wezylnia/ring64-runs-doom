use super::{Emitter, LHS, RESULT, RHS, VALUE};
use crate::x86::instruction::{Address, Alu, MemoryAluOperands, Width};
use wasm_encoder::Instruction as Op;

impl Emitter {
    pub(super) fn memory_integer(
        &mut self,
        kind: Alu,
        address: Address,
        operands: MemoryAluOperands,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        let parallel = self.native_locked_integer(kind, address, operands, width, next_pc);
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        let exit_depth = exit_depth + u32::from(parallel);
        let writes_memory =
            matches!(operands, MemoryAluOperands::MemoryFirst(_)) && kind.writes_result();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let cached = (writes_memory && self.linear_call_spans)
            .then(|| self.load_cached_rmw(address, width, next_pc, exit_depth))
            .flatten();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let ordinary = cached.is_none();
        #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
        let ordinary = true;
        if ordinary {
            self.load_value(address, width, next_pc, writes_memory, exit_depth);
        }
        match operands {
            MemoryAluOperands::MemoryFirst(source) => {
                self.op(Op::LocalGet(VALUE));
                self.narrow(width);
                self.op(Op::LocalSet(LHS));
                self.data_operand(source, width);
                self.op(Op::LocalSet(RHS));
            }
            MemoryAluOperands::RegisterFirst(register) => {
                self.data_register(register, width);
                self.op(Op::LocalSet(LHS)).op(Op::LocalGet(VALUE));
                self.narrow(width);
                self.op(Op::LocalSet(RHS));
            }
        }
        if kind == Alu::Cmp && self.omit_result_status {
            return;
        }
        match kind {
            Alu::Add | Alu::Sub | Alu::Cmp => self.arithmetic_result(kind, width),
            Alu::Adc | Alu::Sbb => self.carry_result(kind, width),
            Alu::And | Alu::Or | Alu::Xor | Alu::Test => {
                self.op(Op::LocalGet(LHS)).op(Op::LocalGet(RHS));
                self.logical_result(kind);
            }
        }
        if writes_memory {
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            if let Some(proof) = cached {
                self.store_cached_rmw(proof, width, exit_depth);
            }
            if ordinary {
                self.store_result(width, exit_depth);
            }
        }
        // A failed store exits above with the incoming flags still live.
        match kind {
            Alu::Add | Alu::Sub | Alu::Cmp => self.arithmetic_flags(kind, width),
            Alu::Adc | Alu::Sbb => self.carry_flags(kind, width),
            Alu::And | Alu::Or | Alu::Xor | Alu::Test => self.logical_flags(width),
        }
        if let MemoryAluOperands::RegisterFirst(register) = operands
            && kind.writes_result()
        {
            self.op(Op::LocalGet(RESULT));
            self.write_register(register, width);
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if parallel {
            self.op(Op::End);
        }
    }
}
