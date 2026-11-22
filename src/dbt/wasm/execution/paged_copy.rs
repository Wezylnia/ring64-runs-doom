use super::{ADDRESS, Emitter, LHS, REMAINING, RHS, RIP, STORE_STATUS, VALUE};
use crate::x86::context::Segment;
use crate::x86::instruction::{
    Address, AddressBase, Instruction, Operation, VectorInstruction, VectorOperand, Width,
};
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

#[derive(Clone, Copy)]
pub(super) struct CopyGroup {
    source: Address,
    destination: Address,
    register: u8,
    pub(super) width: u32,
    end: u64,
}

impl CopyGroup {
    pub(super) fn recognize(lap: &[Instruction]) -> Option<Self> {
        fn vector_move(instruction: Instruction, writing: bool) -> Option<(Address, u8)> {
            let Operation::Vector(VectorInstruction::Move {
                operand: VectorOperand::Memory(address),
                register,
                bytes: 16,
                aligned: false,
                store,
                ..
            }) = instruction.operation
            else {
                return None;
            };
            (store == writing
                && address.segment == Segment::Ds
                && address.size == Width::W64
                && address.base != AddressBase::NextPc)
                .then_some((address, register))
        }

        let (source, register) = vector_move(*lap.first()?, false)?;
        let (destination, target) = vector_move(*lap.get(1)?, true)?;
        if target != register {
            return None;
        }
        let consecutive = |base: Address, address: Address, offset: i64| {
            base.displacement
                .checked_add(offset)
                .is_some_and(|displacement| {
                    address
                        == Address {
                            displacement,
                            ..base
                        }
                })
        };
        let mut pairs = 1;
        for pair in lap[2..].chunks_exact(2).take(3) {
            let offset = i64::from(pairs) * 16;
            if !vector_move(pair[0], false).is_some_and(|(address, bank)| {
                bank == register && consecutive(source, address, offset)
            }) || !vector_move(pair[1], true).is_some_and(|(address, bank)| {
                bank == register && consecutive(destination, address, offset)
            }) {
                break;
            }
            pairs += 1;
        }
        let instructions = (pairs * 2) as usize;
        if pairs < 2
            || lap[instructions..]
                .iter()
                .any(|i| matches!(i.operation, Operation::Vector(_)))
        {
            return None;
        }
        Some(Self {
            source,
            destination,
            register,
            width: pairs * 16,
            end: lap[instructions - 1].next_pc,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct CopyWindow {
    pub(super) group: CopyGroup,
    pub(super) resolve: u32,
    pub(super) source_base: u32,
    pub(super) destination_base: u32,
    pub(super) limit: u32,
    pub(super) source_host: u32,
    pub(super) destination_host: u32,
}

impl Emitter {
    pub(super) fn emit_paged_copy_group(&mut self, window: CopyWindow, exit_depth: u32) {
        fn memory(offset: u64) -> MemArg {
            MemArg {
                offset,
                align: 0,
                memory_index: 0,
            }
        }

        let group = window.group;
        self.address(group.source, 0);
        self.op(Op::LocalTee(LHS))
            .op(Op::LocalGet(window.source_base))
            .op(Op::I64Sub)
            .op(Op::LocalTee(VALUE))
            .op(Op::LocalGet(window.limit))
            .op(Op::I64LtU);
        self.address(group.destination, 0);
        self.op(Op::LocalTee(RHS))
            .op(Op::LocalGet(window.destination_base))
            .op(Op::I64Sub)
            .op(Op::LocalTee(ADDRESS))
            .op(Op::LocalGet(window.limit))
            .op(Op::I64LtU)
            .op(Op::I32And)
            .op(Op::I32Eqz)
            .op(Op::If(BlockType::Empty))
            .op(Op::LocalGet(LHS))
            .op(Op::LocalGet(RHS))
            .op(Op::I32Const(group.width as i32))
            .op(Op::Call(window.resolve))
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I32Eqz);
        self.retry_guarded_paged_operand(exit_depth + 1, true);
        self.op(Op::LocalGet(LHS))
            .op(Op::LocalSet(window.source_base))
            .op(Op::LocalGet(RHS))
            .op(Op::LocalSet(window.destination_base))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(memory(0)))
            .op(Op::LocalSet(window.source_host))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(memory(4)))
            .op(Op::LocalSet(window.destination_host))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I32Load(memory(8)))
            .op(Op::I64ExtendI32U)
            .op(Op::LocalSet(window.limit))
            .op(Op::I64Const(0))
            .op(Op::LocalSet(VALUE))
            .op(Op::I64Const(0))
            .op(Op::LocalSet(ADDRESS))
            .op(Op::End);

        self.op(Op::LocalGet(VALUE))
            .op(Op::I32WrapI64)
            .op(Op::LocalGet(window.source_host))
            .op(Op::I32Add)
            .op(Op::LocalSet(STORE_STATUS))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::LocalGet(window.destination_host))
            .op(Op::I32Add)
            .op(Op::I64ExtendI32U)
            .op(Op::LocalSet(ADDRESS));
        for offset in (0..group.width).step_by(16) {
            // A dependent later load sees the preceding store even on aliased physical backing.
            self.op(Op::LocalGet(ADDRESS))
                .op(Op::I32WrapI64)
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::V128Load(memory(u64::from(offset))));
            if offset + 16 == group.width {
                self.op(Op::LocalTee(self.xmm(group.register)));
            }
            self.op(Op::V128Store(memory(u64::from(offset))));
        }
        self.op(Op::I64Const(group.end as i64))
            .op(Op::LocalSet(RIP))
            .op(Op::LocalGet(REMAINING))
            .op(Op::I64Const(i64::from(group.width / 8)))
            .op(Op::I64Sub)
            .op(Op::LocalSet(REMAINING));
    }
}
