use crate::embedding::exports::{BRIDGE, Core};
use crate::embedding::layout::{INVALID, SEGMENT, UNSUPPORTED};
use crate::memory::Access;
use crate::x86::{context::Segment, decode, paging};

pub(in crate::embedding::exports) fn parse_bank(
    value: &serde_json::Value,
) -> Result<paging::State, &'static str> {
    let word = |name| {
        value[name]
            .as_str()
            .and_then(|v| v.strip_prefix("0x"))
            .and_then(|v| u64::from_str_radix(v, 16).ok())
            .ok_or("paging fields must be unsigned64 hexadecimal words")
    };
    let state = paging::State {
        cr0: word("cr0")?,
        cr2: word("cr2")?,
        cr3: word("cr3")?,
        cr4: word("cr4")?,
        efer: word("efer")?,
        pdpte: value
            .get("pdpte")
            .map(|entries| {
                let entries = entries
                    .as_array()
                    .filter(|entries| entries.len() == 4)
                    .ok_or("captured PDPTEs require four words")?;
                let mut result = [0; 4];
                for (index, entry) in entries.iter().enumerate() {
                    result[index] = entry
                        .as_str()
                        .and_then(|v| v.strip_prefix("0x"))
                        .and_then(|v| u64::from_str_radix(v, 16).ok())
                        .ok_or("invalid captured PDPTE word")?;
                }
                Ok(result)
            })
            .transpose()?,
    };
    Ok(state)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paging_state() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let Some(state) = bridge.core.as_ref().unwrap().paging else { return bridge.error("paging state requires a paged CPU"); };
        let mut json = serde_json::json!({
            "cr0":format!("0x{:x}",state.cr0),"cr2":format!("0x{:x}",state.cr2),
            "cr3":format!("0x{:x}",state.cr3),"cr4":format!("0x{:x}",state.cr4),"efer":format!("0x{:x}",state.efer),
        });
        if let Some(entries) = state.pdpte { json["pdpte"] = serde_json::json!(entries.map(|entry| format!("0x{entry:x}"))); }
        bridge.info = serde_json::to_vec(&json).unwrap();
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_import_paging() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let candidate = match serde_json::from_slice(&bridge.input)
            .map_err(|_| "invalid paging input")
            .and_then(|value| parse_bank(&value))
        {
            Ok(bank) => bank,
            Err(message) => return bridge.error(message),
        };
        let core = bridge.core.as_mut().unwrap();
        let Some(previous) = core.paging else {
            return bridge.error("paging state requires a paged CPU");
        };
        let validation = if core.physical_controls {
            candidate.validate_physical_context(&core.context)
        } else {
            candidate.validate()
        };
        if let Err(message) = validation {
            return bridge.error(message);
        }
        if previous.translation_changed(candidate) {
            core.revoke_host_translations();
        }
        if previous.numeric_controls() != candidate.numeric_controls() {
            core.context_changed();
        }
        core.paging = Some(candidate);
        core.paging_work.clear();
        core.task_work = None;
        core.alignment_check = None;
        0
    })
}

