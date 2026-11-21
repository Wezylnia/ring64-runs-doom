use super::context::Segment;

mod integer;
mod numeric;
mod operands;
mod system;

pub use integer::{
    Alu, Bit, BitIndex, Bmi, Carry, Condition, Count, CountCondition, DoubleShift,
    MemoryAluOperands, Repeat, Shift, StringKind, Unary,
};
pub use numeric::{
    FloatInstruction, Ssse3Op, VectorBinary, VectorCount, VectorDuplicate, VectorInstruction,
    VectorMask, VectorOperand, VectorShift, VectorShuffle, X87Instruction,
};
pub use operands::{
    Address, AddressBase, AddressIndex, DataOperand, Location, Operand, RegisterView, Width,
};
pub use system::{
    FarDescriptor, FarInstruction, FarKind, FarOperand, PagingInstruction, ProtectionDescriptor,
    ProtectionInstruction, ProtectionKeyInstruction, ProtectionKind, SoftwareEventDescriptor,
    SoftwareEventKind, SystemFlagsDescriptor, SystemFlagsKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Halt,
    Processor(super::processor::Instruction),
    ProtectionKey(ProtectionKeyInstruction),
    Paging(PagingInstruction),
    FastSystem(crate::x86::fast_system::Instruction),
    SegmentBase {
        segment: Segment,
        register: u8,
        width: Width,
        write: bool,
    },
    DebugRegister {
        register: u8,
        gpr: u8,
        write: bool,
        width: Width,
    },
    SystemFlags(SystemFlagsDescriptor),
    SoftwareEvent(SoftwareEventDescriptor),
    PopSs {
        width: Width,
    },
    Far(FarInstruction),
    Protection(ProtectionInstruction),
    MaskedStore {
        data: u8,
        mask: u8,
        mmx: bool,
        address: Address,
    },
    Clflush {
        address: Address,
    },
    Mmx {
        op: super::mmx::Op,
        address: Option<Address>,
    },
    NumericImage {
        op: super::numeric_images::ImageOp,
        address: Address,
    },
    X87(X87Instruction),
    Float(FloatInstruction),
    Mxcsr {
        address: Address,
        store: bool,
    },
    Vector(VectorInstruction),
    Nop,
    Exchange {
        dst: Location,
        src: RegisterView,
        width: Width,
    },
    Xadd {
        dst: Location,
        src: RegisterView,
        width: Width,
    },
    CompareExchange {
        dst: Location,
        src: RegisterView,
        width: Width,
    },
    CompareExchange8b {
        address: Address,
    },
    CompareExchange16b {
        address: Address,
    },
    Direction {
        set: bool,
    },
    Io {
        input: bool,
        port: Option<u8>,
        width: Width,
    },
    String {
        kind: StringKind,
        width: Width,
        address_width: Width,
        source_segment: Segment,
        repeat: Repeat,
    },
    Mov {
        dst: RegisterView,
        src: DataOperand,
        width: Width,
    },
    Lea {
        dst: RegisterView,
        address: Address,
        width: Width,
    },
    Load {
        dst: RegisterView,
        address: Address,
        width: Width,
    },
    Store {
        src: DataOperand,
        address: Address,
        width: Width,
    },
    Alu {
        kind: Alu,
        /// First register operand; Cmp and Test do not write it.
        dst: RegisterView,
        src: DataOperand,
        width: Width,
    },
    MemoryAlu {
        kind: Alu,
        address: Address,
        operands: MemoryAluOperands,
        width: Width,
    },
    Unary {
        kind: Unary,
        dst: Location,
        width: Width,
    },
    Multiply {
        signed: bool,
        src: Location,
        width: Width,
    },
    Imul {
        dst: RegisterView,
        src: Location,
        immediate: Option<u64>,
        width: Width,
    },
    Divide {
        signed: bool,
        src: Location,
        width: Width,
    },
    Carry {
        kind: Carry,
    },
    Lahf,
    Sahf,
    Shift {
        kind: Shift,
        dst: Location,
        count: Count,
        width: Width,
    },
    DoubleShift {
        kind: DoubleShift,
        dst: Location,
        src: RegisterView,
        count: Count,
        width: Width,
    },
    Bit {
        kind: Bit,
        dst: Location,
        index: BitIndex,
        width: Width,
    },
    Scan {
        reverse: bool,
        dst: RegisterView,
        src: Location,
        width: Width,
    },
    Bmi {
        kind: Bmi,
        dst: RegisterView,
        src: Location,
        width: Width,
    },
    Bswap {
        dst: RegisterView,
        width: Width,
    },
    Extend {
        dst: RegisterView,
        src: Location,
        source_width: Width,
        width: Width,
        signed: bool,
    },
    SignHigh {
        width: Width,
    },
    Setcc {
        dst: Location,
        condition: Condition,
    },
    Cmov {
        dst: RegisterView,
        src: Location,
        width: Width,
        condition: Condition,
    },
    Push {
        src: Operand,
        width: Width,
    },
    Pop {
        dst: u8,
        width: Width,
    },
    PopMemory {
        address: Address,
        width: Width,
    },
    Leave {
        width: Width,
    },
    CompoundStack(super::compound_stack::CompoundStack),
    Call {
        /// Absolute guest target, also for a decoded relative CALL.
        target: Operand,
        width: Width,
    },
    JumpRegister {
        target: u8,
        width: Width,
    },
    JumpMemory {
        address: Address,
        width: Width,
    },
    Return {
        cleanup: u16,
        width: Width,
    },
    Jump {
        target: u64,
    },
    Jcc {
        condition: Condition,
        target: u64,
    },
    CountBranch {
        condition: CountCondition,
        count_width: Width,
        target: u64,
    },
    X87CompatibilityNop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub pc: u64,
    pub next_pc: u64,
    pub operation: Operation,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub explicit_lock: bool,
}

impl Instruction {
    pub fn encoded_len(&self) -> u32 {
        // Lengths are at most 15; low32 keeps both EIP32 and RIP64 wrap exact.
        self.next_pc.wrapping_sub(self.pc) as u32
    }

    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(crate) fn shared_atomic(self) -> bool {
        self.explicit_lock
            || matches!(
                self.operation,
                Operation::Exchange {
                    dst: Location::Memory(_),
                    ..
                } | Operation::CompareExchange8b { .. }
                    | Operation::CompareExchange16b { .. }
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Alu, Width};

    #[test]
    fn compare_and_test_have_no_operand_write() {
        for kind in [
            Alu::Add,
            Alu::Adc,
            Alu::Sub,
            Alu::Sbb,
            Alu::And,
            Alu::Or,
            Alu::Xor,
        ] {
            assert!(kind.writes_result(), "{kind:?}");
        }
        for kind in [Alu::Cmp, Alu::Test] {
            assert!(!kind.writes_result(), "{kind:?}");
        }
    }

    #[test]
    fn operand_widths_match_the_selected_data_forms() {
        for (width, bits, bytes, mask) in [
            (Width::W8, 8, 1, 0xff),
            (Width::W16, 16, 2, 0xffff),
            (Width::W32, 32, 4, 0xffff_ffff),
            (Width::W64, 64, 8, u64::MAX),
        ] {
            assert_eq!(
                (width.bits(), width.bytes(), width.mask()),
                (bits, bytes, mask)
            );
        }
    }
}
