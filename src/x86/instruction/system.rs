use super::{Address, Location, Width};
use crate::x86::context::Segment;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemFlagsKind {
    Push,
    Pop,
    Cli,
    Sti,
    Clac,
    Stac,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemFlagsDescriptor {
    pub kind: SystemFlagsKind,
    pub width: Width,
}

impl SystemFlagsDescriptor {
    pub fn encode(self) -> u32 {
        let width = match self.width {
            Width::W16 => 0,
            Width::W32 => 1,
            Width::W64 => 2,
            Width::W8 => panic!("byte FLAGS operand"),
        };
        // Operandless AC forms use the previously reserved width3 words.
        let kind = match self.kind {
            SystemFlagsKind::Push => 0,
            SystemFlagsKind::Pop => 1,
            SystemFlagsKind::Cli => 2,
            SystemFlagsKind::Sti => 3,
            SystemFlagsKind::Clac => return 12 + (width << 1),
            SystemFlagsKind::Stac => return 13 + (width << 1),
        };
        kind | width << 2
    }

    pub fn pack(self) -> u32 {
        self.encode()
    }

    pub fn try_decode(encoded: u32) -> Option<Self> {
        if matches!(encoded, 12 | 13) {
            return Some(Self {
                kind: if encoded == 12 {
                    SystemFlagsKind::Clac
                } else {
                    SystemFlagsKind::Stac
                },
                width: Width::W16,
            });
        }
        if encoded & !15 != 0 {
            return None;
        }
        let kind = match encoded & 3 {
            0 => SystemFlagsKind::Push,
            1 => SystemFlagsKind::Pop,
            2 => SystemFlagsKind::Cli,
            _ => SystemFlagsKind::Sti,
        };
        let width = match encoded >> 2 {
            0 => Width::W16,
            1 => Width::W32,
            2 => Width::W64,
            _ => return None,
        };
        if matches!(kind, SystemFlagsKind::Cli | SystemFlagsKind::Sti) && width != Width::W16 {
            return None;
        }
        Some(Self { kind, width })
    }

    pub fn valid_in_mode(self, context: &crate::x86::context::ExecutionContext) -> bool {
        match self.kind {
            SystemFlagsKind::Cli
            | SystemFlagsKind::Sti
            | SystemFlagsKind::Clac
            | SystemFlagsKind::Stac => self.width == Width::W16,
            SystemFlagsKind::Push | SystemFlagsKind::Pop => {
                self.width == Width::W16
                    || self.width
                        == if context.is_long64() {
                            Width::W64
                        } else {
                            Width::W32
                        }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectionKind {
    StoreGdt,
    StoreIdt,
    LoadGdt,
    LoadIdt,
    ReadLdt,
    LoadLdt,
    ReadSegment(Segment),
    LoadSegment(Segment),
    LoadFar(Segment),
    Lar,
    Lsl,
    Verr,
    Verw,
    ReadTask,
    LoadTask,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftwareEventKind {
    Interrupt,
    Int3,
    Into,
    Int1,
    Return,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftwareEventDescriptor {
    pub kind: SoftwareEventKind,
    pub width: Width,
    pub vector: u8,
}

impl SoftwareEventDescriptor {
    pub fn encode(self) -> u32 {
        let kind = match self.kind {
            SoftwareEventKind::Interrupt => 0,
            SoftwareEventKind::Int3 => 1,
            SoftwareEventKind::Into => 2,
            SoftwareEventKind::Int1 => 3,
            SoftwareEventKind::Return => 4,
        };
        let width = match self.width {
            Width::W16 => 0,
            Width::W32 => 1,
            Width::W64 => 2,
            Width::W8 => panic!("byte software-event operand"),
        };
        kind | width << 3 | u32::from(self.vector) << 8
    }

    pub fn try_decode(encoded: u32) -> Option<Self> {
        if encoded & !0xff1f != 0 {
            return None;
        }
        let kind = match encoded & 7 {
            0 => SoftwareEventKind::Interrupt,
            1 => SoftwareEventKind::Int3,
            2 => SoftwareEventKind::Into,
            3 => SoftwareEventKind::Int1,
            4 => SoftwareEventKind::Return,
            _ => return None,
        };
        let width = match (encoded >> 3) & 3 {
            0 => Width::W16,
            1 => Width::W32,
            2 => Width::W64,
            _ => return None,
        };
        let vector = (encoded >> 8) as u8;
        let valid = match kind {
            SoftwareEventKind::Return => vector == 0,
            SoftwareEventKind::Interrupt => width == Width::W16,
            SoftwareEventKind::Int3 => width == Width::W16 && vector == 3,
            SoftwareEventKind::Into => width == Width::W16 && vector == 4,
            SoftwareEventKind::Int1 => width == Width::W16 && vector == 1,
        };
        valid.then_some(Self {
            kind,
            width,
            vector,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtectionDescriptor {
    pub kind: ProtectionKind,
    pub width: Width,
    pub memory: bool,
}

impl ProtectionDescriptor {
    pub fn encode(self) -> u32 {
        use ProtectionKind::*;
        let kind = match self.kind {
            StoreGdt => 0,
            StoreIdt => 1,
            LoadGdt => 2,
            LoadIdt => 3,
            ReadLdt => 4,
            LoadLdt => 5,
            ReadSegment(segment) => 6 + segment as u32,
            LoadSegment(segment) => 12 + segment as u32,
            LoadFar(segment) => 18 + segment as u32,
            Lar => 24,
            Lsl => 25,
            Verr => 26,
            Verw => 27,
            ReadTask => 28,
            LoadTask => 29,
        };
        let width = match self.width {
            Width::W16 => 0,
            Width::W32 => 1,
            Width::W64 => 2,
            Width::W8 => panic!("byte protection operand"),
        };
        kind | (width << 5) | (u32::from(self.memory) << 7)
    }

    pub fn try_decode(encoded: u32) -> Option<Self> {
        use ProtectionKind::*;
        if encoded & !255 != 0 {
            return None;
        }
        let width = match (encoded >> 5) & 3 {
            0 => Width::W16,
            1 => Width::W32,
            2 => Width::W64,
            _ => return None,
        };
        let memory = encoded & 128 != 0;
        let kind = match encoded & 31 {
            0 => StoreGdt,
            1 => StoreIdt,
            2 => LoadGdt,
            3 => LoadIdt,
            4 => ReadLdt,
            5 => LoadLdt,
            value @ 6..=11 => ReadSegment(Segment::from_id(value - 6)),
            value @ 12..=17 => LoadSegment(Segment::from_id(value - 12)),
            value @ 18..=23 => LoadFar(Segment::from_id(value - 18)),
            24 => Lar,
            25 => Lsl,
            26 => Verr,
            27 => Verw,
            28 => ReadTask,
            29 => LoadTask,
            _ => return None,
        };
        let valid = match kind {
            StoreGdt | StoreIdt | LoadGdt | LoadIdt => memory,
            ReadLdt | ReadTask | ReadSegment(_) => !memory || width == Width::W16,
            LoadLdt | LoadTask | Verr | Verw => width == Width::W16,
            LoadSegment(segment) => segment != Segment::Cs && width == Width::W16,
            LoadFar(segment) => {
                memory
                    && segment != Segment::Cs
                    && (width != Width::W64 || !matches!(segment, Segment::Ds | Segment::Es))
            }
            Lar | Lsl => true,
        };
        valid.then_some(Self {
            kind,
            width,
            memory,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtectionInstruction {
    pub kind: ProtectionKind,
    pub operand: Location,
    pub register: u8,
    pub width: Width,
}

impl ProtectionInstruction {
    pub fn descriptor(self) -> ProtectionDescriptor {
        ProtectionDescriptor {
            kind: self.kind,
            width: self.width,
            memory: matches!(self.operand, Location::Memory(_)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarKind {
    Call,
    Jump,
    Return,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarDescriptor {
    pub kind: FarKind,
    pub width: Width,
    pub memory: bool,
}

impl FarDescriptor {
    pub fn encode(self) -> u32 {
        let kind = match self.kind {
            FarKind::Call => 0,
            FarKind::Jump => 1,
            FarKind::Return => 2,
        };
        let width = match self.width {
            Width::W16 => 0,
            Width::W32 => 1,
            Width::W64 => 2,
            Width::W8 => panic!("byte far operand"),
        };
        kind | (width << 2) | (u32::from(self.memory) << 4)
    }
    pub fn try_decode(encoded: u32) -> Option<Self> {
        if encoded & !31 != 0 {
            return None;
        }
        let kind = match encoded & 3 {
            0 => FarKind::Call,
            1 => FarKind::Jump,
            2 => FarKind::Return,
            _ => return None,
        };
        let width = match (encoded >> 2) & 3 {
            0 => Width::W16,
            1 => Width::W32,
            2 => Width::W64,
            _ => return None,
        };
        let memory = encoded & 16 != 0;
        if kind == FarKind::Return && memory {
            return None;
        }
        Some(Self {
            kind,
            width,
            memory,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarOperand {
    Immediate { selector: u16, offset: u64 },
    Memory(Address),
    Return { cleanup: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarInstruction {
    pub kind: FarKind,
    pub width: Width,
    pub operand: FarOperand,
}

impl FarInstruction {
    pub fn descriptor(self) -> FarDescriptor {
        FarDescriptor {
            kind: self.kind,
            width: self.width,
            memory: matches!(self.operand, FarOperand::Memory(_)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagingInstruction {
    ClearTaskSwitched,
    LoadMachineStatusWord(Location),
    ControlRegister {
        register: u8,
        gpr: u8,
        write: bool,
        width: Width,
    },
    ModelSpecificRegister {
        write: bool,
    },
    InvalidatePage(Address),
    InvalidatePcid {
        address: Address,
        gpr: u8,
        width: Width,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ProtectionKeyInstruction {
    Read = 0,
    Write = 1,
}
