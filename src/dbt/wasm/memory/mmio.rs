use super::{ADDRESS, Emitter, REASON, RESULT, STORE_STATUS, VALUE};
use crate::embedding::layout::RunState;
use crate::x86::instruction::{Address, DataOperand, RegisterView, Width};
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

pub(super) enum Move {
    Read(RegisterView),
    Write(DataOperand),
}

impl Emitter {
    pub(super) fn mmio_move(
        &mut self,
        instruction: Move,
        address: Address,
        width: Width,
        next_pc: u64,
        exit_depth: u32,
    ) {
        let descriptor = (u32::from(width.bytes()) << 2)
            | (u32::from(self.observed) << 1)
            | u32::from(matches!(instruction, Move::Write(_)));
        self.address(address, next_pc);
        self.op(Op::LocalSet(ADDRESS));
        if let Move::Write(source) = instruction {
            self.data_operand(source, width);
            self.op(Op::LocalSet(RESULT));
        }

        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let direct = {
            use super::memory::DirectOperand;
            // Span resolvers only cover ordinary RAM mappings. A device or a
            // partial overlap uses the reached operand helper below.
            let operand = if matches!(instruction, Move::Read(_)) {
                self.op(Op::I32Const(1)).op(Op::LocalSet(STORE_STATUS));
                DirectOperand::Load(None)
            } else {
                DirectOperand::Store
            };
            self.direct_access(width, operand, exit_depth, |emitter, depth| {
                emitter.mmio_effect(descriptor, depth)
            })
        };
        #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
        let direct = false;
        if !direct {
            self.mmio_effect(descriptor, exit_depth);
        }
        if let Move::Read(register) = instruction {
            self.op(Op::LocalGet(VALUE));
            self.narrow(width);
            self.write_register(register, width);
        }
        // Paging metadata may invalidate the current translation on a read too.
        self.stores = true;
    }

    fn mmio_effect(&mut self, descriptor: u32, exit_depth: u32) {
        let read = descriptor & 1 == 0;
        let helper = self.mmio_operand.expect("MMIO operand import");
        let segment = self.segment as i32;
        self.op(Op::I32Const(descriptor as i32))
            .op(Op::LocalGet(ADDRESS))
            .op(if read {
                Op::I64Const(0)
            } else {
                Op::LocalGet(RESULT)
            })
            .op(Op::I32Const(segment))
            .op(Op::Call(helper))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Const(2))
            .op(Op::I32GtU)
            .unlikely(Op::If(BlockType::Empty));
        self.op(Op::LocalGet(STORE_STATUS))
            .op(Op::LocalSet(REASON))
            .op(Op::Br(exit_depth + 1))
            .op(Op::End);
        if read {
            self.op(Op::LocalGet(0))
                .op(Op::I64Load(MemArg {
                    offset: std::mem::offset_of!(RunState, vector_scratch) as u64,
                    align: 0,
                    memory_index: 0,
                }))
                .op(Op::LocalSet(VALUE));
        }
    }
}
