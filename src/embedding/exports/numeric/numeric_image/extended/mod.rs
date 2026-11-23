use super::*;
use crate::embedding::layout::PROTECTION;
use crate::x86::access::GuestMemory;
use crate::x86::numeric_images::extended::{ExtendedImage, HEADER};
use crate::x86::protection::{ProtectionFault, ProtectionPhase};

struct Operand {
    offset: u64,
    base: u64,
    segment: Segment,
    access: Access,
}

impl Operand {
    fn field(
        &self,
        context: &ExecutionContext,
        start: u32,
        width: u32,
        access: Access,
    ) -> Result<u64, AccessFailure> {
        let offset = self.offset.wrapping_add(u64::from(start));
        let address = context.translate(offset, width, self.segment, access, false)?;
        // A contiguous operand cannot continue at zero after the linear address space ends.
        if u128::from(address) != u128::from(self.base) + u128::from(start) {
            return Err(AccessFailure::Linear(Fault {
                address: self.base,
                width: start + width,
                access,
                cause: Cause::AddressRange,
                failing: if context.is_long64() {
                    1u128 << 64
                } else {
                    1u128 << 32
                },
            }));
        }
        Ok(address)
    }
}

enum Failure {
    Access(AccessFailure),
    Header,
    Mxcsr(u32),
}

impl From<AccessFailure> for Failure {
    fn from(value: AccessFailure) -> Self {
        Self::Access(value)
    }
}

fn transfer(
    context: &ExecutionContext,
    memory: &mut impl GuestMemory,
    operand: Operand,
    image: &mut ExtendedImage,
    xcr0: u64,
    mut observe: impl FnMut(u64, u32, Access),
) -> Result<(), Failure> {
    let width = image.header_width();
    let header = operand.field(context, HEADER, width, Access::Read)?;
    memory.read_fields(
        header,
        width,
        &[(0, width)],
        &mut image.bytes[HEADER as usize..],
    )?;
    if !image.accept_header(xcr0) {
        return Err(Failure::Header);
    }
    observe(header, width, Access::Read);

    // Each range gets its own translation and admission, including the header write
    // for a zero request. Page-table aliases may change during final publication.
    let save = operand.access == Access::Write;
    let mut writes = Vec::new();
    for (start, width) in image.fields() {
        let address = operand.field(context, start, width, operand.access)?;
        if save {
            memory.check_write(address, width)?;
            writes.push((address, start, width));
        } else {
            let begin = start as usize;
            memory.read_fields(address, width, &[(0, width)], &mut image.bytes[begin..])?;
            if start == 24 {
                image.mxcsr().map_err(Failure::Mxcsr)?;
            }
        }
        observe(address, width, operand.access);
    }
    if save {
        memory.prepare_publication()?;
        for (address, start, width) in writes {
            let bytes = &image.bytes[start as usize..(start + width) as usize];
            memory
                .write_bytes(address, bytes)
                .expect("admitted extended-state write");
        }
    }
    Ok(())
}

// The extended staging buffer and sparse page walks stay outside the FX hot path.
#[inline(never)]
pub(super) fn execute<const OBSERVED: bool, const PAGED: bool>(
    core: &mut Core,
    operation: ImageOp,
    offset: u64,
    segment: Segment,
) -> i32 {
    let access = if operation.is_store() {
        Access::Write
    } else {
        Access::Read
    };
    let Some(base) = core.segment_address(offset, 1, segment, access, false) else {
        return core.memory_failure_reason();
    };
    if base & 63 != 0 {
        core.alignment_fault = Some(vector::AlignmentFault {
            segment,
            offset,
            address: base,
            width: operation.width(),
            access,
            alignment: 64,
        });
        return ALIGNMENT;
    }
    let raw = unsafe { &*core.state.get() };
    let xcr0 = raw.xcr0;
    let mut image = ExtendedImage::new(operation, xcr0, raw.registers[0], raw.registers[2]);
    if operation.is_store() {
        image.save(&raw.x87, &raw.xmm, raw.mxcsr, raw.protection_keys.pkru);
    }
    let mut candidate = 0;
    let result = guest_semantics!(
        core,
        PAGED,
        transfer,
        Operand {
            offset,
            base,
            segment,
            access
        },
        &mut image,
        xcr0,
        |address, width, access| {
            if OBSERVED {
                candidate |= raw.debug.registers.matches(address, width, access);
            }
        }
    );
    if PAGED && result.is_err() {
        core.paging_work.clear();
    }
    match result {
        Ok(()) => {
            if !operation.is_store() {
                let raw = unsafe { &mut *core.state.get() };
                image.restore(
                    &mut raw.x87,
                    &mut raw.xmm,
                    &mut raw.mxcsr,
                    &mut raw.protection_keys.pkru,
                );
            }
            core.debug_candidate |= candidate;
            if operation.is_store() && core.image.active_changed() {
                2
            } else {
                0
            }
        }
        Err(Failure::Access(fault)) => {
            core.access_failure(fault);
            core.memory_failure_reason()
        }
        Err(Failure::Header) => {
            core.protection_fault = Some(ProtectionFault {
                vector: 13,
                error_code: 0,
                phase: ProtectionPhase::Target,
                selector: None,
            });
            PROTECTION
        }
        Err(Failure::Mxcsr(loaded)) => {
            core.general_protection = Some(numeric::GeneralProtectionFault { loaded });
            GENERAL_PROTECTION
        }
    }
}

#[cfg(test)]
mod tests;
