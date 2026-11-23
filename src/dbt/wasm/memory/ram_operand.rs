use super::Emitter;
use wasm_encoder::{Instruction as Op, MemArg, ValType};

impl Emitter {
    #[cfg(guest_memory64)]
    fn ram_window_address(&mut self, argument: &mut MemArg) {
        use crate::memory::guest_windows::{WINDOW_SHIFT, biases_pointer};
        argument.memory_index = 1;
        if crate::memory::guest_windows::use_direct_offsets() {
            self.op(Op::I64ExtendI32U);
            return;
        }
        let token = self.ram_operand_locals;
        self.op(Op::LocalTee(token))
            .op(Op::I32Const(WINDOW_SHIFT as i32))
            .op(Op::I32ShrU)
            .op(Op::I32Const(3))
            .op(Op::I32Shl)
            .op(Op::I64Load(MemArg {
                offset: u64::from(biases_pointer()),
                align: 3,
                memory_index: 0,
            }))
            .op(Op::LocalGet(token))
            .op(Op::I64ExtendI32U)
            .op(Op::I64Add);
    }

    pub(in crate::dbt::wasm) fn ram_load(
        &mut self,
        instruction: fn(MemArg) -> Op<'static>,
        argument: MemArg,
    ) -> &mut Self {
        #[cfg(guest_memory64)]
        let mut argument = argument;
        #[cfg(guest_memory64)]
        if argument.memory_index == 0 {
            self.ram_window_address(&mut argument);
        }
        self.op(instruction(argument))
    }

    pub(in crate::dbt::wasm) fn ram_store(
        &mut self,
        instruction: fn(MemArg) -> Op<'static>,
        argument: MemArg,
        _value: ValType,
    ) -> &mut Self {
        #[cfg(guest_memory64)]
        let mut argument = argument;
        #[cfg(guest_memory64)]
        if argument.memory_index == 0 {
            let value = self.ram_operand_locals
                + match _value {
                    ValType::I32 => 1,
                    ValType::I64 => 2,
                    ValType::V128 => 3,
                    _ => unreachable!(),
                };
            self.op(Op::LocalSet(value));
            self.ram_window_address(&mut argument);
            self.op(Op::LocalGet(value));
        }
        self.op(instruction(argument))
    }
}
