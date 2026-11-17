use super::*;
use crate::x86::paging::CR4_PKE;
use crate::x86::protection::{ProtectionFault, ProtectionPhase};

#[unsafe(no_mangle)]
pub extern "C" fn core_protection_key(kind: u32, eax: u32, ecx: u32, edx: u32) -> i32 {
    if kind > 1 {
        return -1;
    }
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        if core.paging.is_none_or(|paging| paging.cr4 & CR4_PKE == 0) {
            core.decode_fault = Some(decode::DecodeFault::UndefinedOpcode);
            return INVALID;
        }
        if ecx != 0 || kind == 1 && edx != 0 {
            core.protection_fault = Some(ProtectionFault {
                vector: 13,
                error_code: 0,
                phase: ProtectionPhase::Target,
                selector: None,
            });
            return super::super::layout::PROTECTION;
        }
        let raw = unsafe { &mut *core.state.get() };
        if kind == 0 {
            raw.vector_scratch[..8]
                .copy_from_slice(&u64::from(raw.protection_keys.pkru).to_le_bytes());
            raw.vector_scratch[8..16].fill(0);
        } else {
            raw.protection_keys.pkru = eax;
        }
        0
    })
}
