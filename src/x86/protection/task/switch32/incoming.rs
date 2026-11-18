use super::*;
use crate::x86::context::SegmentCause;
use crate::x86::protection::{far::Loaded, reload};

impl Incoming {
    /// Qualify hidden descriptors under the incoming root. Leave reached caches live
    /// on a guest fault or a retryable missing physical backing.
    pub fn qualify(
        &self,
        context: &mut ExecutionContext,
        image: &mut impl GuestMemory,
        esp: &mut u64,
    ) -> Result<(), Error> {
        let classify = |failure, ldt| match failure {
            Failure::Protection(mut fault) => {
                if fault.vector == 13 || ldt {
                    fault.vector = 10;
                }
                fault.error_code |= self.ext;
                Failure::Protection(fault)
            }
            failure => failure,
        };
        let ldtr = context.ldtr();
        if !ldtr.valid {
            reload::ldt(context, image, ldtr.selector)
                .map_err(|failure| classify(failure, true))?;
        }
        if !context.segment(Segment::Cs).usable {
            self.load_cs(context, image)?;
        }
        for segment in [
            Segment::Ss,
            Segment::Es,
            Segment::Ds,
            Segment::Fs,
            Segment::Gs,
        ] {
            if !context.segment(segment).usable {
                reload::segment(context, image, segment, context.selector(segment))
                    .map_err(|failure| classify(failure, false))?;
            }
        }
        if let Some(code) = self.error_code {
            let mask = context.stack_width().mask();
            let pointer = esp.wrapping_sub(4) & mask;
            let address = context
                .translate(pointer, 4, Segment::Ss, Access::Write, false)
                .map_err(|failure| match failure {
                    AccessFailure::Segment(f) if f.cause == SegmentCause::Limit => {
                        Failure::Protection(ProtectionFault {
                            vector: 12,
                            error_code: self.ext,
                            selector: Some(context.selector(Segment::Ss)),
                            phase: ProtectionPhase::Target,
                        })
                    }
                    other => Failure::Operand(other),
                })?;
            image
                .check_frame_write_aligned(address, 4, context.cpl(), Intent::Natural(4))
                .map_err(Failure::Operand)?;
            image.prepare_publication().map_err(Failure::Operand)?;
            image
                .write(address, 4, u64::from(code))
                .expect("admitted task error-code push");
            *esp = (*esp & !mask) | pointer;
        }
        if self.eip > context.segment(Segment::Cs).limit {
            return Err(Failure::Protection(ProtectionFault {
                vector: 13,
                error_code: self.ext,
                phase: ProtectionPhase::Target,
                selector: None,
            })
            .into());
        }
        Ok(())
    }

    fn load_cs(
        &self,
        context: &mut ExecutionContext,
        image: &mut impl GuestMemory,
    ) -> Result<(), Failure> {
        let selector = context.selector(Segment::Cs);
        let invalid = |phase| fault(selector, self.ext, 10, phase);
        if selector < 4 {
            return Err(invalid(ProtectionPhase::Selector));
        }
        let lookup = Lookup::resolve(context, selector)
            .filter(|l| l.fits(8))
            .ok_or_else(|| invalid(ProtectionPhase::Selector))?;
        let d = Descriptor::decode(lookup.read(context, image, 0)?);
        if !d.code()
            || if d.conforming() {
                d.dpl() > context.cpl()
            } else {
                d.dpl() != context.cpl()
            }
        {
            return Err(invalid(ProtectionPhase::Descriptor));
        }
        if !d.present() {
            return Err(fault(selector, self.ext, 11, ProtectionPhase::Descriptor));
        }
        if !d.accessed() {
            lookup.set_accessed(context, image, d.access)?;
        }
        let loaded = Loaded {
            lookup,
            descriptor: d,
        };
        context.set_segment(Segment::Cs, selector, loaded.cache(), loaded.attributes());
        Ok(())
    }
}
