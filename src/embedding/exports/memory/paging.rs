use super::*;
use crate::x86::paging::{self, walk::Failure};
use crate::x86::protection_keys::State as Keys;

impl Core {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn remember_indexed_scalar_read(&self, address: u64, width: u32) {
        if self.state.indexed_paged_reads != 0
            && let Some(paging) = self.paging.as_ref()
            && let Some((slot, translation)) =
                self.translation_cache.recent_indexed_read(paging, address)
            && let Some((page, host)) = indexed_read_page(&self.image, address, width, translation)
        {
            self.translation_cache
                .remember_indexed_read(slot, page, host);
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn remember_indexed_scalar_write(&mut self, address: u64, width: u32) {
        if self.state.indexed_paged_reads == 0 || self.image.active_changed() {
            return;
        }
        let Some(paging) = self.paging.as_ref() else {
            return;
        };
        let Some((slot, translation)) = self.translation_cache.recent_indexed_read(paging, address)
        else {
            return;
        };
        let page = address & !4095;
        let offset = address - page;
        if translation.dirty
            || offset + u64::from(width) > 4096
            || !translation.contains(page)
            || translation.remaining < 4096 - offset
        {
            return;
        }
        let physical = translation.physical - offset;
        if let Some(span) = self.image.physical_direct_write_span(physical, 4096) {
            let host = span.host + (physical - span.base) as u32;
            if host != 0 {
                let readable =
                    self.image.physical_direct_read_pointer(physical, 4096) == Some(host);
                self.translation_cache
                    .remember_indexed_write(slot, page, host, readable);
            }
        }
    }

    #[cfg(not(feature = "parallel"))]
    #[cold]
    #[inline(never)]
    fn recover_paged_fetch(&mut self, pc: u64) -> Option<crate::x86::instruction::Instruction> {
        self.active_owner?;
        if self.image.active_changed() {
            return None;
        }
        let paging = self.paging.as_ref()?;
        let linear = self
            .context
            .translate(pc, 1, Segment::Cs, Access::Execute, false)
            .ok()?;
        self.translation_cache
            .observe(
                paging,
                paging::walk::Request {
                    linear,
                    access: Access::Execute,
                    class: paging::walk::AccessClass::operand(self.context.cpl(), false),
                },
                &Keys::ZERO,
            )
            .ok()??;
        if !self.revalidate_active_owner() {
            return None;
        }
        self.warm_paged_instruction(pc)
    }

    #[cfg(not(feature = "parallel"))]
    #[inline(always)]
    pub(super) fn warm_guarded_code_page(&self, pc: u64, integer_only: bool) -> bool {
        if self.image.active_changed() || !self.context.is_long64() {
            return false;
        }
        let Some(state) = self
            .paging
            .as_ref()
            .filter(|state| state.cr0 & (1 << 31) != 0)
        else {
            return false;
        };
        let Some(physical) = self.active_owner.and_then(|owner| {
            self.translation_owners
                .guarded_code_start(owner, pc, state, integer_only)
        }) else {
            return false;
        };
        self.translation_cache.touch_current_fetch(
            state,
            paging::walk::Request {
                linear: pc,
                access: Access::Execute,
                class: paging::walk::AccessClass::operand(self.context.cpl(), false),
            },
            physical,
        )
    }

    #[cfg(not(feature = "parallel"))]
    pub(super) fn warm_paged_instruction(
        &self,
        pc: u64,
    ) -> Option<crate::x86::instruction::Instruction> {
        if self.image.active_changed() {
            return None;
        }
        let paging = self
            .paging
            .as_ref()
            .filter(|state| state.cr0 & (1 << 31) != 0)?;
        let context = &self.context;
        let (instruction, linear, physical) = self.translation_owners.current_instruction(
            self.active_owner?,
            pc,
            paging,
            |instruction| {
                // Long64 CS is identity, and the active proof covers every instruction byte.
                if context.is_long64() {
                    return Some(pc);
                }
                if instruction.next_pc <= pc {
                    return None;
                }
                context.nonwrapping_code32_fetch(pc, instruction.encoded_len())
            },
        )?;
        let request = paging::walk::Request {
            linear,
            access: Access::Execute,
            class: paging::walk::AccessClass::operand(self.context.cpl(), false),
        };
        self.translation_cache
            .touch_current_fetch(paging, request, physical)
            .then_some(instruction)
    }

    pub(super) fn paged_decode(
        &mut self,
        pc: u64,
    ) -> Result<crate::x86::instruction::Instruction, DecodeError> {
        let paging = self.paging.expect("paged decode profile");
        let context = &self.context;
        let cache = &self.translation_cache;
        let pg0 = paging.cr0 & (1 << 31) == 0;
        let final_byte = |image: &Image, request: paging::walk::Request, physical| {
            image.physical_fetch(physical).map_err(|fault| {
                DecodeError::Paging(
                    request.physical_failure(fault, paging::walk::PhysicalPhase::FinalAccess),
                )
            })
        };
        let mut consumed = if pg0 { None } else { Some([(0, 0, 0); 15]) };
        let mut consumed_length = 0;
        let mut warm_span: Option<(u64, u64, u64)> = None;
        let mut reuse_warm = !pg0;
        decode::decode_with_profile(pc, context, true, |offset| {
            let linear = context
                .translate(offset, 1, Segment::Cs, Access::Execute, false)
                .map_err(|failure| match failure {
                    AccessFailure::Linear(f) => DecodeError::Fetch(f),
                    AccessFailure::Segment(f) => DecodeError::Segment(f),
                    AccessFailure::Paging(f) => DecodeError::Paging(f),
                    AccessFailure::AlignmentCheck(_) => {
                        unreachable!("instruction fetch is alignment exempt")
                    }
                })?;
            let request = paging::walk::Request {
                linear,
                access: Access::Execute,
                class: paging::walk::AccessClass::operand(context.cpl(), false),
            };
            if pg0 {
                let physical = request.unpaged_physical().map_err(DecodeError::Paging)?;
                return final_byte(&self.image, request, physical);
            }
            let consumed = consumed.as_mut().expect("paged encoding buffer");
            if let Some((base, physical, remaining)) = warm_span
                && linear >= base
                && linear - base < remaining
            {
                let physical = physical + (linear - base);
                let byte = final_byte(&self.image, request, physical)?;
                consumed[consumed_length] = (linear, physical, byte);
                consumed_length += 1;
                return Ok(byte);
            }
            warm_span = None;
            let mut accessed_write = false;
            let translation = match cache
                .lookup(&paging, request, &Keys::ZERO)
                .map_err(DecodeError::Paging)?
            {
                Some(translation) => translation,
                // Keys do not restrict fetch; required fetch keeps normal A/fill effects.
                None => paging::walk::fetch_walk(
                    &mut self.image,
                    &paging,
                    request,
                    |image| {
                        accessed_write = true;
                        reuse_warm = false;
                        encoding_current(
                            image,
                            &paging,
                            cache,
                            &consumed[..consumed_length],
                            request.class,
                        )
                    },
                    &Keys::ZERO,
                )
                .map_err(|failure| DecodeError::Paging(cache.fault(&paging, failure)))?
                .ok_or(DecodeError::PagingRestart)?,
            };
            if accessed_write
                && (!encoding_current(
                    &self.image,
                    &paging,
                    cache,
                    &consumed[..consumed_length],
                    request.class,
                ) || !cache
                    .peek(&self.image, &paging, request, &Keys::ZERO)
                    .is_ok_and(|fresh| fresh.physical == translation.physical))
            {
                return Err(DecodeError::PagingRestart);
            }
            let byte = final_byte(&self.image, request, translation.physical)?;
            let translation = cache.fill(&paging, request, translation);
            if reuse_warm {
                warm_span = Some((linear, translation.physical, translation.remaining));
            }
            consumed[consumed_length] = (linear, translation.physical, byte);
            consumed_length += 1;
            Ok(byte)
        })
    }
    pub(super) fn paging_failure_json(&self) -> Option<serde_json::Value> {
        if let Some((control, slot, fault)) = self.pdpte_load_fault {
            return Some(serde_json::json!({
                "phase":"pdpte-load","control":format!("cr{control}"),"slot":slot,
                "physicalAddress":format!("0x{:x}",fault.address),"width":fault.width,
                "failingPhysicalAddress":format!("0x{:x}",fault.failing),"physicalAccess":"read",
                "cause":match fault.cause {Cause::Unmapped=>"unmapped",Cause::Permission=>"permission",Cause::AddressRange=>"address-range",Cause::Noncanonical=>unreachable!("physical address")}
            }));
        }
        let mut json = match self.paging_failure? {
            Failure::Page(fault) => {
                serde_json::json!({"vector":14,"address":format!("0x{:x}",fault.address),"access":access_name(fault.access),"errorCode":fault.error_code})
            }
            Failure::Physical(fault) => serde_json::json!({
                "address":format!("0x{:x}",fault.linear),"access":access_name(fault.access),
                "physicalAddress":format!("0x{:x}",fault.physical.address),"width":fault.physical.width,
                "failingPhysicalAddress":format!("0x{:x}",fault.physical.failing),
                "physicalAccess":access_name(fault.physical.access),"cause":match fault.physical.cause {
                    Cause::Unmapped=>"unmapped",Cause::Permission=>"permission",Cause::AddressRange=>"address-range",Cause::Noncanonical=>unreachable!("physical domain has no canonicality")
                },"phase":fault.phase.as_str(),
            }),
        };
        if let Some(system) = self.paging_system {
            json["table"] = serde_json::json!(system.table.as_str());
            json["systemPhase"] = serde_json::json!(system.phase.as_str());
            match system.reference {
                crate::x86::protection::SystemReference::Selector(selector) => {
                    json["selector"] = serde_json::json!(selector)
                }
                crate::x86::protection::SystemReference::Vector(vector) => {
                    json["referenceVector"] = serde_json::json!(vector)
                }
            }
        } else if matches!(self.paging_failure, Some(Failure::Physical(_))) {
            json["segment"] = serde_json::json!(
                Segment::from_id(unsafe { (*self.state.get()).fault_segment }).name()
            );
        }
        Some(json)
    }
    pub(super) fn memory_failure_reason(&self) -> i32 {
        match self.paging_failure {
            Some(Failure::Page(_)) => 24,
            Some(Failure::Physical(_)) => 25,
            None if self.segment_fault.is_some() => SEGMENT,
            None if self.alignment_check.is_some() => super::super::layout::ALIGNMENT_CHECK,
            None => DATA,
        }
    }

    pub(super) fn system_paging_failure(
        &mut self,
        fault: crate::x86::protection::SystemPagingFault,
    ) -> i32 {
        self.paging_system = Some(fault);
        self.paging_failure(fault.failure)
    }
    pub(super) fn paging_failure(&mut self, failure: Failure) -> i32 {
        self.paging_failure = Some(failure);
        match failure {
            Failure::Page(fault) => {
                self.translation_cache
                    .fault(self.paging.as_ref().expect("paged fault"), failure);
                self.paging
                    .as_mut()
                    .expect("page fault belongs to paged execution")
                    .cr2 = fault.address;
                24
            }
            Failure::Physical(_) => 25,
        }
    }
}

fn encoding_current(
    image: &Image,
    paging: &paging::State,
    cache: &paging::cache::Cache,
    bytes: &[(u64, u64, u8)],
    class: paging::walk::AccessClass,
) -> bool {
    bytes.iter().all(|&(linear, physical, byte)| {
        cache
            .peek(
                image,
                paging,
                paging::walk::Request {
                    linear,
                    access: Access::Execute,
                    class,
                },
                &Keys::ZERO,
            )
            .is_ok_and(|translation| translation.physical == physical)
            && image.physical_fetch(physical) == Ok(byte)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_access_failed() -> i32 {
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        i32::from(
            core.paging_failure.is_some()
                || core.fault.is_some()
                || core.segment_fault.is_some()
                || core.alignment_check.is_some(),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_segmented_access_failed() -> i32 {
    core_paged_access_failed()
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_access_reason() -> i32 {
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        if core.alignment_fault.is_some() {
            super::super::layout::ALIGNMENT
        } else {
            core.memory_failure_reason()
        }
    })
}

impl Core {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    #[inline(always)]
    fn admit_indexed_paged_reads(&mut self) {
        let Some(paging) = self.paging.as_ref() else {
            self.state.indexed_paged_reads = 0;
            return;
        };
        if !self.context.is_long64()
            || self.context.cpl() != 0
            || paging.cr0 & (1 << 31) == 0
            || paging.cr4 & (paging::CR4_SMAP | paging::CR4_PKE | paging::CR4_PKS) != 0
            || self.paged_region_trace.is_some()
        {
            self.state.indexed_paged_reads = 0;
            return;
        }
        // No-flush CR3 changes can keep live slots for another PCID.
        if self.indexed_paged_read_root != Some(paging.cr3) {
            self.translation_cache.clear_indexed_pages();
            self.indexed_paged_read_root = Some(paging.cr3);
        }
        if self.state.indexed_paged_reads == 0 {
            self.state.indexed_paged_reads = self.translation_cache.indexed_reads_ptr();
        }
    }

    fn clear_paged_instruction(&mut self) {
        self.paging_work.clear();
        self.alignment_check = None;
        self.numeric_fault = None;
        self.numeric_form = None;
        self.paging_failure = None;
        self.pdpte_load_fault = None;
        self.paging_system = None;
        self.debug_candidate = 0;
        unsafe {
            (*self.state.get()).fault_segment = Segment::Cs as u32;
        }
    }
}

// A cold entry may retry once after checked execution: 0 retry, 1 enter, 2 checked.
#[cfg(not(feature = "parallel"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_guard(pc: u64, bytes: u32, flags: u64, shadow: u32) -> i32 {
    paged_loop_guard::<false, false, false>(pc, bytes, flags, shadow)
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[derive(Default)]
pub(super) struct GuardedReads {
    pub(super) active: Option<GuardedWalk>,
    authority: Option<(paging::State, paging::walk::AccessClass, Keys)>,
    pub(super) pages: Vec<crate::embedding::layout::paging::LoopReadPage>,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(super) struct GuardedWalk {
    owner: u64,
    cr3: u64,
    pdpte: Option<[u64; 4]>,
    words: paging::walk::UpperWords,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
impl GuardedReads {
    fn end(&mut self, state: &StateStorage) {
        state.loop_read_table.set(0);
        self.active = None;
    }

    fn begin(&mut self, core: &Core, owner: u64, paging: paging::State) {
        let state = &core.state;
        let run = unsafe { &*state.get() };
        let class = paging::walk::AccessClass::operand(
            core.context.cpl(),
            run.flags & crate::x86::flags::AC != 0,
        );
        let authority = (paging, class, run.protection_keys);
        if core.translation_cache.take_loop_read_invalidation() || self.authority != Some(authority)
        {
            let generation = state.loop_read_generation.get() % 4095 + 1;
            if generation == 1 {
                self.pages.fill(Default::default());
            }
            state.loop_read_generation.set(generation);
            self.authority = Some(authority);
        }
        #[cfg(target_arch = "wasm32")]
        if !self.pages.is_empty() {
            state.loop_read_table.set(self.pages.as_ptr() as u32);
        }
        self.active = Some(GuardedWalk {
            owner,
            cr3: paging.cr3,
            pdpte: paging.pdpte,
            words: Default::default(),
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn remember(&mut self, state: &StateStorage, page: u64, host: u32) {
        use crate::embedding::layout::paging::{LOOP_READ_PAGES, LoopReadPage};
        if self.pages.is_empty() {
            self.pages.resize(LOOP_READ_PAGES, Default::default());
        }
        let slot = (page >> 12) as usize & (LOOP_READ_PAGES - 1);
        self.pages[slot] = LoopReadPage {
            tag: page | state.loop_read_generation.get(),
            host_bias: host.wrapping_sub(page as u32),
        };
        state.loop_read_table.set(self.pages.as_ptr() as u32);
    }
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_walk_guard(pc: u64, bytes: u32, flags: u64, shadow: u32) -> i32 {
    paged_walk_guard::<false>(pc, bytes, flags, shadow)
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_walk_loop_guard32(
    pc: u64,
    bytes: u32,
    flags: u64,
    shadow: u32,
) -> i32 {
    paged_walk_guard::<true>(pc, bytes, flags, shadow)
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
fn clear_guarded_walk() {
    BRIDGE.with_borrow(|bridge| {
        if let Some(core) = bridge.core.as_ref() {
            core.guarded_walk.borrow_mut().end(&core.state);
        }
    });
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
fn paged_walk_guard<const FLAT_DATA32: bool>(pc: u64, bytes: u32, flags: u64, shadow: u32) -> i32 {
    // Recheck read authority on every entry. Unwalked page-table words are visit-local.
    clear_guarded_walk();
    let status = paged_loop_guard::<FLAT_DATA32, false, false>(pc, bytes, flags, shadow);
    if status == 1 {
        BRIDGE.with_borrow(|bridge| {
            let core = bridge.core.as_ref().unwrap();
            if !core.image.has_mmio()
                && core.paged_region_trace.is_none()
                && let Some(owner) = core
                    .active_owner
                    .filter(|&owner| core.image.source_is_active(owner))
                && let Some(paging) = core.paging
            {
                core.guarded_walk.borrow_mut().begin(core, owner, paging);
            }
        });
    }
    status
}

#[cfg(not(feature = "parallel"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_read_loop_guard32(
    pc: u64,
    bytes: u32,
    flags: u64,
    shadow: u32,
) -> i32 {
    paged_loop_guard::<true, false, false>(pc, bytes, flags, shadow)
}

#[cfg(not(feature = "parallel"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_sse_loop_guard(pc: u64, bytes: u32, flags: u64, shadow: u32) -> i32 {
    paged_loop_guard::<false, true, false>(pc, bytes, flags, shadow)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_sse_write_loop_guard32(
    pc: u64,
    bytes: u32,
    flags: u64,
    shadow: u32,
) -> i32 {
    paged_loop_guard::<false, true, true>(pc, bytes, flags, shadow)
}

#[cfg(not(feature = "parallel"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_write_loop_guard32(
    pc: u64,
    bytes: u32,
    flags: u64,
    shadow: u32,
) -> i32 {
    paged_loop_guard::<false, false, true>(pc, bytes, flags, shadow)
}

#[cfg(not(feature = "parallel"))]
fn paged_loop_guard<const FLAT_DATA32: bool, const SSE: bool, const FLAT_WRITE32: bool>(
    pc: u64,
    bytes: u32,
    flags: u64,
    shadow: u32,
) -> i32 {
    use crate::embedding::layout::paging::MXCSR_EXCEPTION_MASKS;
    use crate::x86::flags::{RF, TF};

    #[cfg(target_arch = "wasm32")]
    let indexed_reads = shadow & crate::embedding::layout::paging::INDEXED_PAGED_READ_GUARD != 0;
    #[cfg(target_arch = "wasm32")]
    let newest_fetch = shadow & crate::embedding::layout::paging::NEWEST_FETCH_GUARD != 0;
    #[cfg(target_arch = "wasm32")]
    let shadow = shadow
        & !(crate::embedding::layout::paging::INDEXED_PAGED_READ_GUARD
            | crate::embedding::layout::paging::NEWEST_FETCH_GUARD);
    if flags & (RF | TF) != 0 || shadow != 0 {
        return 2;
    }
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        #[cfg(target_arch = "wasm32")]
        if newest_fetch {
            core.state.newest_fetch.tag = 0;
        }
        let state = unsafe { &*core.state.get() };
        // Retained operand reads advance to their same-page successor fetch.
        // Masked arithmetic cannot fault between the read and that fetch.
        if (SSE
            && (!core.numeric_admission_unrestricted()
                || state.mxcsr & MXCSR_EXCEPTION_MASKS != MXCSR_EXCEPTION_MASKS))
            || ((FLAT_DATA32 || SSE && !core.context.is_long64())
                && !core.context.flat_paged_data32())
            || (FLAT_WRITE32 && !core.context.flat_paged_write32())
            || !core.context.single_page_code_span(pc, bytes)
            || state.event.kind != 0
            || state.interrupts != Default::default()
            || state.debug.registers.active()
            || state.debug.continuation != Default::default()
            || core.debug_candidate != 0
        {
            return 2;
        }
        // A data TLB replacement can revoke the active owner's admission
        // while its executable translation and bytes remain current.
        let warm_guard = core.warm_guarded_code_page(pc, false);
        if !warm_guard
            && core.warm_paged_instruction(pc).is_none()
            && core.recover_paged_fetch(pc).is_none()
        {
            return 0;
        }
        // The active owner covers every loop byte. A retained executable page
        // covers the whole lap; admitted operands preserve its translation.
        core.clear_paged_instruction();
        core.reconcile_translation_changes();
        #[cfg(target_arch = "wasm32")]
        if indexed_reads {
            core.admit_indexed_paged_reads();
        }
        #[cfg(target_arch = "wasm32")]
        if newest_fetch
            && warm_guard
            && core.paged_region_trace.is_none()
            && !core.image.active_changed()
            && core.debug_completion_quiet()
            && let (Some(owner), Some(paging)) = (core.active_owner, core.paging.as_ref())
            // Reconciliation can require this owner to validate its fetch again.
            && core
                .translation_owners
                .guarded_code_start(owner, pc, paging, false)
                .is_some()
            && let Some((slot, id)) = core.translation_cache.current_fetch_slot(pc)
        {
            let indexed = if indexed_reads && core.state.indexed_paged_reads != 0 {
                crate::embedding::layout::paging::NEWEST_FETCH_INDEXED
            } else {
                0
            };
            let numeric = if SSE || core.numeric_admission_unrestricted() {
                crate::embedding::layout::paging::NEWEST_FETCH_SSE
            } else {
                0
            };
            core.state.newest_fetch = crate::embedding::layout::paging::NewestFetch {
                tag: (pc & !4095) | 1 | indexed | numeric,
                id,
                table: core.translation_cache.indexed_reads_ptr(),
                source_invalid: core.image.active_source_invalid_ptr(),
                cache_changed: core.translation_cache.changed_ptr(),
                slot,
            };
        }
        1
    })
}

#[cfg(not(feature = "parallel"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_dword(address: u64) -> u64 {
    paged_loop_read::<4, false>(address)
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_walk_dword(address: u64) -> u64 {
    if address & 3 != 0 || !crate::memory::canonical48(address) {
        clear_guarded_walk();
        return u64::MAX;
    }
    BRIDGE.with_borrow(|bridge| {
        let read = || {
            let core = bridge.core.as_ref().unwrap();
            let paging = core.paging.as_ref()?;
            let state = unsafe { &*core.state.get() };
            let request = paging::walk::Request {
                linear: address,
                access: Access::Read,
                class: paging::walk::AccessClass::operand(
                    core.context.cpl(),
                    state.flags & crate::x86::flags::AC != 0,
                ),
            };
            let keys = &state.protection_keys;
            let mut reads = core.guarded_walk.borrow_mut();
            let owner = core
                .active_owner
                .filter(|&id| core.image.source_is_active(id));
            if reads.active.as_ref().is_some_and(|lease| {
                Some(lease.owner) != owner || lease.cr3 != paging.cr3 || lease.pdpte != paging.pdpte
            }) {
                reads.end(&core.state);
            }
            let (value, translation) = core.translation_cache.read_guarded_dword_then_fetch(
                &core.image,
                paging,
                request,
                keys,
                || {
                    owner?;
                    if let Some(lease) = reads.active.as_mut() {
                        lease.words.peek(&core.image, paging, request, keys).ok()
                    } else {
                        paging::walk::peek(&core.image, paging, request, keys).ok()
                    }
                },
            )?;
            // Only a synchronous read-only body may reuse this full-page RAM proof.
            #[cfg(target_arch = "wasm32")]
            if reads.active.is_some()
                && translation.accessed
                && !translation.global
                && let Some((page, host)) = indexed_read_page(&core.image, address, 4, translation)
            {
                reads.remember(&core.state, page, host);
            }
            #[cfg(not(target_arch = "wasm32"))]
            let _ = translation;
            Some(value)
        };
        match read() {
            Some(value) => u64::from(value),
            None => {
                // Checked replay may publish A bits before changing or
                // re-entering this clone.
                let core = bridge.core.as_ref().unwrap();
                core.guarded_walk.borrow_mut().end(&core.state);
                u64::MAX
            }
        }
    })
}

#[cfg(not(feature = "parallel"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_byte(address: u64) -> u64 {
    paged_loop_read::<1, false>(address)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_byte_for_store(address: u64) -> u64 {
    paged_loop_read::<1, true>(address)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_byte_write_pointer(address: u64) -> u64 {
    if !crate::memory::canonical48(address) {
        return u64::MAX;
    }
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let mut read = || {
            if !core
                .active_owner
                .is_some_and(|owner| core.image.source_is_active(owner))
            {
                return None;
            }
            let paging = core.paging.as_ref()?;
            let state = unsafe { &*core.state.get() };
            let request = paging::walk::Request {
                linear: address,
                access: Access::Read,
                class: paging::walk::AccessClass::operand(
                    core.context.cpl(),
                    state.flags & crate::x86::flags::AC != 0,
                ),
            };
            core.translation_cache.data_byte_then_fetch::<false, _>(
                paging,
                request,
                &state.protection_keys,
                None,
                |index, translation| {
                    let write = paging::walk::Request {
                        access: Access::Write,
                        ..request
                    };
                    // The same local slot is already second-newest (or the fetch
                    // itself) after this read. Its paired store adds no touch.
                    let (value, host) = core.image.physical_read_mapped_byte_for_store(
                        translation.physical,
                        || {
                            !translation.global
                                && !translation.dirty
                                && translation
                                    .rights
                                    .authorize(paging, write, &state.protection_keys)
                                    .is_ok()
                        },
                    )?;
                    let slot_and_value = ((index as u32 + 1) << 8) | u32::from(value);
                    let host = host.map_or(0, |pointer| pointer as u32);
                    Some((u64::from(host) << 32) | u64::from(slot_and_value))
                },
            )
        };
        read().unwrap_or(u64::MAX)
    })
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_ready(address: u64, width: u32) -> i32 {
    let supported = match width {
        1 | 4 => address & u64::from(width - 1) == 0 && crate::memory::canonical48(address),
        16 => crate::memory::check_span(address, width, Access::Read).is_ok(),
        _ => false,
    };
    i32::from(supported && paged_loop_operand_ready::<false, false>(address, width))
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_split_read_ready(address: u64, width: u32) -> i32 {
    if width != 16 {
        return core_paged_loop_read_ready(address, width);
    }
    i32::from(
        crate::memory::check_span(address, width, Access::Read).is_ok()
            && paged_loop_operand_ready::<false, true>(address, width),
    )
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_write_ready(address: u64, width: u32) -> i32 {
    if width != 4 || address & 3 != 0 || !crate::memory::canonical48(address) {
        return 0;
    }
    i32::from(paged_loop_operand_ready::<true, false>(address, width))
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn paged_loop_operand_ready<const WRITE: bool, const SPLIT: bool>(
    address: u64,
    width: u32,
) -> bool {
    // The checked instruction already performed its architectural access.
    // Project the retained result without doing a second access or aging it.
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        let Some(paging) = core.paging.as_ref() else {
            return false;
        };
        let state = unsafe { &*core.state.get() };
        let request = paging::walk::Request {
            linear: address,
            access: if WRITE { Access::Write } else { Access::Read },
            class: paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            ),
        };
        core.translation_cache
            .observe(paging, request, &state.protection_keys)
            .is_ok_and(|entry| {
                entry.is_some_and(|entry| {
                    if entry.remaining < u64::from(width) {
                        if !SPLIT || entry.global {
                            return false;
                        }
                        let offset = entry.remaining;
                        let next = paging::walk::Request {
                            linear: address + offset,
                            ..request
                        };
                        let contiguous = core
                            .translation_cache
                            .observe(paging, next, &state.protection_keys)
                            .is_ok_and(|last| {
                                last.is_some_and(|last| {
                                    !last.global
                                        && last.remaining >= u64::from(width) - offset
                                        && entry.physical.checked_add(offset) == Some(last.physical)
                                })
                            });
                        if !contiguous {
                            return false;
                        }
                    }
                    if WRITE {
                        core.image
                            .physical_direct_write_ready(entry.physical, width)
                    } else {
                        // The full vector uses the locally retained span helper.
                        (width != 16 || !entry.global)
                            && core
                                .image
                                .physical_direct_read_span(entry.physical, width)
                                .is_some()
                    }
                })
            })
    })
}

#[cfg(not(feature = "parallel"))]
fn paged_loop_read<const WIDTH: u32, const CAPTURE_SLOT: bool>(address: u64) -> u64 {
    let read = || {
        if address & u64::from(WIDTH - 1) != 0 || !crate::memory::canonical48(address) {
            return None;
        }
        BRIDGE.with_borrow(|bridge| {
            let core = bridge.core.as_ref().unwrap();
            let paging = core.paging.as_ref()?;
            let state = unsafe { &*core.state.get() };
            let class = paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            );
            let request = paging::walk::Request {
                linear: address,
                access: Access::Read,
                class,
            };
            let read = |physical| {
                if WIDTH == 1
                    && let Some(value) = core.image.physical_read_mapped_byte(physical)
                {
                    return Some(u32::from(value));
                }
                if WIDTH == 4
                    && let Some(value) = core.image.physical_read_mapped_dword(physical)
                {
                    return Some(value);
                }
                core.image
                    .physical_read(physical, WIDTH)
                    .ok()
                    .map(|value| value as u32)
            };
            if CAPTURE_SLOT {
                if !core
                    .active_owner
                    .is_some_and(|owner| core.image.source_is_active(owner))
                {
                    return None;
                }
                return core.translation_cache.data_byte_then_fetch::<false, _>(
                    paging,
                    request,
                    &state.protection_keys,
                    None,
                    |index, translation| {
                        core.image
                            .physical_read_mapped_byte(translation.physical)
                            .map(|value| ((index as u32 + 1) << 8) | u32::from(value))
                    },
                );
            }
            core.translation_cache.read_scalar_then_fetch::<WIDTH, _>(
                paging,
                request,
                &state.protection_keys,
                read,
            )
        })
    };
    read().map_or(u64::MAX, u64::from)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn core_paged_loop_read_span(address: u64, bytes: u32, segment: u32) -> u32 {
    use crate::embedding::layout::{
        PAGED_PAIR_CONTINUE, PAGED_PAIR_START, PAGED_PAIR_WINDOW,
        paging::PREFIX_INDEXED_READ_SEGMENT,
    };

    if segment == Segment::Ds as u32 {
        return paged_loop_read_span::<false, false, false, false>(address, bytes, segment);
    }
    if segment == crate::embedding::layout::paging::PREFIX_DWORD_READ_SEGMENT && bytes == 4 {
        return paged_loop_read_span::<false, false, true, false>(address, 4, Segment::Ds as u32);
    }
    if segment == PREFIX_INDEXED_READ_SEGMENT && matches!(bytes, 1 | 2 | 8) {
        return paged_loop_read_span::<false, false, false, true>(
            address,
            bytes,
            Segment::Ds as u32,
        );
    }
    if segment == PREFIX_INDEXED_READ_SEGMENT && bytes == 4 {
        return index_guarded_dword_read(address);
    }
    if bytes == 8 && matches!(segment, PAGED_PAIR_START | PAGED_PAIR_CONTINUE) {
        return paged_loop_read_paired_dwords(address, segment == PAGED_PAIR_CONTINUE);
    }
    if bytes == 4 && segment == PAGED_PAIR_WINDOW {
        return paged_loop_read_pair_window(address);
    }
    0
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[cold]
#[inline(never)]
fn index_guarded_dword_read(address: u64) -> u32 {
    use crate::embedding::layout::paging::PREFIX_DWORD_READ_SEGMENT;

    let result = core_paged_loop_read_span(address, 4, PREFIX_DWORD_READ_SEGMENT);
    if result != 0 {
        BRIDGE.with_borrow(|bridge| {
            let core = bridge.core.as_ref().unwrap();
            if core.state.indexed_paged_reads != 0 {
                core.translation_cache
                    .index_last_guarded_dword_read(address, core.direct_span.host);
            }
        });
    }
    result
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn core_paged_loop_read_span_pages(address: u64, bytes: u32, segment: u32) -> u32 {
    paged_loop_read_span::<true, false, false, false>(address, bytes, segment)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_span_ready(address: u64, bytes: u32, segment: u32) -> u32 {
    let retained = core_paged_loop_read_span_pages(address, bytes, segment);
    if retained != 0 {
        return retained;
    }
    if segment != Segment::Ds as u32
        || bytes == 0
        || crate::memory::check_span(address, bytes, Access::Read).is_err()
    {
        return 0;
    }
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        if !core
            .active_owner
            .is_some_and(|owner| core.image.source_is_active(owner))
            || !core.context.is_long64() && !core.context.flat_paged_read_span32(address, bytes)
        {
            return 0;
        }
        let Some(paging) = core.paging.as_ref() else {
            return 0;
        };
        let state = unsafe { &*core.state.get() };
        let request = paging::walk::Request {
            linear: address,
            access: Access::Read,
            class: paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            ),
        };
        let Some(host) = core.translation_cache.ready_dense_read_then_fetch(
            &core.image,
            paging,
            request,
            &state.protection_keys,
            bytes,
            |physical| core.image.physical_direct_read_pointer(physical, bytes),
        ) else {
            return 0;
        };
        core.direct_span = crate::memory::DirectSpan {
            base: address,
            host,
            length: bytes,
        };
        &raw const core.direct_span as u32
    })
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_block(address: u64) -> u32 {
    let segment = Segment::Ds as u32;
    // Keep the generic bodies inlined in their established span exports.
    let span = core_paged_loop_read_span(address, 16, segment);
    if span != 0 || address & 0xfff <= 0xff0 {
        span
    } else {
        // At most two leaves can contribute to this single vector operand.
        core_paged_loop_read_span_pages(address, 16, segment)
    }
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_block_host(address: u64) -> u32 {
    let segment = Segment::Ds as u32;
    let host = paged_loop_read_span::<false, true, false, false>(address, 16, segment);
    if host != 0 || address & 0xfff <= 0xff0 {
        host
    } else {
        paged_loop_read_span::<true, true, false, false>(address, 16, segment)
    }
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_write_block(address: u64, value_ptr: u32) -> i32 {
    const WIDTH: u32 = 16;
    if crate::memory::check_span(address, WIDTH, Access::Write).is_err() {
        return 0;
    }
    let written = BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        if !core.context.is_long64()
            || !core
                .active_owner
                .is_some_and(|owner| core.image.source_is_active(owner))
        {
            return None;
        }
        let paging = core.paging.as_ref()?;
        let state = unsafe { &*core.state.get() };
        assert_eq!(state.vector_scratch.as_ptr() as u32, value_ptr);
        let bytes = state.vector_scratch;
        let request = paging::walk::Request {
            linear: address,
            access: Access::Write,
            // Unaligned m128 has no #AC. AC still controls supervisor access.
            class: paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            ),
        };
        core.translation_cache.data_then_fetch::<true, _>(
            paging,
            request,
            &state.protection_keys,
            WIDTH,
            true,
            |physical| core.image.physical_direct_write(physical, bytes),
        )
    });
    i32::from(written.is_some())
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_copy_window(source: u64, destination: u64, width: u32) -> u32 {
    use crate::embedding::layout::{COPY_FILL_INDEX, COPY_NO_SPLIT, COPY_SCALAR};
    use crate::memory::check_span;

    let scalar = width & COPY_SCALAR != 0;
    let no_split = width & COPY_NO_SPLIT != 0;
    let populate = width & COPY_FILL_INDEX != 0;
    let scalar_widths = (width >> 8) as u16;
    let width = width & 255;
    let valid = if scalar {
        (2..=32).contains(&width)
    } else {
        (16..=64).contains(&width) && width.is_multiple_of(16)
    };
    if !valid {
        return 0;
    }
    let page_remaining = |address: u64| 4096 - (address & 4095);
    let split = page_remaining(source).min(page_remaining(destination)) < u64::from(width);
    if no_split && split
        || check_span(source, width, Access::Read).is_err()
        || check_span(destination, width, Access::Write).is_err()
    {
        return 0;
    }
    let parts = if scalar && split {
        let parts = std::array::from_fn(|index| u32::from((scalar_widths >> (4 * index)) & 15));
        if parts.iter().any(|part| !matches!(part, 0 | 1 | 2 | 4 | 8))
            || parts.iter().sum::<u32>() != width
        {
            return 0;
        }
        parts
    } else {
        [16; 4]
    };
    BRIDGE
        .with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            if !core.context.is_long64()
                || !core
                    .active_owner
                    .is_some_and(|owner| core.image.source_is_active(owner))
            {
                return None;
            }
            let paging = core.paging.as_ref()?;
            let state = unsafe { &mut *core.state.get() };
            if scalar
                && core.context.cpl() == 3
                && paging.cr0 & (1 << 18) != 0
                && state.flags & crate::x86::flags::AC != 0
            {
                return None;
            }
            let class = paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            );
            let read = paging::walk::Request {
                linear: source,
                access: Access::Read,
                class,
            };
            let write = paging::walk::Request {
                linear: destination,
                access: Access::Write,
                class,
            };
            let mut indexed = [None; 2];
            let mut resolve = |image: &mut Image| {
                if split {
                    core.translation_cache.copy_split_pair_then_fetch(
                        paging,
                        [read, write],
                        &state.protection_keys,
                        width,
                        parts,
                        |physical_read, physical_write| {
                            let source =
                                image.physical_direct_read_pointer(physical_read, width)?;
                            let destination =
                                image.physical_direct_write_span(physical_write, width)?;
                            let offset = (physical_write - destination.base) as u32;
                            // No subsequent range inherits a split proof.
                            Some((source, destination.host + offset, width))
                        },
                    )
                } else {
                    core.translation_cache.copy_pair_then_fetch(
                        paging,
                        [read, write],
                        &state.protection_keys,
                        width,
                        |[(read, read_remaining), (write, write_remaining)],
                         [read_slot, write_slot]| {
                            let (read_span, write_span) =
                                image.physical_direct_copy_spans(read, write, width)?;
                            let read_offset = (read - read_span.base) as u32;
                            let write_offset = (write - write_span.base) as u32;
                            if populate && core.state.indexed_paged_reads != 0 {
                                indexed = [
                                    indexed_copy_page(source, read_offset, read_span, read_slot),
                                    indexed_copy_page(
                                        destination,
                                        write_offset,
                                        write_span,
                                        write_slot,
                                    ),
                                ];
                            }
                            // A 4 KiB window also bounds potential shadowing by overlapping local leaves.
                            let length = page_remaining(source)
                                .min(page_remaining(destination))
                                .min(read_remaining)
                                .min(write_remaining)
                                .min(u64::from(read_span.length - read_offset))
                                .min(u64::from(write_span.length - write_offset))
                                as u32;
                            debug_assert!(length >= width);
                            Some((
                                read_span.host + read_offset,
                                write_span.host + write_offset,
                                length,
                            ))
                        },
                    )
                }
            };
            let (source_host, destination_host, length) =
                resolve(&mut core.image).or_else(|| {
                    core.translation_cache.ready_copy_destination(
                        &mut core.image,
                        paging,
                        [read, write],
                        &state.protection_keys,
                        width,
                        |image, source, destination, prefix, translation| {
                            let (_, write) =
                                image.physical_direct_copy_spans(source, destination, width)?;
                            if prefix != 0 {
                                // Earlier guest stores must not change words needed by the later walk.
                                let start = u64::from(write.host) + destination - write.base;
                                for &entry in translation.entries() {
                                    let slot = u64::from(image.physical_direct_read_pointer(
                                        entry,
                                        translation.entry_width,
                                    )?);
                                    if start < slot + u64::from(translation.entry_width)
                                        && slot < start + prefix
                                    {
                                        return None;
                                    }
                                }
                            }
                            Some(())
                        },
                    )?;
                    resolve(&mut core.image)
                })?;
            if let Some((slot, page, host)) = indexed[0] {
                core.translation_cache
                    .remember_indexed_read(slot, page, host);
            }
            if let Some((slot, page, host)) = indexed[1] {
                core.translation_cache
                    .remember_indexed_write(slot, page, host, false);
            }
            let scratch = &mut state.vector_scratch;
            scratch[..4].copy_from_slice(&source_host.to_le_bytes());
            scratch[4..8].copy_from_slice(&destination_host.to_le_bytes());
            scratch[8..12].copy_from_slice(&(length - width + 1).to_le_bytes());
            Some(scratch.as_ptr() as u32)
        })
        .unwrap_or(0)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline]
fn indexed_copy_page(
    address: u64,
    offset: u32,
    span: crate::memory::DirectSpan,
    slot: usize,
) -> Option<(usize, u64, u32)> {
    let page_offset = offset.checked_sub((address & 4095) as u32)?;
    if span.length.checked_sub(page_offset)? < 4096 {
        return None;
    }
    Some((slot, address & !4095, span.host + page_offset))
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_block_window(address: u64) -> u64 {
    paged_loop_block_window::<false>(address)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_retouchable_block_window(address: u64) -> u64 {
    paged_loop_block_window::<true>(address)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn paged_loop_block_window<const RETOUCH: bool>(address: u64) -> u64 {
    const WIDTH: u32 = 16;

    if crate::memory::check_span(address, WIDTH, Access::Read).is_err() {
        return 0;
    }
    BRIDGE
        .with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            if !core.context.is_long64() && !core.context.flat_paged_read_span32(address, WIDTH) {
                return None;
            }
            let paging = core.paging.as_ref()?;
            let state = unsafe { &*core.state.get() };
            let request = paging::walk::Request {
                linear: address,
                access: Access::Read,
                class: paging::walk::AccessClass::operand(
                    core.context.cpl(),
                    state.flags & crate::x86::flags::AC != 0,
                ),
            };
            core.translation_cache
                .data_translation_then_fetch::<false, _>(
                    paging,
                    request,
                    &state.protection_keys,
                    WIDTH,
                    true,
                    |slot, translation| {
                        let span = core
                            .image
                            .physical_direct_read_span(translation.physical, WIDTH)?;
                        let offset = (translation.physical - span.base) as u32;
                        let length =
                            u64::from(span.length - offset).min(translation.remaining) as u32;
                        if crate::memory::check_span(address, length, Access::Read).is_err()
                            || (!core.context.is_long64()
                                && !core.context.flat_paged_read_span32(address, length))
                        {
                            return None;
                        }
                        // Remember identity as well as the pointer. Generated
                        // locals keep the window inside this guarded lap.
                        if RETOUCH {
                            core.translation_cache
                                .remember_guarded_read_window(slot, translation.cache_id?);
                        }
                        let host = span.host + offset;
                        Some(u64::from(length) << 32 | u64::from(host))
                    },
                )
        })
        .unwrap_or(0)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_retouch_block_window(address: u64) -> i32 {
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        i32::from(
            core.translation_cache
                .retouch_guarded_read_window(address)
                .is_some(),
        )
    })
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn indexed_read_page(
    image: &crate::memory::Image,
    address: u64,
    width: u32,
    translation: paging::walk::Translation,
) -> Option<(u64, u32)> {
    let page = address & !4095;
    let offset = address - page;
    if offset + u64::from(width) > 4096
        || !translation.contains(page)
        || translation.remaining < 4096 - offset
    {
        return None;
    }
    let host = image.physical_direct_read_pointer(translation.physical - offset, 4096)?;
    Some((page, host))
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn paged_loop_read_span<
    const MULTI_PAGE: bool,
    const HOST_POINTER: bool,
    const PREFIX_DWORD_MEMO: bool,
    const INDEXED: bool,
>(
    address: u64,
    bytes: u32,
    segment: u32,
) -> u32 {
    if segment != Segment::Ds as u32
        || bytes == 0
        || crate::memory::check_span(address, bytes, Access::Read).is_err()
    {
        return 0;
    }
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        if !core.context.is_long64() && !core.context.flat_paged_read_span32(address, bytes) {
            return 0;
        }
        let Some(paging) = core.paging.as_ref() else {
            return 0;
        };
        let state = unsafe { &*core.state.get() };
        let request = paging::walk::Request {
            linear: address,
            access: Access::Read,
            class: paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            ),
        };
        let read = |physical| core.image.physical_direct_read_pointer(physical, bytes);
        let mut indexed_page = None;
        let host = if MULTI_PAGE {
            core.translation_cache.contiguous_read_then_fetch(
                paging,
                request,
                &state.protection_keys,
                bytes,
                read,
            )
        } else if PREFIX_DWORD_MEMO {
            core.translation_cache.read_local_dword_then_fetch(
                paging,
                request,
                &state.protection_keys,
                |physical| core.image.physical_direct_read_span(physical, 4),
            )
        } else if INDEXED && core.state.indexed_paged_reads != 0 {
            core.translation_cache
                .data_translation_then_fetch::<false, _>(
                    paging,
                    request,
                    &state.protection_keys,
                    bytes,
                    true,
                    |slot, translation| {
                        if let Some((page, host)) =
                            indexed_read_page(&core.image, address, bytes, translation)
                        {
                            indexed_page = Some((slot, page, host));
                            return Some(host + (address - page) as u32);
                        }
                        read(translation.physical)
                    },
                )
        } else {
            core.translation_cache.data_then_fetch::<false, _>(
                paging,
                request,
                &state.protection_keys,
                bytes,
                true,
                read,
            )
        };
        let Some(host) = host else {
            return 0;
        };
        if let Some((slot, page, host)) = indexed_page {
            core.translation_cache
                .remember_indexed_read(slot, page, host);
        }
        if HOST_POINTER {
            // The guarded MOVUPS consumes this pointer immediately.
            return host;
        }
        core.direct_span = crate::memory::DirectSpan {
            base: address,
            host,
            length: bytes,
        };
        &raw const core.direct_span as u32
    })
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub(super) struct GuardedDwordPair {
    address: u64,
    binding: (usize, u64),
    host: u32,
    window_start: u16,
    window_end: u16,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline(never)]
fn paged_loop_read_pair_window(address: u64) -> u32 {
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        let Some(pair) = core.guarded_dword_pair else {
            return 0;
        };
        // This clone already resolved the binding. Clip reuse to the same 4KiB
        // and readable physical extent, then retain the original access order.
        let offset = address.wrapping_sub(pair.address & !4095);
        if offset < u64::from(pair.window_start)
            || offset > u64::from(pair.window_end - 4)
            || offset & 3 != 0
            || core
                .translation_cache
                .retouch_guarded_read_binding(pair.binding, address)
                .is_none()
        {
            return 0;
        }
        pair.host
            .wrapping_add(address.wrapping_sub(pair.address) as u32)
    })
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline(never)]
fn paged_loop_read_paired_dwords(address: u64, continuing: bool) -> u32 {
    const BYTES: u32 = 8;
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(paging) = core.paging.as_ref() else {
            return 0;
        };
        // The emitting clone starts fresh after every guard or replay.
        // A live slot keeps the previously authorized mapping and extent.
        let host = if continuing
            && let Some(pair) = core.guarded_dword_pair
            && pair.address == address
            && core
                .translation_cache
                .retouch_guarded_read_binding(pair.binding, address)
                .is_some()
        {
            pair.host
        } else {
            core.guarded_dword_pair = None;
            if !core.context.is_long64()
                || crate::memory::check_span(address, BYTES, Access::Read).is_err()
            {
                return 0;
            }
            let state = unsafe { &*core.state.get() };
            let request = paging::walk::Request {
                linear: address,
                access: Access::Read,
                class: paging::walk::AccessClass::operand(
                    core.context.cpl(),
                    state.flags & crate::x86::flags::AC != 0,
                ),
            };
            let Some((memo, host)) = core
                .translation_cache
                .data_translation_then_fetch::<false, _>(
                    paging,
                    request,
                    &state.protection_keys,
                    BYTES,
                    true,
                    |slot, translation| {
                        let physical = translation.physical;
                        let span = core.image.physical_direct_read_span(physical, BYTES)?;
                        let host = span.host + (physical - span.base) as u32;
                        let page = physical & !4095;
                        let memo = translation.cache_id.map(|id| GuardedDwordPair {
                            address,
                            binding: (slot, id),
                            host,
                            window_start: span.base.saturating_sub(page) as u16,
                            window_end: (span.base + u64::from(span.length) - page).min(4096)
                                as u16,
                        });
                        Some((memo, host))
                    },
                )
            else {
                return 0;
            };
            if let Some(pair) = memo
                && core.state.indexed_paged_reads != 0
                && pair.window_start == 0
                && pair.window_end == 4096
            {
                let page = address & !4095;
                core.translation_cache.remember_indexed_read(
                    pair.binding.0,
                    page,
                    host - (address - page) as u32,
                );
            }
            core.guarded_dword_pair = memo;
            host
        };
        core.direct_span = crate::memory::DirectSpan {
            base: address,
            host,
            length: BYTES,
        };
        &raw const core.direct_span as u32
    })
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_pair(
    first: u64,
    first_bytes: u32,
    last: u64,
    last_bytes: u32,
) -> u64 {
    paged_loop_read_pair::<false>(first, first_bytes, last, last_bytes)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_split_pair(
    first: u64,
    first_bytes: u32,
    last: u64,
    last_bytes: u32,
) -> u64 {
    paged_loop_read_pair::<true>(first, first_bytes, last, last_bytes)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline(always)]
fn paged_loop_read_pair<const SPLIT: bool>(
    first: u64,
    first_bytes: u32,
    last: u64,
    last_bytes: u32,
) -> u64 {
    let spans = [(first, first_bytes), (last, last_bytes)];
    if spans.iter().any(|&(address, bytes)| {
        address & 3 != 0
            || bytes < 4
            || (SPLIT && bytes & 15 != 0)
            || crate::memory::check_span(address, bytes, Access::Read).is_err()
    }) {
        return 0;
    }
    BRIDGE
        .with_borrow(|bridge| {
            let core = bridge.core.as_ref()?;
            if !core.context.is_long64()
                && spans
                    .iter()
                    .any(|&(address, bytes)| !core.context.flat_paged_read_span32(address, bytes))
            {
                return None;
            }
            let paging = core.paging.as_ref()?;
            let state = unsafe { &*core.state.get() };
            let class = paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            );
            let requests = spans.map(|(linear, bytes)| {
                (
                    paging::walk::Request {
                        linear,
                        access: Access::Read,
                        class,
                    },
                    bytes,
                )
            });
            let access = |[first, last]: [u64; 2]| {
                let first = core
                    .image
                    .physical_direct_read_pointer(first, first_bytes)?;
                let last = core.image.physical_direct_read_pointer(last, last_bytes)?;
                Some(u64::from(first) | (u64::from(last) << 32))
            };
            if SPLIT {
                core.translation_cache.read_split_pair_then_fetch(
                    paging,
                    &state.protection_keys,
                    requests,
                    access,
                )
            } else {
                core.translation_cache.read_pair_then_fetch(
                    paging,
                    &state.protection_keys,
                    requests,
                    access,
                )
            }
        })
        .unwrap_or(0)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_read_dword_window(address: u64) -> u32 {
    use crate::memory::DirectSpan;

    if address & 3 != 0 || !crate::memory::canonical48(address) {
        return 0;
    }
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(paging) = core.paging.as_ref() else {
            return 0;
        };
        let state = unsafe { &*core.state.get() };
        let request = paging::walk::Request {
            linear: address,
            access: Access::Read,
            class: paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            ),
        };
        let Some(window) = core
            .translation_cache
            .data_translation_then_fetch::<false, _>(
                paging,
                request,
                &state.protection_keys,
                4,
                true,
                |slot, translation| {
                    let span = core
                        .image
                        .physical_direct_read_span(translation.physical, 4)?;
                    let first = span.base.max(translation.physical_base).next_multiple_of(4);
                    let end = (span.base + u64::from(span.length))
                        .min(translation.physical_base + translation.span);
                    let length = u32::try_from(end - first).ok()?;
                    let base = translation.linear_base + (first - translation.physical_base);
                    if !core.context.is_long64()
                        && !core.context.flat_paged_read_span32(base, length)
                    {
                        return None;
                    }
                    core.translation_cache
                        .remember_guarded_read_window(slot, translation.cache_id?);
                    Some(DirectSpan {
                        base,
                        length,
                        host: span.host + (first - span.base) as u32,
                    })
                },
            )
        else {
            return 0;
        };
        core.direct_span = window;
        &raw const core.direct_span as u32
    })
}

#[cfg(not(feature = "parallel"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_write_byte(address: u64, value: u32) -> i32 {
    paged_loop_write(address, [value as u8])
}

#[cfg(not(feature = "parallel"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_write_dword(address: u64, value: u32) -> i32 {
    paged_loop_write(address, value.to_le_bytes())
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_indexed_dword_write(
    index: u64,
    table: u64,
    store: u64,
    continuing: u32,
) -> u32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        if continuing == 0 {
            core.indexed_write_memo = Default::default();
        }
        if !crate::memory::canonical48(index) {
            return 0;
        }
        let Some(paging) = core.paging.as_ref() else {
            return 0;
        };
        let state = unsafe { &*core.state.get() };
        let class = paging::walk::AccessClass::operand(
            core.context.cpl(),
            state.flags & crate::x86::flags::AC != 0,
        );
        let request = |linear, access| paging::walk::Request {
            linear,
            access,
            class,
        };
        let Some(proof) = core.translation_cache.indexed_dword_write_then_fetch(
            &mut core.image,
            paging,
            &state.protection_keys,
            request(index, Access::Read),
            (table, request(store, Access::Write)),
            &mut core.indexed_write_memo,
        ) else {
            if core.indexed_write_memo.unsupported_leaf {
                return crate::embedding::layout::INDEXED_WRITE_UNSUPPORTED_LEAF;
            }
            return 0;
        };
        core.indexed_dword_write = proof;
        &raw const core.indexed_dword_write as u32
    })
}

