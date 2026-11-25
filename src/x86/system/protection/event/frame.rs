use super::*;
use crate::memory::Access;
use crate::x86::access::{EventFrameWrite, EventFrameWriteFailure};

pub(super) struct Frame {
    writes: [EventFrameWrite; 10],
    count: usize,
    cursor: u64,
    raw: u64,
    pointer_width: Width,
    new_stack_error: Option<u16>,
    cpl: u8,
}

impl Frame {
    pub fn new(context: &ExecutionContext, sp: u64, new_stack_error: Option<u16>) -> Self {
        let pointer_width = context.stack_width();
        Self {
            writes: [EventFrameWrite::default(); 10],
            count: 0,
            cursor: sp & pointer_width.mask(),
            raw: sp,
            pointer_width,
            new_stack_error,
            cpl: context.cpl(),
        }
    }

    pub fn push(
        &mut self,
        context: &ExecutionContext,
        value: u64,
        width: Width,
    ) -> Result<(), EventFailure> {
        self.cursor =
            self.cursor.wrapping_sub(u64::from(width.bytes())) & self.pointer_width.mask();
        let address = context
            .translate(
                self.cursor,
                u32::from(width.bytes()),
                Segment::Ss,
                Access::Write,
                false,
            )
            .map_err(|failure| {
                if let Some(error_code) = self.new_stack_error
                    && let AccessFailure::Segment(segment) = failure
                    && segment.cause == crate::x86::context::SegmentCause::Limit
                {
                    return EventFailure::StackLimit {
                        failure: segment,
                        protection: ProtectionFault {
                            vector: 12,
                            error_code,
                            phase: ProtectionPhase::Target,
                            selector: Some(context.selector(Segment::Ss)),
                        },
                    };
                }
                if context.mode() == Mode::Long64
                    && matches!(
                        failure,
                        AccessFailure::Linear(crate::memory::Fault {
                            cause: crate::memory::Cause::Noncanonical,
                            ..
                        })
                    )
                {
                    fault(12, 0, ProtectionPhase::Target, None)
                } else {
                    EventFailure::Operand {
                        segment: Segment::Ss,
                        failure,
                    }
                }
            })?;
        self.writes[self.count] = EventFrameWrite {
            address,
            width: u32::from(width.bytes()),
            value,
        };
        self.count += 1;
        Ok(())
    }

    pub fn publish(
        &self,
        image: &mut impl GuestMemory,
        descriptors: [Option<DescriptorWrite>; 2],
    ) -> Result<(), EventFailure> {
        image
            .write_event_frame(self.cpl, &self.writes[..self.count], |image| {
                commit_descriptors(image, descriptors);
            })
            .map_err(|fault| match fault {
                EventFrameWriteFailure::Admission(failure) => EventFailure::Operand {
                    segment: Segment::Ss,
                    failure,
                },
                EventFrameWriteFailure::Publication {
                    failure,
                    frame_start,
                } => {
                    let descriptor_start =
                        frame_start.saturating_sub(descriptors.iter().flatten().count());
                    descriptor_publication_failure(descriptors, descriptor_start, failure)
                }
            })
    }

    pub fn sp(&self) -> u64 {
        (self.raw & !self.pointer_width.mask()) | self.cursor
    }
}
