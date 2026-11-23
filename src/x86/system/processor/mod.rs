use super::protection::{ProtectionFault, ProtectionPhase};
use crate::x86::context::{ExecutionContext, Mode};
use crate::x86::paging::{self, ControlFailure};

pub const TSC_MSR: u32 = 0x10;
pub const TSC_AUX_MSR: u32 = 0xc0000103;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Instruction {
    Cpuid = 0,
    Rdtsc = 1,
    Rdtscp = 2,
    Xgetbv = 3,
    Xsetbv = 4,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct TimeState {
    pub tsc: u64,
    pub tsc_aux: u32,
}

pub fn cpuid(eax: u32, ecx: u32, cr4: u64, xcr0: u64) -> [u32; 4] {
    match eax {
        0 => [0xd, 0x676e6952, 0x656e6967, 0x6e453436],
        1 => [
            0,
            0,
            0x04022201 | (u32::from(cr4 & paging::CR4_OSXSAVE != 0) << 27),
            0x00002110,
        ],
        7 if ecx == 0 => [
            0,
            0x00102489,
            0x80000008 | if cr4 & (1 << 22) != 0 { 1 << 4 } else { 0 },
            0,
        ],
        0xd => match ecx {
            0 => [
                crate::x86::xstate::SUPPORTED as u32,
                if xcr0 & (1 << 9) != 0 {
                    crate::x86::xstate::MAX_BYTES
                } else {
                    crate::x86::xstate::PKRU_OFFSET
                } as u32,
                crate::x86::xstate::MAX_BYTES as u32,
                0,
            ],
            9 => [8, crate::x86::xstate::PKRU_OFFSET as u32, 0, 0],
            _ => [0; 4],
        },
        0x80000000 => [0x80000001, 0, 0, 0],
        0x80000001 => [0, 0, 0, 0x08000000],
        _ => [0; 4],
    }
}

impl TimeState {
    pub fn sample(self, retired: u64) -> u64 {
        self.tsc.wrapping_add(retired)
    }

    pub fn settle(&mut self, retired: u64) {
        self.tsc = self.sample(retired);
    }

    pub fn read_msr(self, index: u32, retired: u64) -> Result<u64, ControlFailure> {
        match index {
            TSC_MSR => Ok(self.sample(retired)),
            TSC_AUX_MSR => Ok(self.tsc_aux.into()),
            _ => Err(ControlFailure::Unsupported),
        }
    }

    pub fn write_msr(
        &mut self,
        index: u32,
        value: u64,
        prior_retired: u64,
    ) -> Result<(), ControlFailure> {
        match index {
            // The same bank is the invocation base until its common exit settles it.
            TSC_MSR => self.tsc = value.wrapping_sub(prior_retired).wrapping_sub(1),
            TSC_AUX_MSR => {
                self.tsc_aux = value
                    .try_into()
                    .map_err(|_| ControlFailure::GeneralProtection)?;
            }
            _ => return Err(ControlFailure::Unsupported),
        }
        Ok(())
    }
}

pub fn admit_timestamp(
    context: &ExecutionContext,
    paging: Option<paging::State>,
) -> Result<(), ProtectionFault> {
    if context.mode() == Mode::Real
        || context.cpl() == 0
        || paging.is_none_or(|state| state.cr4 & paging::CR4_TSD == 0)
    {
        Ok(())
    } else {
        Err(ProtectionFault {
            vector: 13,
            error_code: 0,
            phase: ProtectionPhase::Privilege,
            selector: None,
        })
    }
}

#[cfg(test)]
mod tests;
