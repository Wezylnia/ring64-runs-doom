use super::*;
use crate::x86::protection::ProtectionFault;
use crate::x86::{processor, xstate};

#[unsafe(no_mangle)]
pub extern "C" fn core_processor(kind: u32, eax: u32, ecx: u32, delta_or_edx: u64) -> i32 {
    if kind > processor::Instruction::Xsetbv as u32 {
        return -1;
    }
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        if kind >= processor::Instruction::Xgetbv as u32 {
            let raw = unsafe { &mut *core.state.get() };
            let write = (kind == processor::Instruction::Xsetbv as u32)
                .then_some((delta_or_edx << 32) | u64::from(eax));
            let cr4 = core.paging.map_or(0, |state| state.cr4);
            return match xstate::access_xcr0(&mut raw.xcr0, cr4, &core.context, ecx, write) {
                Ok(value) => {
                    if write.is_none() {
                        raw.vector_scratch[..8].copy_from_slice(&value.to_le_bytes());
                    }
                    0
                }
                Err(xstate::ControlFault::Disabled) => {
                    core.decode_fault = Some(decode::DecodeFault::UndefinedOpcode);
                    INVALID
                }
                Err(xstate::ControlFault::GeneralProtection(phase)) => {
                    core.protection_fault = Some(ProtectionFault {
                        vector: 13,
                        error_code: 0,
                        phase,
                        selector: None,
                    });
                    super::super::layout::PROTECTION
                }
            };
        }
        if kind != 0
            && let Err(fault) = processor::admit_timestamp(&core.context, core.paging)
        {
            core.protection_fault = Some(fault);
            return super::super::layout::PROTECTION;
        }
        let raw = unsafe { &mut *core.state.get() };
        if kind == 0 {
            for (slot, value) in raw.vector_scratch.chunks_exact_mut(4).zip(processor::cpuid(
                eax,
                ecx,
                core.paging.map_or(0, |state| state.cr4),
                raw.xcr0,
            )) {
                slot.copy_from_slice(&value.to_le_bytes());
            }
        } else {
            raw.vector_scratch[..8].copy_from_slice(&raw.time.sample(delta_or_edx).to_le_bytes());
            raw.vector_scratch[8..12].copy_from_slice(&raw.time.tsc_aux.to_le_bytes());
        }
        0
    })
}
