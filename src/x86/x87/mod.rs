mod execute;
mod number;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) use execute::admitted_exchange_compare;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct State {
    pub registers: [[u8; 10]; 8],
    pub control_word: u16,
    pub status_word: u16,
    pub tag_word: u16,
    pub opcode: u16,
    pub instruction_pointer: u64,
    pub data_pointer: u64,
}
impl Default for State {
    fn default() -> Self {
        Self {
            registers: [[0; 10]; 8],
            control_word: 0x37f,
            status_word: 0,
            tag_word: 0xffff,
            opcode: 0,
            instruction_pointer: 0,
            data_pointer: 0,
        }
    }
}
pub fn classify_tag(raw: u128) -> u16 {
    number::tag(raw)
}

impl State {
    pub fn pending(&self) -> u16 {
        self.status_word & !self.control_word & 63
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.opcode > 0x7ff {
            return Err("invalid x87 opcode");
        }
        let pending = self.pending() != 0;
        if (self.status_word & 0x80 != 0) != pending || (self.status_word & 0x8000 != 0) != pending
        {
            Err("inconsistent x87 pending status")
        } else {
            Ok(())
        }
    }
    pub fn raw(&self, index: usize) -> u128 {
        let mut b = [0; 16];
        b[..10].copy_from_slice(&self.registers[index]);
        u128::from_le_bytes(b)
    }
    pub fn set_raw(&mut self, index: usize, value: u128) {
        self.registers[index].copy_from_slice(&value.to_le_bytes()[..10]);
    }
    pub fn top(&self) -> usize {
        usize::from((self.status_word >> 11) & 7)
    }
    pub fn physical(&self, index: u8) -> usize {
        (self.top() + usize::from(index)) & 7
    }
    pub fn tag(&self, index: usize) -> u16 {
        (self.tag_word >> (2 * index)) & 3
    }
    pub fn set_tag(&mut self, index: usize, tag: u16) {
        self.tag_word = (self.tag_word & !(3 << (2 * index))) | (tag << (2 * index));
    }
    pub fn refresh(&mut self) {
        self.status_word =
            (self.status_word & !0x8080) | if self.pending() != 0 { 0x8080 } else { 0 };
    }
    fn top_to(&mut self, top: usize) {
        self.status_word = (self.status_word & !0x3800) | ((top as u16 & 7) << 11);
    }
    fn put(&mut self, index: usize, value: u128) {
        self.set_raw(index, value);
        self.set_tag(index, number::tag(value));
    }
    fn pop(&mut self) {
        let top = self.top();
        self.set_tag(top, 3);
        self.top_to(top + 1);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Kind {
    Load,
    Store,
    Exchange,
    Free,
    DecTop,
    IncTop,
    #[default]
    Nop,
    ChangeSign,
    Abs,
    Test,
    Examine,
    Constant,
    Add,
    Mul,
    Compare,
    Sub,
    SubReverse,
    Div,
    DivReverse,
    Sqrt,
    Round,
    CompareUnordered,
    CompareFlags,
    CompareUnorderedFlags,
    ConditionalMove,
    LoadControl,
    StoreControl,
    StoreStatus,
    Clear,
    Init,
    Wait,
    Remainder,
    RemainderNearest,
    Scale,
    Extract,
    StoreTruncated,
    Exp2MinusOne,
    MulLog2,
    MulLog2PlusOne,
    Atan2,
    SinCos,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Format {
    #[default]
    None,
    Register,
    F32,
    F64,
    F80,
    I16,
    I32,
    I64,
    Ax,
    Bcd,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Op {
    pub kind: Kind,
    pub format: Format,
    pub index: u8,
    pub dest_index: bool,
    pub pop: u8,
    pub condition: u8,
    pub encoding: u16,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) const REGISTER_PAIR_SEGMENT: u32 = u32::MAX;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) const MEMORY_PAIR_SEGMENT: u32 = 1 << 31;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) const MEMORY_FIRST_PAIR_SEGMENT: u32 = 1 << 30;

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const PAGED_LOAD_PAIR_SEGMENT: u32 = 1 << 29;

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const PAGED_STORE_PAIR_SEGMENT: u32 = 1 << 28;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) fn memory_first_pair(first: Op, second: Op) -> bool {
    matches!(first.format, Format::F32 | Format::F64)
        && (first.kind == Kind::Load
            && second.kind == Kind::Load
            && second.format == Format::Register
            || matches!(
                first.kind,
                Kind::Add | Kind::Mul | Kind::Sub | Kind::SubReverse | Kind::Div | Kind::DivReverse
            ) && second.kind == Kind::Store
                && matches!(second.format, Format::F32 | Format::F64))
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) fn register_load_memory_arithmetic(first: Op, second: Op) -> bool {
    first.kind == Kind::Load
        && first.format == Format::Register
        && matches!(second.format, Format::F32 | Format::F64)
        && matches!(
            second.kind,
            Kind::Add | Kind::Mul | Kind::Sub | Kind::SubReverse | Kind::Div | Kind::DivReverse
        )
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) fn register_load_arithmetic(first: Op, second: Op) -> bool {
    first.kind == Kind::Load
        && first.format == Format::Register
        && second.format == Format::Register
        && matches!(
            second.kind,
            Kind::Add | Kind::Mul | Kind::Sub | Kind::SubReverse
        )
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) fn register_exchange_compare(first: Op, second: Op) -> bool {
    first.kind == Kind::Exchange
        && first.format == Format::Register
        && second.format == Format::Register
        && second.writes_flags()
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) fn register_store_pop(op: Op) -> bool {
    op.kind == Kind::Store && op.format == Format::Register && op.pop == 1
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
impl State {
    pub(crate) fn can_store_register_pop_pair(&self, first: u8) -> bool {
        let top = self.top();
        // A store to ST(1) fills the second pop's source before TOP advances.
        self.pending() == 0 && self.tag(top) != 3 && (first == 1 || self.tag((top + 1) & 7) != 3)
    }

    pub(crate) fn can_load_register(&self, index: u8) -> bool {
        self.pending() == 0
            && self.tag(self.physical(index)) != 3
            && self.tag((self.top() + 7) & 7) == 3
    }

    fn load_admitted_register(&mut self, index: u8) -> (usize, u128) {
        let raw = self.raw(self.physical(index));
        let next = (self.top() + 7) & 7;
        self.status_word &= !0x200;
        self.refresh();
        self.top_to(next);
        self.put(next, raw);
        (next, raw)
    }

    pub(crate) fn load_memory_arithmetic(&mut self, index: u8, op: Op, source: u64) {
        execute::load_memory_arithmetic(self, index, op, u128::from(source));
    }

    pub(crate) fn load_memory_register(&mut self, format: Format, source: u64, index: u8) -> bool {
        execute::load_memory_register(self, format, u128::from(source), index)
    }

    pub(crate) fn memory_arithmetic_store(
        &mut self,
        first: Op,
        source: u64,
        second: Op,
    ) -> Option<Option<u64>> {
        execute::memory_arithmetic_store(self, first, u128::from(source), second)
    }

    pub(crate) fn can_exchange_register(&self, index: u8) -> bool {
        self.pending() == 0 && self.tag(self.top()) != 3 && self.tag(self.physical(index)) != 3
    }

    pub(crate) fn store_register_pop(&mut self, index: u8) -> bool {
        let top = self.top();
        if self.tag(top) == 3 {
            return false;
        }
        let other = (top + usize::from(index)) & 7;
        self.status_word &= !0x200;
        self.refresh();
        if other != top {
            self.put(other, self.raw(top));
        }
        self.set_tag(top, 3);
        self.top_to(top + 1);
        true
    }
}

impl Op {
    pub fn encode(self) -> u32 {
        self.kind as u32
            | ((self.format as u32) << 6)
            | (u32::from(self.index) << 10)
            | ((self.dest_index as u32) << 13)
            | (u32::from(self.pop) << 14)
            | (u32::from(self.condition) << 16)
            | (u32::from(self.encoding) << 19)
    }
    pub fn decode(n: u32) -> Self {
        Self::try_decode(n).expect("trusted x87 operation")
    }
    pub fn try_decode(n: u32) -> Option<Self> {
        use Kind::*;
        let kinds = [
            Load,
            Store,
            Exchange,
            Free,
            DecTop,
            IncTop,
            Nop,
            ChangeSign,
            Abs,
            Test,
            Examine,
            Constant,
            Add,
            Mul,
            Compare,
            Sub,
            SubReverse,
            Div,
            DivReverse,
            Sqrt,
            Round,
            CompareUnordered,
            CompareFlags,
            CompareUnorderedFlags,
            ConditionalMove,
            LoadControl,
            StoreControl,
            StoreStatus,
            Clear,
            Init,
            Wait,
            Remainder,
            RemainderNearest,
            Scale,
            Extract,
            StoreTruncated,
            Exp2MinusOne,
            MulLog2,
            MulLog2PlusOne,
            Atan2,
            SinCos,
        ];
        let formats = [
            Format::None,
            Format::Register,
            Format::F32,
            Format::F64,
            Format::F80,
            Format::I16,
            Format::I32,
            Format::I64,
            Format::Ax,
            Format::Bcd,
        ];
        if n >> 30 != 0 || n & 63 == Kind::Constant as u32 && (n >> 10) & 7 == 7 {
            return None;
        }
        let op = Self {
            kind: *kinds.get((n & 63) as usize)?,
            format: *formats.get(((n >> 6) & 15) as usize)?,
            index: ((n >> 10) & 7) as u8,
            dest_index: n & (1 << 13) != 0,
            pop: ((n >> 14) & 3) as u8,
            condition: ((n >> 16) & 7) as u8,
            encoding: ((n >> 19) & 0x7ff) as u16,
        };
        op.well_formed().then_some(op)
    }
    fn well_formed(self) -> bool {
        use Format::*;
        use Kind::*;
        if self.condition != 0 && self.kind != ConditionalMove {
            return false;
        }
        if self.format != Register && self.kind != Constant && self.index != 0 {
            return false;
        }
        if matches!(self.kind, MulLog2 | MulLog2PlusOne | Atan2) {
            return self.format == Register && self.index == 1 && self.dest_index && self.pop == 1;
        }
        let arithmetic = matches!(self.kind, Add | Mul | Sub | SubReverse | Div | DivReverse);
        if self.dest_index && !(arithmetic && self.format == Register) {
            return false;
        }
        match self.kind {
            Load => {
                matches!(
                    self.format,
                    Register | F32 | F64 | F80 | I16 | I32 | I64 | Bcd
                ) && self.pop == 0
            }
            Store => {
                matches!(
                    self.format,
                    Register | F32 | F64 | F80 | I16 | I32 | I64 | Bcd
                ) && self.pop <= 1
                    && (!matches!(self.format, F80 | I64 | Bcd) || self.pop == 1)
            }
            StoreTruncated => matches!(self.format, I16 | I32 | I64) && self.pop == 1,
            Add | Mul | Sub | SubReverse | Div | DivReverse => {
                matches!(self.format, Register | F32 | F64 | I16 | I32)
                    && if self.dest_index {
                        self.pop <= 1
                    } else {
                        self.pop == 0
                    }
            }
            Compare => {
                matches!(self.format, Register | F32 | F64 | I16 | I32)
                    && (self.pop <= 1
                        || self.pop == 2 && self.format == Register && self.index == 1)
            }
            CompareUnordered => {
                self.format == Register && (self.pop <= 1 || self.pop == 2 && self.index == 1)
            }
            CompareFlags | CompareUnorderedFlags => self.format == Register && self.pop <= 1,
            ConditionalMove | Exchange | Free => self.format == Register && self.pop == 0,
            SinCos => self.format == None && self.pop == 0 && self.encoding == 0x1fb,
            StoreStatus => matches!(self.format, None | Ax) && self.pop == 0,
            Constant => self.format == None && self.index < 7 && self.pop == 0,
            _ => self.format == None && self.pop == 0,
        }
    }
    pub fn is_waiting(self) -> bool {
        !matches!(
            self.kind,
            Kind::StoreControl | Kind::StoreStatus | Kind::Clear | Kind::Init
        )
    }
    pub fn memory_bytes(self) -> u32 {
        match self.format {
            Format::I16 => 2,
            Format::F32 | Format::I32 => 4,
            Format::F64 | Format::I64 => 8,
            Format::F80 | Format::Bcd => 10,
            Format::None
                if matches!(
                    self.kind,
                    Kind::LoadControl | Kind::StoreControl | Kind::StoreStatus
                ) =>
            {
                2
            }
            _ => 0,
        }
    }
    pub fn alignment_intent(self) -> super::alignment::Intent {
        use super::alignment::Intent;
        match self.format {
            Format::F80 | Format::Bcd | Format::F64 | Format::I64 => Intent::Natural(8),
            Format::F32 | Format::I32 => Intent::Natural(4),
            Format::I16 => Intent::Natural(2),
            Format::None if self.memory_bytes() != 0 => Intent::Natural(2),
            _ => Intent::Exempt,
        }
    }
    pub fn is_store(self) -> bool {
        matches!(
            self.kind,
            Kind::Store | Kind::StoreTruncated | Kind::StoreControl | Kind::StoreStatus
        ) && self.format != Format::Register
            && self.format != Format::Ax
    }
    pub fn writes_flags(self) -> bool {
        matches!(self.kind, Kind::CompareFlags | Kind::CompareUnorderedFlags)
    }
    pub fn writes_ax(self) -> bool {
        self.kind == Kind::StoreStatus && self.format == Format::Ax
    }
    pub fn updates_environment(self) -> bool {
        !matches!(
            self.kind,
            Kind::LoadControl
                | Kind::StoreControl
                | Kind::StoreStatus
                | Kind::Clear
                | Kind::Init
                | Kind::Wait
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effect {
    pub value: u128,
    pub flags: u64,
    pub store: bool,
    pub ax: bool,
}
pub fn eval(state: &mut State, op: Op, source: u128, flags: u64) -> Effect {
    execute::eval(state, op, source, flags)
}
