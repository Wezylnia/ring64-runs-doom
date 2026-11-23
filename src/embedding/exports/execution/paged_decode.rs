use crate::embedding::exports::*;
use crate::x86::paging;
use crate::x86::protection_keys::State as Keys;

impl Core {
    #[cfg(not(feature = "parallel"))]
    #[cold]
    #[inline(never)]
    pub(in crate::embedding::exports) fn recover_paged_fetch(
        &mut self,
        pc: u64,
    ) -> Option<crate::x86::instruction::Instruction> {
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
    pub(in crate::embedding::exports) fn warm_guarded_code_page(
        &self,
        pc: u64,
        integer_only: bool,
    ) -> bool {
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
    pub(in crate::embedding::exports) fn warm_paged_instruction(
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

    pub(in crate::embedding::exports) fn paged_decode(
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
