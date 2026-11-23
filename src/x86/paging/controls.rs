use crate::x86::context::{ExecutionContext, Mode, Segment};

pub const EFER_MSR: u32 = 0xc0000080;
pub const CR4_TSD: u64 = 1 << 2;
pub const CR4_PGE: u64 = 1 << 7;
pub const CR4_FSGSBASE: u64 = 1 << 16;
pub const CR4_OSXSAVE: u64 = 1 << 18;
pub const CR4_PCIDE: u64 = 1 << 17;
pub const CR4_SMEP: u64 = 1 << 20;
pub const CR4_SMAP: u64 = 1 << 21;
pub const CR4_PKE: u64 = 1 << 22;
pub const CR4_PKS: u64 = 1 << 24;
const CR0_FIXED: u64 = 0x80000033;
const CR0_WP: u64 = 1 << 16;
const CR0_AM: u64 = 1 << 18;
const CR0_NUMERIC: u64 = 0xe;
const CR4_NUMERIC: u64 = 0x600;
const CR4_PROTECTION: u64 = CR4_SMEP | CR4_SMAP;
const CR4_KEYS: u64 = CR4_PKE | CR4_PKS;
const CR4_TRANSLATION: u64 = CR4_PGE | CR4_PCIDE;
const CR3_ALLOWED: u64 = 0x000ffffffffff018;
const CR3_RAW: u64 = 0x000fffffffffffff;
const CR4_FIXED: u64 = 0x628;
const EFER_FIXED: u64 = 0x500;
const EFER_NXE: u64 = 1 << 11;
const EFER_SCE: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct State {
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub efer: u64,
    pub pdpte: Option<[u64; 4]>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            cr0: CR0_FIXED,
            cr2: 0,
            cr3: 0,
            cr4: CR4_FIXED,
            efer: EFER_FIXED,
            pdpte: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlFailure {
    Undefined,
    GeneralProtection,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdpteFailure {
    Physical {
        slot: u8,
        fault: crate::memory::Fault,
    },
    Reserved,
}

impl State {
    pub fn reset() -> Self {
        Self {
            cr0: 0x60000010,
            cr2: 0,
            cr3: 0,
            cr4: 0,
            efer: 0,
            pdpte: None,
        }
    }
    pub fn legacy_pae(self) -> bool {
        self.cr0 & (1 << 31) != 0 && self.cr4 & (1 << 5) != 0 && self.efer & (1 << 10) == 0
    }

    pub fn pcid(self) -> u16 {
        if self.cr4 & CR4_PCIDE != 0 {
            (self.cr3 & 0xfff) as u16
        } else {
            0
        }
    }

    pub fn validate_physical(self, mode: Mode) -> Result<(), &'static str> {
        let pg = self.cr0 & 0x80000000 != 0;
        let pe = self.cr0 & 1 != 0;
        let lma = pg && self.efer & 0x100 != 0;
        let coherent_mode = match mode {
            Mode::Real => !pe && !pg,
            Mode::Protected | Mode::Virtual8086 => pe && !lma,
            Mode::Long64 | Mode::Compatibility => pe && lma,
        };
        if !coherent_mode
            || self.cr0 & !0xe005003f != 0
            || self.cr0 & 0x10 == 0
            || pg && !pe
            || self.cr0 & 0x60000000 == 0x20000000
            || self.cr3 & !CR3_RAW != 0
            || self.cr4
                & !(0x63c
                    | CR4_PROTECTION
                    | CR4_TRANSLATION
                    | CR4_KEYS
                    | CR4_FSGSBASE
                    | CR4_OSXSAVE)
                != 0
            || self.efer & !0xd01 != 0
            || (self.efer & 0x400 != 0) != lma
            || lma && self.cr4 & 0x20 == 0
            || self.cr4 & CR4_PCIDE != 0 && !lma
        {
            return Err("physical controls require a consistent execution family");
        }
        if self.legacy_pae() != self.pdpte.is_some() {
            return Err("captured PDPTE quartet requires active legacy PAE");
        }
        if self
            .pdpte
            .is_some_and(|entries| !entries.into_iter().all(valid_pdpte))
        {
            return Err("reserved present captured PDPTE");
        }
        Ok(())
    }

    pub fn validate_physical_context(self, context: &ExecutionContext) -> Result<(), &'static str> {
        self.validate_physical(context.mode())?;
        if self.efer & 0x400 != 0
            && ((context.mode() == Mode::Long64) != context.attributes(Segment::Cs).long
                || context.mode() == Mode::Long64 && context.segment(Segment::Cs).default32
                || context.tr().valid && matches!(context.tr().attributes & 15, 1 | 3))
        {
            return Err("physical IA32e context has inconsistent loaded CS or task cache");
        }
        Ok(())
    }

    pub fn transition_context(
        self,
        candidate: Self,
        context: &ExecutionContext,
    ) -> Result<ExecutionContext, ControlFailure> {
        if self.efer & 0x400 == 0
            && candidate.efer & 0x400 != 0
            && (context.attributes(Segment::Cs).long
                || context.tr().valid && matches!(context.tr().attributes & 15, 1 | 3))
        {
            return Err(ControlFailure::GeneralProtection);
        }
        let mode = if candidate.cr0 & 1 == 0 {
            Mode::Real
        } else if candidate.efer & 0x400 != 0 {
            if context.attributes(Segment::Cs).long {
                Mode::Long64
            } else {
                Mode::Compatibility
            }
        } else if context.mode() == Mode::Virtual8086 {
            Mode::Virtual8086
        } else {
            Mode::Protected
        };
        let mut result = context.clone();
        result.set_mode_and_cpl(mode, if mode == Mode::Real { 0 } else { context.cpl() });
        Ok(result)
    }

    pub fn prepare_physical_cr_write_in_context(
        self,
        register: u8,
        value: u64,
        context: &ExecutionContext,
    ) -> Result<Self, ControlFailure> {
        // Loaded-CS/task activation restrictions are architectural invalidity,
        // so they precede refusal of otherwise-defined deferred facilities.
        if register == 0
            && self.efer & 0x400 == 0
            && self.efer & 0x100 != 0
            && value & 0x80000000 != 0
            && (context.attributes(Segment::Cs).long
                || context.tr().valid && matches!(context.tr().attributes & 15, 1 | 3))
        {
            return Err(ControlFailure::GeneralProtection);
        }
        self.prepare_physical_cr_write(register, value, context.mode())
    }

    fn normalize_cr0_write(&self, value: u64, mode: Mode) -> Result<u64, ControlFailure> {
        if value >> 32 != 0 {
            return Err(ControlFailure::GeneralProtection);
        }
        let value = (value & 0xe005003f) | 0x10;
        if value & 0x80000001 == 0x80000000
            || value & 0x60000000 == 0x20000000
            || (mode == Mode::Long64 || self.cr4 & CR4_PCIDE != 0) && value & 0x80000000 == 0
        {
            return Err(ControlFailure::GeneralProtection);
        }
        Ok(value)
    }

    fn check_cr4_write(&self, value: u64) -> Result<(), ControlFailure> {
        let defined = 0x7fff | (0x3ff << 16) | (3 << 27) | (1 << 32);
        if value & !defined != 0
            || value & CR4_PCIDE != 0
                && (self.efer & 0x400 == 0 || self.cr4 & CR4_PCIDE == 0 && self.cr3 & 0xfff != 0)
            || value & (1 << 23) != 0 && self.cr0 & CR0_WP == 0
        {
            return Err(ControlFailure::GeneralProtection);
        }
        Ok(())
    }

    pub fn prepare_physical_cr_write(
        self,
        register: u8,
        value: u64,
        mode: Mode,
    ) -> Result<Self, ControlFailure> {
        use ControlFailure::{GeneralProtection, Unsupported};
        let mut candidate = self;
        match register {
            0 => {
                let value = self.normalize_cr0_write(value, mode)?;
                if value & 0x80000000 != 0 && self.efer & 0x100 != 0 && self.cr4 & 0x20 == 0 {
                    return Err(GeneralProtection);
                }
                candidate.cr0 = value;
                candidate.efer = (self.efer & !0x400)
                    | if value & 0x80000000 != 0 && self.efer & 0x100 != 0 {
                        0x400
                    } else {
                        0
                    };
            }
            2 => candidate.cr2 = value,
            3 => {
                if self.efer & 0x400 != 0 {
                    candidate = self.prepare_cr_write(register, value, mode)?;
                } else {
                    candidate.cr3 = u64::from(value as u32)
                        & if self.cr4 & 0x20 != 0 {
                            0xffffffe0
                        } else {
                            0xfffff018
                        };
                }
            }
            4 => {
                self.check_cr4_write(value)?;
                if self.efer & 0x400 != 0 && (value & 0x20 == 0 || (self.cr4 ^ value) & 0x1000 != 0)
                {
                    return Err(GeneralProtection);
                }
                if value
                    & !(0x63c
                        | CR4_PROTECTION
                        | CR4_TRANSLATION
                        | CR4_KEYS
                        | CR4_FSGSBASE
                        | CR4_OSXSAVE)
                    != 0
                {
                    return Err(Unsupported);
                }
                candidate.cr4 = value;
            }
            _ => return self.prepare_cr_write(register, value, mode),
        }
        if !candidate.legacy_pae() {
            candidate.pdpte = None;
        }
        Ok(candidate)
    }

    pub fn prepare_physical_efer_write(self, value: u64) -> Result<Self, ControlFailure> {
        if value & !0xd01 != 0 || self.cr0 & 0x80000000 != 0 && (self.efer ^ value) & 0x100 != 0 {
            return Err(ControlFailure::GeneralProtection);
        }
        let efer = (value & !0x400)
            | if self.cr0 & 0x80000000 != 0 && value & 0x100 != 0 {
                0x400
            } else {
                0
            };
        Ok(Self { efer, ..self })
    }

    pub fn needs_pdpte_reload(self, candidate: Self, register: u8) -> bool {
        candidate.legacy_pae()
            && match register {
                0 => (self.cr0 ^ candidate.cr0) & 0xe0000000 != 0,
                3 => true,
                4 => (self.cr4 ^ candidate.cr4) & 0x001000b0 != 0,
                _ => false,
            }
    }

    pub fn load_pdpte(self, image: &crate::memory::Image) -> Result<[u64; 4], PdpteFailure> {
        let mut entries = [0; 4];
        let root = self.cr3 & 0xffffffe0;
        for (slot, entry) in entries.iter_mut().enumerate() {
            #[cfg(test)]
            super::walk::physical_counts::read(8);
            *entry = image
                .physical_read(root + slot as u64 * 8, 8)
                .map_err(|fault| PdpteFailure::Physical {
                    slot: slot as u8,
                    fault,
                })?;
        }
        if !entries.into_iter().all(valid_pdpte) {
            return Err(PdpteFailure::Reserved);
        }
        Ok(entries)
    }
}

