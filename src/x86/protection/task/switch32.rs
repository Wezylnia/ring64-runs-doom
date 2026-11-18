//! Nested protected-mode task switches with 32-bit TSS images.
use super::super::access::Lookup;
use super::super::*;
use super::{Descriptor, TaskRegister, cached_task};
use crate::x86::context::{LdtRegister, LoadedSegment, SegmentAttributes};
use crate::x86::flags;

mod fields;
mod incoming;
use fields::{Write, read_field};

const NT: u32 = 1 << 14;
const TSS_SEGMENTS: [Segment; 6] = [
    Segment::Es,
    Segment::Cs,
    Segment::Ss,
    Segment::Ds,
    Segment::Fs,
    Segment::Gs,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Direct {
        selector: u16,
        descriptor: u64,
        next_pc: u32,
    },
    Gate {
        selector: u16,
        saved_pc: u32,
        saved_flags: u32,
        ext: u16,
        error_code: Option<u32>,
    },
    NestedReturn {
        next_pc: u32,
    },
}

impl Start {
    pub fn direct(selector: u16, descriptor: u64, next_pc: u64) -> Self {
        Self::Direct {
            selector,
            descriptor,
            next_pc: next_pc as u32,
        }
    }

    pub fn gate(
        selector: u16,
        saved_pc: u64,
        saved_flags: u64,
        ext: u16,
        error_code: Option<u32>,
    ) -> Self {
        Self::Gate {
            selector,
            saved_pc: saved_pc as u32,
            saved_flags: saved_flags as u32,
            ext,
            error_code,
        }
    }

    pub fn nested_return(next_pc: u64) -> Self {
        Self::NestedReturn {
            next_pc: next_pc as u32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    Fault(Failure),
}

impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        Self::Fault(value)
    }
}

fn fault(selector: u16, ext: u16, vector: u8, phase: ProtectionPhase) -> Failure {
    Failure::Protection(ProtectionFault {
        vector,
        error_code: (selector & 0xfffc) | ext,
        phase,
        selector: Some(selector),
    })
}

fn lookup_gdt(
    context: &ExecutionContext,
    selector: u16,
    ext: u16,
    vector: u8,
) -> Result<Lookup, Failure> {
    if selector >= 4
        && selector & 4 == 0
        && let Some(lookup) = Lookup::resolve(context, selector).filter(|lookup| lookup.fits(8))
    {
        return Ok(lookup);
    }
    Err(fault(selector, ext, vector, ProtectionPhase::Selector))
}

pub struct Prepared {
    pub cr3: u32,
    writes: Vec<Write>,
    task: TaskRegister,
    selectors: [u16; 6],
    ldt: u16,
    incoming: Incoming,
}

#[derive(Clone, Copy, Debug)]
pub struct Incoming {
    pub registers: [u32; 8],
    pub eip: u32,
    pub flags: u32,
    pub trap: bool,
    ext: u16,
    error_code: Option<u32>,
}

