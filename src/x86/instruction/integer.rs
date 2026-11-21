use super::{DataOperand, RegisterView};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alu {
    Add,
    Adc,
    Sub,
    Sbb,
    Cmp,
    And,
    Or,
    Xor,
    Test,
}

impl Alu {
    pub fn writes_result(self) -> bool {
        match self {
            Self::Add | Self::Adc | Self::Sub | Self::Sbb | Self::And | Self::Or | Self::Xor => {
                true
            }
            Self::Cmp | Self::Test => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Inc,
    Dec,
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carry {
    Clear,
    Set,
    Complement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringKind {
    Ins,
    Outs,
    Movs,
    Stos,
    Lods,
    Cmps,
    Scas,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    None,
    Count,
    WhileEqual,
    WhileNotEqual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    Immediate(u8),
    Cl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    Rol,
    Ror,
    Rcl,
    Rcr,
    Shl,
    Shr,
    Sar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleShift {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bit {
    Test,
    Set,
    Reset,
    Complement,
}

impl Bit {
    pub fn writes_result(self) -> bool {
        self != Self::Test
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bmi {
    AndNot(RegisterView),
    Extract(RegisterView),
    Isolate,
    Mask,
    Reset,
    TrailingZeros,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitIndex {
    Register(RegisterView),
    Immediate(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryAluOperands {
    MemoryFirst(DataOperand),
    RegisterFirst(RegisterView),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    Overflow,
    NotOverflow,
    Below,
    AboveOrEqual,
    Equal,
    NotEqual,
    BelowOrEqual,
    Above,
    Sign,
    NotSign,
    Parity,
    NotParity,
    Less,
    GreaterOrEqual,
    LessOrEqual,
    Greater,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CountCondition {
    Always,
    Equal,
    NotEqual,
    Zero,
}