fn valid_pdpte(value: u64) -> bool {
    value & 1 == 0 || value & 0xfff00000000001e6 == 0
}

impl State {
    pub fn validate(self) -> Result<(), &'static str> {
        if self.pdpte.is_some()
            || self.cr0 & !(CR0_WP | CR0_AM | CR0_NUMERIC) != CR0_FIXED & !CR0_NUMERIC
            || self.cr3 & !CR3_RAW != 0
            || self.cr4
                & !(CR4_NUMERIC
                    | CR4_TSD
                    | CR4_PROTECTION
                    | CR4_TRANSLATION
                    | CR4_KEYS
                    | CR4_FSGSBASE
                    | CR4_OSXSAVE)
                != CR4_FIXED & !CR4_NUMERIC
            || self.efer & !(EFER_NXE | EFER_SCE) != EFER_FIXED
        {
            Err("paging controls do not match the normalized IA32e profile")
        } else {
            Ok(())
        }
    }

    pub fn read_cr(self, register: u8) -> Result<u64, ControlFailure> {
        match register {
            0 => Ok(self.cr0),
            2 => Ok(self.cr2),
            3 => Ok(self.cr3),
            4 => Ok(self.cr4),
            8 => Err(ControlFailure::Unsupported),
            _ => Err(ControlFailure::Undefined),
        }
    }

    pub fn prepare_cr_write(
        self,
        register: u8,
        value: u64,
        mode: Mode,
    ) -> Result<Self, ControlFailure> {
        use ControlFailure::{GeneralProtection, Undefined, Unsupported};
        let mut candidate = self;
        match register {
            0 => {
                let value = self.normalize_cr0_write(value, mode)?;
                if value & !(CR0_WP | CR0_AM | CR0_NUMERIC) != CR0_FIXED & !CR0_NUMERIC {
                    return Err(Unsupported);
                }
                candidate.cr0 = value;
            }
            2 => candidate.cr2 = value,
            3 => {
                if value & 0x1ff0000000000000 != 0
                    || value & (1 << 63) != 0 && self.cr4 & CR4_PCIDE == 0
                {
                    return Err(GeneralProtection);
                }
                if value & (3 << 61) != 0 {
                    return Err(Unsupported);
                }
                candidate.cr3 = value
                    & if self.cr4 & CR4_PCIDE != 0 {
                        CR3_RAW
                    } else {
                        CR3_ALLOWED
                    };
            }
            4 => {
                self.check_cr4_write(value)?;
                if value & (1 << 5) == 0 || value & (1 << 12) != 0 {
                    return Err(GeneralProtection);
                }
                if value
                    & !(CR4_NUMERIC
                        | CR4_TSD
                        | CR4_PROTECTION
                        | CR4_TRANSLATION
                        | CR4_KEYS
                        | CR4_FSGSBASE
                        | CR4_OSXSAVE)
                    != CR4_FIXED & !CR4_NUMERIC
                {
                    return Err(Unsupported);
                }
                candidate.cr4 = value;
            }
            8 => {
                return Err(if value & !15 != 0 {
                    GeneralProtection
                } else {
                    Unsupported
                });
            }
            _ => return Err(Undefined),
        }
        Ok(candidate)
    }

    pub fn prepare_efer_write(self, value: u64) -> Result<Self, ControlFailure> {
        if value & !0xd01 != 0 || value & (1 << 8) == 0 {
            return Err(ControlFailure::GeneralProtection);
        }
        Ok(Self {
            efer: (value & (EFER_NXE | EFER_SCE)) | EFER_FIXED,
            ..self
        })
    }

    pub fn read_msr(self, index: u32) -> Result<u64, ControlFailure> {
        if index == EFER_MSR {
            Ok(self.efer)
        } else {
            Err(ControlFailure::Unsupported)
        }
    }

    pub fn prepare_msr_write(self, index: u32, value: u64) -> Result<Self, ControlFailure> {
        if index == EFER_MSR {
            self.prepare_efer_write(value)
        } else {
            Err(ControlFailure::Unsupported)
        }
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn with_machine_status(self, source: u16) -> Self {
        Self {
            // Replace MP, EM and TS. LMSW can set PE, but cannot clear it.
            cr0: (self.cr0 & !0xe) | u64::from(source & 0xf),
            ..self
        }
    }

    pub fn numeric_controls(self) -> u8 {
        ((self.cr0 >> 1) & 7) as u8 | (((self.cr4 >> 9) & 3) as u8) << 3
    }

    pub fn translation_changed(self, other: Self) -> bool {
        (
            self.cr0 & !(CR0_NUMERIC | CR0_AM),
            self.cr3,
            self.cr4 & !(CR4_NUMERIC | CR4_TSD | CR4_KEYS | CR4_FSGSBASE | CR4_OSXSAVE),
            self.efer & EFER_NXE,
            self.pdpte,
        ) != (
            other.cr0 & !(CR0_NUMERIC | CR0_AM),
            other.cr3,
            other.cr4 & !(CR4_NUMERIC | CR4_TSD | CR4_KEYS | CR4_FSGSBASE | CR4_OSXSAVE),
            other.efer & EFER_NXE,
            other.pdpte,
        )
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod pcid_control_tests;

#[cfg(test)]
mod legacy_tests;
