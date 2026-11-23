use super::{ADDRESS, DirectOperand, Emitter, VALUE};
use crate::dbt::wasm::{GUEST_MEMORY, PRIVATE_MEMORY};
use crate::memory::GuestSpan;
use wasm_encoder::{BlockType, Instruction as Op, MemArg};

// A base, length and RAM offset for each of stack/data and read/write.
pub(in crate::dbt::wasm) const SPAN_WORDS: u32 = 12;

#[derive(Clone, Copy)]
pub(in crate::dbt::wasm) struct SpanLocals {
    pub(in crate::dbt::wasm) first: u32,
    pub(in crate::dbt::wasm) resolve: u32,
}

impl Emitter {
    // Read and write proofs last for one artifact invocation. A zero length misses,
    // including on the first access. Failed proofs run the ordinary fault path.
    pub(super) fn guest64_access(
        &mut self,
        operand: DirectOperand,
        bytes: u32,
        store: bool,
        cache: SpanLocals,
        exit_depth: u32,
        slow: impl FnOnce(&mut Self, u32),
    ) {
        let stack = self.segment == crate::x86::context::Segment::Ss;
        let base = cache.first + 6 * u32::from(stack) + 3 * u32::from(store);
        let length = base + 1;
        let host = base + 2;
        let descriptor = cache.first + SPAN_WORDS;
        match operand {
            DirectOperand::VectorDword(_) | DirectOperand::VectorBlock(_) => {
                self.op(Op::LocalSet(ADDRESS));
            }
            DirectOperand::VectorBlockStore {
                address, next_pc, ..
            } => {
                self.address(address, next_pc);
                self.op(Op::LocalSet(ADDRESS));
            }
            _ => {}
        }
        self.op(Op::Block(BlockType::Empty));
        self.linear_address();
        self.op(Op::LocalGet(base))
            .op(Op::I64Sub)
            .op(Op::LocalTee(VALUE))
            .op(Op::LocalGet(length))
            .op(Op::I64GeU);
        if bytes > 1 {
            self.op(Op::LocalGet(VALUE))
                .op(Op::I64Const(i64::from(bytes - 1)))
                .op(Op::I64Add)
                .op(Op::LocalGet(length))
                .op(Op::I64GeU)
                .op(Op::I32Or);
        }
        self.unlikely(Op::If(BlockType::Empty));
        self.linear_address();
        self.op(Op::I32Const(bytes as i32))
            .op(Op::I32Const(i32::from(store)))
            .op(Op::Call(cache.resolve))
            .op(Op::LocalTee(descriptor))
            .op(Op::I32Eqz)
            .unlikely(Op::If(BlockType::Empty));
        slow(self, exit_depth + 3);
        self.op(Op::Br(2)).op(Op::End);
        for (local, offset) in [
            (base, std::mem::offset_of!(GuestSpan, base)),
            (length, std::mem::offset_of!(GuestSpan, length)),
            (host, std::mem::offset_of!(GuestSpan, host)),
        ] {
            self.op(Op::LocalGet(descriptor))
                .op(Op::I64Load(MemArg {
                    offset: offset as u64,
                    align: 3,
                    memory_index: PRIVATE_MEMORY,
                }))
                .op(Op::LocalSet(local));
        }
        self.linear_address();
        self.op(Op::LocalGet(base))
            .op(Op::I64Sub)
            .op(Op::LocalSet(VALUE))
            .op(Op::End)
            .op(Op::LocalGet(host))
            .op(Op::LocalGet(VALUE))
            .op(Op::I64Add);
        self.direct_operand(
            operand,
            MemArg {
                offset: 0,
                align: 0,
                memory_index: GUEST_MEMORY,
            },
        );
        self.op(Op::End);
    }
}