#[cfg(not(feature = "parallel"))]
fn paged_loop_write<const WIDTH: usize>(address: u64, value: [u8; WIDTH]) -> i32 {
    if address & (WIDTH as u64 - 1) != 0 || !crate::memory::canonical48(address) {
        return 0;
    }
    let written = BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let paging = core.paging.as_ref()?;
        let state = unsafe { &*core.state.get() };
        let request = paging::walk::Request {
            linear: address,
            access: Access::Write,
            class: paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            ),
        };
        let write = |physical| core.image.physical_direct_write(physical, value);
        if WIDTH == 4 {
            return core.translation_cache.write_dword_then_fetch(
                paging,
                request,
                &state.protection_keys,
                write,
            );
        }
        core.translation_cache.data_then_fetch::<true, _>(
            paging,
            request,
            &state.protection_keys,
            WIDTH as u32,
            false,
            write,
        )
    });
    i32::from(written.is_some())
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_paged_loop_write_byte_after_read(
    address: u64,
    value: u32,
    slot: u32,
) -> i32 {
    if !crate::memory::canonical48(address) {
        return 0;
    }
    let written = BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        if !core
            .active_owner
            .is_some_and(|owner| core.image.source_is_active(owner))
        {
            return None;
        }
        let paging = core.paging.as_ref()?;
        let state = unsafe { &*core.state.get() };
        let request = paging::walk::Request {
            linear: address,
            access: Access::Write,
            class: paging::walk::AccessClass::operand(
                core.context.cpl(),
                state.flags & crate::x86::flags::AC != 0,
            ),
        };
        core.translation_cache.data_byte_then_fetch::<true, _>(
            paging,
            request,
            &state.protection_keys,
            slot.checked_sub(1).map(|index| index as usize),
            |_, translation| {
                core.image
                    .physical_direct_write(translation.physical, [value as u8])
            },
        )
    });
    i32::from(written.is_some())
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_debug_begin(pc: u64, flags: u64, shadow: u32) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.clear_paged_instruction();
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let (indexed_data, indexed_vector, shadow) = {
            use crate::embedding::layout::debug::{
                PAGED_INDEXED_SCALAR_READ, PAGED_INDEXED_VECTOR,
            };
            let indexed_bits = PAGED_INDEXED_SCALAR_READ | PAGED_INDEXED_VECTOR;
            (
                shadow & indexed_bits != 0,
                shadow & PAGED_INDEXED_VECTOR != 0,
                shadow & !indexed_bits,
            )
        };
        #[cfg(not(feature = "parallel"))]
        let (near_transfer, quiet_completion, shadow) = {
            use crate::embedding::layout::debug::{PAGED_NEAR_TRANSFER, PAGED_QUIET_COMPLETION};
            (
                shadow & PAGED_NEAR_TRANSFER != 0,
                shadow & PAGED_QUIET_COMPLETION != 0,
                shadow & !(PAGED_NEAR_TRANSFER | PAGED_QUIET_COMPLETION),
            )
        };
        let debug = core.debug_execute(pc, flags, shadow);
        if debug != 0 {
            return debug;
        }
        #[cfg(not(feature = "parallel"))]
        let integer_only = !near_transfer;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let integer_only =
            integer_only && !(indexed_vector && core.numeric_admission_unrestricted());
        #[cfg(not(feature = "parallel"))]
        if core.warm_guarded_code_page(pc, integer_only) {
            core.reconcile_translation_changes();
            #[cfg(target_arch = "wasm32")]
            if indexed_data {
                if core.debug_completion_quiet() {
                    core.admit_indexed_paged_reads();
                    if core.state.indexed_paged_reads != 0 {
                        core.fault = None;
                    }
                } else {
                    core.state.indexed_paged_reads = 0;
                }
            }
            if quiet_completion
                && core.paged_region_trace.is_none()
                && flags & crate::x86::flags::TF == 0
                && shadow != 2
                && core.debug_completion_quiet()
            {
                return crate::embedding::layout::debug::CONTINUE_ACTIVE;
            }
            return 0;
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if indexed_data {
            core.state.indexed_paged_reads = 0;
        }
        #[cfg(not(feature = "parallel"))]
        if let Some(instruction) = core
            .warm_paged_instruction(pc)
            .or_else(|| core.recover_paged_fetch(pc))
        {
            core.reconcile_translation_changes();
            let result = core.instruction_admission(instruction.operation, pc, true);
            if quiet_completion
                && !core.context.is_long64()
                && result == 0
                && core.paged_region_trace.is_none()
                && flags & crate::x86::flags::TF == 0
                && shadow != 2
                && core.debug_completion_quiet()
            {
                return crate::embedding::layout::debug::CONTINUE_ACTIVE;
            }
            return result;
        }
        match core.paged_decode(pc) {
            Ok(instruction) if !core.image.active_changed() => {
                core.reconcile_translation_changes();
                if core
                    .paging
                    .is_some_and(|paging| paging.cr0 & (1 << 31) != 0)
                    && core.active_owner.is_some_and(|owner| {
                        !core
                            .translation_owners
                            .instruction_matches(owner, instruction)
                    })
                {
                    core.invalidate_runtime_sources();
                    return 26;
                }
                core.instruction_admission(instruction.operation, pc, true)
            }
            Ok(_) | Err(DecodeError::PagingRestart) => 26,
            Err(DecodeError::Paging(failure)) => core.paging_failure(failure),
            Err(DecodeError::Fetch(fault)) => {
                core.fault = Some(fault);
                FETCH
            }
            Err(DecodeError::Segment(fault)) => {
                core.segment_fault = Some(fault);
                SEGMENT
            }
            Err(error) => {
                core.decode_fault = error.architectural_fault();
                if matches!(error, DecodeError::Unsupported | DecodeError::Undefined) {
                    UNSUPPORTED
                } else {
                    INVALID
                }
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_debug_finish(
    pc: u64,
    resume: u64,
    completed: u32,
    flags: u64,
    decision: u32,
) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let reason = core.finish_debug(pc, resume, completed != 0, flags, decision);
        if reason == 0 || reason == super::super::layout::debug::CONTINUE_ACTIVE {
            if core.image.active_changed() {
                0
            } else {
                super::super::layout::debug::CONTINUE_ACTIVE
            }
        } else {
            reason
        }
    })
}

fn access_name(access: Access) -> &'static str {
    match access {
        Access::Read => "read",
        Access::Write => "write",
        Access::Execute => "execute",
    }
}

pub(super) fn parse_bank(value: &serde_json::Value) -> Result<paging::State, &'static str> {
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
            return super::super::layout::PROTECTION;
        }
        if matches!(kind, 2 | 3) && register == crate::x86::protection_keys::PKRS_MSR {
            if kind == 3 && value > u64::from(u32::MAX) {
                core.protection_fault = Some(ProtectionFault {
                    vector: 13,
                    error_code: 0,
                    phase: ProtectionPhase::Target,
                    selector: None,
                });
                return super::super::layout::PROTECTION;
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
                            return super::super::layout::PROTECTION;
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
                            return super::super::layout::PROTECTION;
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
                super::super::layout::PROTECTION
            }
            Err(ControlFailure::Unsupported) => UNSUPPORTED,
            Err(ControlFailure::Undefined) => {
                core.decode_fault = Some(decode::DecodeFault::UndefinedOpcode);
                INVALID
            }
        }
    })
}
