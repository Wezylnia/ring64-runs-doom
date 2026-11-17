use super::*;
use crate::embedding::layout::PROTECTION;
use crate::x86::access::GuestMemory;
use crate::x86::context::Mode;
use crate::x86::protection::{ProtectionFault, ProtectionPhase};

fn refuse(core: &mut Core, phase: ProtectionPhase) -> i32 {
    core.protection_fault = Some(ProtectionFault {
        vector: 13,
        error_code: 0,
        phase,
        selector: None,
    });
    PROTECTION
}

#[unsafe(no_mangle)]
pub extern "C" fn core_invpcid(segment: u32, offset: u64, kind: u64, flags: u64) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(bank) = core.paging else {
            return UNSUPPORTED;
        };
        core.fault = None;
        core.segment_fault = None;
        core.alignment_fault = None;
        core.alignment_check = None;
        core.protection_fault = None;
        core.paging_failure = None;
        core.paging_system = None;
        if core.context.mode() == Mode::Virtual8086 || core.context.cpl() != 0 {
            return refuse(core, ProtectionPhase::Privilege);
        }
        let kind = if core.context.is_long64() {
            kind
        } else {
            u64::from(kind as u32)
        };
        if kind > 3 {
            return refuse(core, ProtectionPhase::Target);
        }
        let Some(segment) = Segment::ALL.get(segment as usize).copied() else {
            return -1;
        };
        let Some(address) = core.segment_address(offset, 16, segment, Access::Read, false) else {
            return core.memory_failure_reason();
        };
        let result = crate::x86::paging::access::Memory::new(
            &mut core.image,
            &bank,
            core.context.cpl(),
            flags,
            &mut core.paging_work,
        )
        .with_cache(&core.translation_cache)
        .read_vector_aligned(address, 16, crate::x86::alignment::Intent::Exempt);
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(failure) => {
                core.access_failure(failure);
                return core.memory_failure_reason();
            }
        };
        let descriptor = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let linear = u64::from_le_bytes(bytes[8..].try_into().unwrap());
        let pcid = (descriptor & 0xfff) as u16;
        if descriptor & !0xfff != 0
            || kind < 2 && bank.cr4 & (1 << 17) == 0 && pcid != 0
            || kind == 0 && !crate::memory::canonical48(linear)
        {
            return refuse(core, ProtectionPhase::Target);
        }
        core.invalidate_pcid(kind as u32, pcid, linear);
        core.debug_operand(address, 16, Access::Read);
        // The generated boundary retires this completed invalidation before redispatch.
        0
    })
}