#[cold]
#[inline(never)]
fn read_machine_status_word(
    core: &mut Core,
    paging: &paging::State,
    segment: u32,
    offset: u64,
    flags: u64,
) -> Result<u16, i32> {
    use crate::x86::{access::GuestMemory, alignment::Intent};

    let Some(segment) = Segment::ALL.get(segment as usize).copied() else {
        return Err(-1);
    };
    let Some(address) = core.segment_address(offset, 2, segment, Access::Read, false) else {
        return Err(core.memory_failure_reason());
    };
    let read = paging::access::Memory::new(
        &mut core.image,
        paging,
        core.context.cpl(),
        flags,
        &mut core.paging_work,
        unsafe { &(*core.state.get()).protection_keys },
    )
    .with_cache(&core.translation_cache)
    .read_aligned(address, 2, Intent::Natural(2));
    match read {
        Ok(source) => {
            core.debug_operand(address, 2, Access::Read);
            Ok(source as u16)
        }
        Err(failure) => {
            core.access_failure(failure);
            Err(core.memory_failure_reason())
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paging_control(
    kind: u32,
    register: u32,
    value: u64,
    _pc: u64,
    flags: u64,
    retired: u64,
) -> i32 {
    use crate::x86::protection::{ProtectionFault, ProtectionPhase};
    use paging::ControlFailure;
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(state) = core.paging else {
            return UNSUPPORTED;
        };
        if core.context.cpl() != 0 {
            core.protection_fault = Some(ProtectionFault {
                vector: 13,
                error_code: 0,
                phase: ProtectionPhase::Privilege,
                selector: None,
            });
            return crate::embedding::layout::PROTECTION;
        }
        if matches!(kind, 2 | 3) && register == crate::x86::protection_keys::PKRS_MSR {
            if kind == 3 && value > u64::from(u32::MAX) {
                core.protection_fault = Some(ProtectionFault {
                    vector: 13,
                    error_code: 0,
                    phase: ProtectionPhase::Target,
                    selector: None,
                });
                return crate::embedding::layout::PROTECTION;
            }
            let raw = unsafe { &mut *core.state.get() };
            if kind == 2 {
                raw.vector_scratch[..8]
                    .copy_from_slice(&u64::from(raw.protection_keys.pkrs).to_le_bytes());
            } else {
                raw.protection_keys.pkrs = value as u32;
            }
            return 0;
        }
        let mut invalidation_target = None;
        let operation = match kind {
            2 | 3 if register == crate::x86::xstate::XSS_MSR => {
                Err(ControlFailure::GeneralProtection)
            }
            0 => state.read_cr(register as u8).map(|v| (state, Some(v))),
            1 => {
                let value = if core.context.is_long64() {
                    value
                } else {
                    u64::from(value as u32)
                };
                if core.physical_controls {
                    state.prepare_physical_cr_write_in_context(register as u8, value, &core.context)
                } else {
                    state.prepare_cr_write(register as u8, value, core.context.mode())
                }
                .map(|s| (s, None))
            }
            2 if matches!(
                register,
                crate::x86::processor::TSC_MSR | crate::x86::processor::TSC_AUX_MSR
            ) =>
            {
                unsafe { (*core.state.get()).time.read_msr(register, retired) }
                    .map(|value| (state, Some(value)))
            }
            2 => if register == paging::EFER_MSR {
                state.read_msr(register)
            } else {
                core.fast_system
                    .read_msr_profile(register, &core.context, core.physical_controls)
            }
            .map(|v| (state, Some(v))),
            3 if matches!(
                register,
                crate::x86::processor::TSC_MSR | crate::x86::processor::TSC_AUX_MSR
            ) =>
            {
                unsafe { (*core.state.get()).time.write_msr(register, value, retired) }
                    .map(|()| (state, None))
            }
            3 if register == paging::EFER_MSR => if core.physical_controls {
                state.prepare_physical_efer_write(value)
            } else {
                state.prepare_msr_write(register, value)
            }
            .map(|s| (s, None)),
            3 => core
                .fast_system
                .prepare_msr_write_profile(register, value, &core.context, core.physical_controls)
                .map(|(bank, context)| {
                    if core.context != context {
                        core.context = context;
                        core.context_changed();
                    }
                    core.fast_system = bank;
                    (state, None)
                }),
            4 => {
                let Some(segment) = Segment::ALL.get(register as usize).copied() else {
                    return -1;
                };
                // Address formation has no target data access.
                let linear = if !core.context.is_long64() {
                    match core
                        .context
                        .translate(value, 0, segment, Access::Read, false)
                    {
                        Ok(linear) => linear,
                        Err(failure) => {
                            core.access_failure(failure);
                            return SEGMENT;
                        }
                    }
                } else {
                    let base = if matches!(segment, Segment::Fs | Segment::Gs) {
                        core.context.segment(segment).base
                    } else {
                        0
                    };
                    let linear = value.wrapping_add(base);
                    if !crate::memory::canonical48(linear) {
                        return 0;
                    }
                    linear
                };
                invalidation_target = Some(linear);
                Ok((state, None))
            }
            5 => Ok((
                paging::State {
                    cr0: state.cr0 & !8,
                    ..state
                },
                None,
            )),
            6 => Ok((state.with_machine_status(value as u16), None)),
            7 => match read_machine_status_word(core, &state, register, value, flags) {
                Ok(source) => Ok((state.with_machine_status(source), None)),
                Err(reason) => return reason,
            },
            _ => return -1,
        };
        match operation {
            Ok((mut candidate, result)) => {
                let context = if core.physical_controls {
                    match state.transition_context(candidate, &core.context) {
                        Ok(context) => context,
                        Err(_) => {
                            core.protection_fault = Some(ProtectionFault {
                                vector: 13,
                                error_code: 0,
                                phase: ProtectionPhase::Target,
                                selector: None,
                            });
                            return crate::embedding::layout::PROTECTION;
                        }
                    }
                } else {
                    core.context.clone()
                };
                if core.physical_controls
                    && kind == 1
                    && state.needs_pdpte_reload(candidate, register as u8)
                {
                    match candidate.load_pdpte(&core.image) {
                        Ok(entries) => candidate.pdpte = Some(entries),
                        Err(paging::PdpteFailure::Physical { slot, fault }) => {
                            core.pdpte_load_fault = Some((register as u8, slot, fault));
                            return 25;
                        }
                        Err(paging::PdpteFailure::Reserved) => {
                            core.protection_fault = Some(ProtectionFault {
                                vector: 13,
                                error_code: 0,
                                phase: ProtectionPhase::Target,
                                selector: None,
                            });
                            return crate::embedding::layout::PROTECTION;
                        }
                    }
                }
                let context_changed = core.context != context;
                core.context = context;
                let translation_changed = state.translation_changed(candidate);
                if let Some(linear) = invalidation_target {
                    core.translation_cache
                        .invalidate_page(state.pcid(), linear, true);
                } else if kind == 1 && register == 3 {
                    let no_flush = state.cr4 & paging::CR4_PCIDE != 0
                        && core.context.is_long64()
                        && value & (1 << 63) != 0;
                    if !no_flush {
                        core.translation_cache.invalidate_pcid(candidate.pcid());
                    }
                } else if translation_changed {
                    let only_pcide_enable = state.cr4 & paging::CR4_PCIDE == 0
                        && candidate.cr4 & paging::CR4_PCIDE != 0
                        && !state.translation_changed(paging::State {
                            cr4: candidate.cr4 & !paging::CR4_PCIDE,
                            ..candidate
                        });
                    if !only_pcide_enable {
                        core.translation_cache.clear();
                    }
                }
                if kind == 1
                    && register == 3
                    && state.cr0 & (1 << 31) != 0
                    && state.cr3 != candidate.cr3
                {
                    core.guest_root_changed();
                } else if kind == 4 || kind == 1 && register == 3 || translation_changed {
                    core.invalidate_runtime_sources();
                } else if context_changed
                    || state.numeric_controls() != candidate.numeric_controls()
                {
                    core.context_changed();
                }
                core.paging = Some(candidate);
                if let Some(value) = result {
                    unsafe {
                        (&mut (*core.state.get()).vector_scratch)[..8]
                            .copy_from_slice(&value.to_le_bytes());
                    }
                }
                0
            }
            Err(ControlFailure::GeneralProtection) => {
                core.protection_fault = Some(ProtectionFault {
                    vector: 13,
                    error_code: 0,
                    phase: ProtectionPhase::Target,
                    selector: None,
                });
                crate::embedding::layout::PROTECTION
            }
            Err(ControlFailure::Unsupported) => UNSUPPORTED,
            Err(ControlFailure::Undefined) => {
                core.decode_fault = Some(decode::DecodeFault::UndefinedOpcode);
                INVALID
            }
        }
    })
}
