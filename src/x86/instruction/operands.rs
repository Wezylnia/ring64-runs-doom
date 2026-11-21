use crate::x86::context::Segment;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    W8,
    W16,
    W32,
    W64,
}

impl Width {
    pub fn bits(self) -> u32 {
        match self {
            Self::W8 => 8,
            Self::W16 => 16,
            Self::W32 => 32,
            Self::W64 => 64,
        }
    }

    pub fn bytes(self) -> u8 {
        (self.bits() / 8) as u8
    }

    pub fn mask(self) -> u64 {
        match self {
            Self::W64 => u64::MAX,
            _ => (1u64 << self.bits()) - 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegisterView {
    pub bank: u8,
    pub high_byte: bool,
}

impl RegisterView {
    pub const fn low(bank: u8) -> Self {
        assert!(bank < 16);
        Self {
            bank,
            high_byte: false,
        }
    }

    pub const fn high_byte(bank: u8) -> Self {
        assert!(bank < 4);
        Self {
            bank,
            high_byte: true,
        }
    }

    pub fn shift(self) -> u32 {
        if self.high_byte { 8 } else { 0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Register(u8),
    Immediate(u64),
    Memory(Address),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataOperand {
    Register(RegisterView),
    /// the operand-width bit pattern, zero-extended to u64.
    Immediate(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    Register(RegisterView),
    Memory(Address),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressBase {
    Register(u8),
    /// The containing instruction's completed next_pc.
    NextPc,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressIndex {
    pub register: u8,
    /// The effective-address multiplier: 1, 2, 4 or 8.
    pub scale: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    pub base: AddressBase,
    pub index: Option<AddressIndex>,
    pub displacement: i64,
    pub size: Width,
    pub segment: Segment,
}

impl Default for Address {
    fn default() -> Self {
        Self {
            base: AddressBase::None,
            index: None,
            displacement: 0,
            size: Width::W64,
            segment: Segment::Ds,
        }
    }
}