/// Capture the image and admit all outgoing writes under the old page-table root.
/// The caller may then validate a new PAE root before publishing anything.
pub fn prepare(
    context: &ExecutionContext,
    image: &impl GuestMemory,
    start: Start,
    registers: &[u64; 16],
    flags: u64,
) -> Result<Prepared, Error> {
    if context.mode() != Mode::Protected {
        return Err(Error::Unsupported);
    }
    let ext = match start {
        Start::Gate { ext, .. } => ext,
        _ => 0,
    };
    let old = cached_task(context, 0, ext)?;
    if old.attributes & 15 == 3 {
        return Err(Error::Unsupported);
    }
    let returning = matches!(start, Start::NestedReturn { .. });
    let selector = match start {
        Start::Direct { selector, .. } | Start::Gate { selector, .. } => selector,
        Start::NestedReturn { .. } => {
            cached_task(context, 1, ext)?;
            read_field(context, image, old, 0, 2)? as u16
        }
    };
    let invalid = if returning { 10 } else { 13 };
    let lookup = lookup_gdt(context, selector, ext, invalid)?;
    let raw = match start {
        Start::Direct { descriptor, .. } => descriptor,
        _ => lookup.read(context, image, 0)?,
    };
    let descriptor = Descriptor::decode(raw);
    let kind = descriptor.access & 15;
    let required = if returning { 11 } else { 9 };
    if !descriptor.system()
        || kind != required && kind != required - 8
        || matches!(start, Start::Direct { .. })
            && descriptor.dpl() < context.cpl().max((selector & 3) as u8)
    {
        return Err(fault(selector, ext, invalid, ProtectionPhase::Descriptor).into());
    }
    if !descriptor.present() {
        return Err(fault(selector, ext, 11, ProtectionPhase::Descriptor).into());
    }
    if kind != required {
        return Err(Error::Unsupported);
    }
    if descriptor.limit < 0x67 {
        return Err(fault(selector, ext, 10, ProtectionPhase::Descriptor).into());
    }
    cached_task(context, 0x67, ext)?;
    let task = TaskRegister {
        selector,
        valid: true,
        base: u64::from(descriptor.base),
        limit: descriptor.limit,
        attributes: u16::from(descriptor.access | 2) | u16::from(descriptor.flags) << 8,
    };
    let read = |at, width| read_field(context, image, task, at, width);
    let cr3 = read(28, 4)?;
    let eip = read(32, 4)?;
    let loaded_flags = read(36, 4)?;
    if loaded_flags & flags::VM as u32 != 0 {
        return Err(Error::Unsupported);
    }
    let mut incoming_regs = [0; 8];
    for (index, register) in incoming_regs.iter_mut().enumerate() {
        *register = read(40 + 4 * index as u32, 4)?;
    }
    let mut selectors = [0; 6];
    for (index, selector) in selectors.iter_mut().enumerate() {
        *selector = read(72 + 4 * index as u32, 2)? as u16;
    }
    let ldt = read(96, 2)? as u16;
    let trap = read(100, 2)? & 1 != 0;
    let (saved_pc, saved_flags, error_code) = match start {
        Start::Gate {
            saved_pc,
            saved_flags,
            error_code,
            ..
        } => (saved_pc, saved_flags, error_code),
        Start::Direct { next_pc, .. } => (next_pc, flags as u32, None),
        Start::NestedReturn { next_pc } => (next_pc, flags as u32 & !NT, None),
    };
    let mut writes = Vec::with_capacity(18);
    let mut save = |offset, value| {
        writes.push(Write::field(context, image, old, offset, 4, value)?);
        Ok::<_, Failure>(())
    };
    save(32, saved_pc)?;
    save(36, saved_flags)?;
    for (index, value) in registers[..8].iter().enumerate() {
        save(40 + 4 * index as u32, *value as u32)?;
    }
    for (index, segment) in TSS_SEGMENTS.into_iter().enumerate() {
        save(72 + 4 * index as u32, u32::from(context.selector(segment)))?;
    }
    if returning {
        let lookup = lookup_gdt(context, old.selector, ext, 10)?;
        let access = Descriptor::decode(lookup.read(context, image, 0)?).access;
        writes.push(Write::Busy(lookup.prepare_busy_byte(
            context,
            image,
            8,
            access & !2,
        )?));
    } else {
        writes.push(Write::field(
            context,
            image,
            task,
            0,
            2,
            u32::from(old.selector),
        )?);
        writes.push(Write::Busy(lookup.prepare_busy_byte(
            context,
            image,
            8,
            descriptor.access | 2,
        )?));
    }
    Ok(Prepared {
        cr3,
        writes,
        task,
        selectors,
        ldt,
        incoming: Incoming {
            registers: incoming_regs,
            eip,
            flags: (loaded_flags & flags::IMPORTED_MASK as u32)
                | 2
                | if returning { 0 } else { NT },
            trap,
            ext,
            error_code,
        },
    })
}

impl Prepared {
    pub fn commit(
        self,
        context: &mut ExecutionContext,
        image: &mut impl GuestMemory,
    ) -> Result<Incoming, Error> {
        image
            .prepare_publication_ordered()
            .map_err(|failure| self.writes[failure.index.unwrap_or(0)].failure(failure.failure))?;
        for write in self.writes {
            write.commit(image);
        }
        context.set_tr(self.task);
        context.set_mode_and_cpl(Mode::Protected, (self.selectors[1] & 3) as u8);
        context.set_ldtr(LdtRegister {
            selector: self.ldt,
            ..LdtRegister::default()
        });
        for (segment, selector) in TSS_SEGMENTS.into_iter().zip(self.selectors) {
            context.set_segment(
                segment,
                selector,
                LoadedSegment {
                    usable: false,
                    ..LoadedSegment::default()
                },
                SegmentAttributes::default(),
            );
        }
        Ok(self.incoming)
    }
}

#[cfg(test)]
mod tests;
