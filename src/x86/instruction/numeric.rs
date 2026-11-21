use super::{Address, Location, Width};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorOperand {
    Register(u8),
    Memory(Address),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorBinary {
    And,
    AndNot,
    Or,
    Xor,
    Add(Width),
    Sub(Width),
    SignedSaturatingAdd(Width),
    UnsignedSaturatingAdd(Width),
    SignedSaturatingSub(Width),
    UnsignedSaturatingSub(Width),
    CompareEqual(Width),
    CompareGreater(Width),
    MinUnsignedByte,
    MaxUnsignedByte,
    MinSignedWord,
    MaxSignedWord,
    Average(Width),
    MulLowWord,
    MulHighWord { signed: bool },
    MulUnsignedDword,
    MaddSignedWord,
    SumAbsoluteByteDifferences,
    PackSigned(Width),
    PackUnsignedWord,
    UnpackLow(Width),
    UnpackHigh(Width),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorShuffle {
    Dwords,
    LowWords,
    HighWords,
    RawSingles,
    RawDoubles,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorDuplicate {
    LowDouble,
    EvenSingles,
    OddSingles,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorShift {
    Left(Width),
    Right(Width),
    ArithmeticRight(Width),
    ByteLeft,
    ByteRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorCount {
    Immediate(u8),
    Vector(VectorOperand),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorMask {
    Bytes,
    Singles,
    Doubles,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Ssse3Op {
    Pshufb,
    Phaddw,
    Phaddd,
    Phaddsw,
    Pmaddubsw,
    Phsubw,
    Phsubd,
    Phsubsw,
    Psignb,
    Psignw,
    Psignd,
    Pmulhrsw,
    Pabsb,
    Pabsw,
    Pabsd,
    Palignr,
}

impl Ssse3Op {
    pub const ALL: [Self; 16] = [
        Self::Pshufb,
        Self::Phaddw,
        Self::Phaddd,
        Self::Phaddsw,
        Self::Pmaddubsw,
        Self::Phsubw,
        Self::Phsubd,
        Self::Phsubsw,
        Self::Psignb,
        Self::Psignw,
        Self::Psignd,
        Self::Pmulhrsw,
        Self::Pabsb,
        Self::Pabsw,
        Self::Pabsd,
        Self::Palignr,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorInstruction {
    HalfMove {
        register: u8,
        operand: VectorOperand,
        high: bool,
        store: bool,
    },
    Move {
        register: u8,
        operand: VectorOperand,
        bytes: u8,
        preserve_upper: bool,
        aligned: bool,
        store: bool,
    },
    ToXmm {
        dst: u8,
        src: Location,
        width: Width,
    },
    FromXmm {
        dst: Location,
        src: u8,
        width: Width,
    },
    Binary {
        kind: VectorBinary,
        dst: u8,
        src: VectorOperand,
    },
    Ssse3 {
        op: Ssse3Op,
        dst: u8,
        src: VectorOperand,
        immediate: u8,
    },
    Shuffle {
        kind: VectorShuffle,
        dst: u8,
        src: VectorOperand,
        immediate: u8,
    },
    Shift {
        kind: VectorShift,
        dst: u8,
        count: VectorCount,
    },
    InsertWord {
        dst: u8,
        src: Location,
        immediate: u8,
    },
    ExtractWord {
        dst: u8,
        src: u8,
        immediate: u8,
    },
    Mask {
        kind: VectorMask,
        dst: u8,
        src: u8,
    },
    Duplicate {
        kind: VectorDuplicate,
        dst: u8,
        src: VectorOperand,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloatInstruction {
    pub op: crate::x86::float::Op,
    pub dst: u8,
    pub src: Location,
}

impl FloatInstruction {
    pub fn source_is_gpr(self) -> bool {
        self.op.scalar && self.op.kind == crate::x86::float::Kind::IntToFloat
    }
    pub fn destination_is_gpr(self) -> bool {
        self.op.scalar
            && matches!(
                self.op.kind,
                crate::x86::float::Kind::FloatToInt | crate::x86::float::Kind::TruncToInt
            )
    }
    pub fn writes_flags(self) -> bool {
        matches!(
            self.op.kind,
            crate::x86::float::Kind::Comi | crate::x86::float::Kind::Ucomi
        )
    }
    pub fn source_bytes(self) -> u8 {
        use crate::x86::float::{Format, Kind};
        if self.source_is_gpr() {
            return if self.op.integer64 { 8 } else { 4 };
        }
        if self.op.scalar {
            return if self.op.format == Format::F32 { 4 } else { 8 };
        }
        if self.op.kind == Kind::FloatConvert && self.op.format == Format::F32
            || self.op.kind == Kind::IntToFloat && self.op.format == Format::F64
        {
            8
        } else {
            16
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct X87Instruction {
    pub op: crate::x86::x87::Op,
    pub address: Option<Address>,
}
