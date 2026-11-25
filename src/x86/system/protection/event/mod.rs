use super::access::DescriptorWrite;
use super::{ExecutionContext, GuestMemory, ProtectionFault, ProtectionPhase, SystemMemoryFault};
#[cfg(test)]
use crate::memory::Image;
use crate::x86::context::{AccessFailure, Mode, Segment, SegmentFault};
use crate::x86::exception::{ExceptionEvent, InterruptEvent};
use crate::x86::instruction::{SoftwareEventDescriptor, SoftwareEventKind, Width};

mod entry;
mod flags;
mod frame;
mod returning;
mod system;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventInputs {
    pub flags: u64,
    pub sp: u64,
    pub next_pc: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventOutcome {
    pub pc: u64,
    pub sp: u64,
    pub flags: u64,
    pub context_changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventFailure {
    Operand {
        segment: Segment,
        failure: AccessFailure,
    },
    StackLimit {
        failure: SegmentFault,
        protection: ProtectionFault,
    },
    Protection(ProtectionFault),
    SystemMemory(SystemMemoryFault),
    SystemPaging(super::SystemPagingFault),
    Task(super::task::switch32::Start),
    Unsupported,
}

impl EventFailure {
    pub fn architectural(self, context: &ExecutionContext) -> Option<ProtectionFault> {
        match self {
            Self::Protection(fault) => Some(fault),
            Self::StackLimit { protection, .. } => Some(protection),
            Self::Operand { segment, failure } => super::access_fault(context, segment, failure),
            Self::SystemMemory(fault) => super::system_fault(context, fault),
            Self::SystemPaging(_) => None,
            Self::Unsupported | Self::Task(_) => None,
        }
    }
}

pub fn valid_in_mode(descriptor: SoftwareEventDescriptor, context: &ExecutionContext) -> bool {
    descriptor.width != Width::W8
        && SoftwareEventDescriptor::try_decode(descriptor.encode()) == Some(descriptor)
        && (descriptor.width != Width::W64 || context.is_long64())
        && !(descriptor.kind == SoftwareEventKind::Into && context.is_long64())
}

pub fn execute(
    context: &mut ExecutionContext,
    image: &mut impl GuestMemory,
    descriptor: SoftwareEventDescriptor,
    inputs: EventInputs,
) -> Result<EventOutcome, EventFailure> {
    if descriptor.width == Width::W8 || !valid_in_mode(descriptor, context) {
        return Err(EventFailure::Unsupported);
    }
    if descriptor.kind == SoftwareEventKind::Return {
        return returning::execute(context, image, descriptor, inputs);
    }
    if descriptor.kind == SoftwareEventKind::Into && inputs.flags & 0x800 == 0 {
        return Ok(EventOutcome {
            pc: inputs.next_pc,
            sp: inputs.sp,
            flags: inputs.flags,
            context_changed: false,
        });
    }
    entry::execute(context, image, descriptor, inputs)
}

pub fn enter_exception(
    context: &mut ExecutionContext,
    image: &mut impl GuestMemory,
    event: ExceptionEvent,
    sp: u64,
    live_flags: u64,
) -> Result<EventOutcome, EventFailure> {
    if event
        .validate(event.return_pc, live_flags, context)
        .is_err()
    {
        return Err(EventFailure::Unsupported);
    }
    entry::enter_exception(context, image, event, sp, live_flags)
}

pub fn enter_interrupt(
    context: &mut ExecutionContext,
    image: &mut impl GuestMemory,
    event: InterruptEvent,
    sp: u64,
    live_flags: u64,
) -> Result<EventOutcome, EventFailure> {
    if event
        .validate(event.return_pc, live_flags, context)
        .is_err()
    {
        return Err(EventFailure::Unsupported);
    }
    entry::enter_interrupt(context, image, event, sp, live_flags)
}

fn fault(
    vector: u8,
    error_code: u16,
    phase: ProtectionPhase,
    selector: Option<u16>,
) -> EventFailure {
    EventFailure::Protection(ProtectionFault {
        vector,
        error_code,
        phase,
        selector,
    })
}

fn selector_fault(selector: u16, vector: u8, ext: u16, phase: ProtectionPhase) -> EventFailure {
    fault(vector, (selector & 0xfffc) | ext, phase, Some(selector))
}

fn publish_descriptors(
    image: &mut impl GuestMemory,
    code_write: Option<DescriptorWrite>,
    stack_write: Option<DescriptorWrite>,
) -> Result<(), EventFailure> {
    let descriptors = [code_write, stack_write];
    image
        .prepare_publication()
        .map_err(|failure| descriptor_publication_failure(descriptors, failure))?;
    commit_descriptors(image, descriptors);
    Ok(())
}

fn descriptor_publication_failure(
    descriptors: [Option<DescriptorWrite>; 2],
    failure: AccessFailure,
) -> EventFailure {
    if let Some(write) = descriptors
        .into_iter()
        .flatten()
        .find(|write| write.contains(failure))
    {
        descriptor_failure(write.failure(failure))
    } else {
        EventFailure::Operand {
            segment: Segment::Ss,
            failure,
        }
    }
}

fn commit_descriptors(image: &mut impl GuestMemory, descriptors: [Option<DescriptorWrite>; 2]) {
    for write in descriptors.into_iter().flatten() {
        write.commit(image);
    }
}

fn descriptor_failure(failure: super::Failure) -> EventFailure {
    match failure {
        super::Failure::Protection(failure) => EventFailure::Protection(failure),
        super::Failure::SystemMemory(failure) => EventFailure::SystemMemory(failure),
        super::Failure::SystemPaging(failure) => EventFailure::SystemPaging(failure),
        super::Failure::Operand(_) => unreachable!("unsegmented descriptor access"),
    }
}
