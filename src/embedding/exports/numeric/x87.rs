use super::*;
use crate::embedding::layout::X87_EXCEPTION;
use crate::x86::context::Mode;
use crate::x86::x87::{self, Op};

#[derive(Default)]
pub(super) struct DecodedOperations([Option<(u32, Op)>; 32]);

impl DecodedOperations {
    fn index(word: u32) -> usize {
        (word.wrapping_mul(0x9e37_79b9) >> 27) as usize
    }

    fn get(&mut self, word: u32) -> Option<Op> {
        let entry = &mut self.0[Self::index(word)];
        if let Some((cached, operation)) = *entry
            && cached == word
        {
            return Some(operation);
        }
        let operation = Op::try_decode(word)?;
        *entry = Some((word, operation));
        Some(operation)
    }
}

fn saved_pointer(context: &ExecutionContext, offset: u64, segment: Segment) -> u64 {
    match context.mode() {
        Mode::Long64 => offset,
        Mode::Real | Mode::Virtual8086 => {
            context.segment_base(segment).wrapping_add(offset) as u32 as u64
        }
        Mode::Protected | Mode::Compatibility => offset as u32 as u64,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_x87(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    core_x87_impl::<false, false>(operation, offset, segment, instruction_pc, flags, rax)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_x87_observed(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    core_x87_impl::<true, false>(operation, offset, segment, instruction_pc, flags, rax)
}

fn core_x87_impl<const OBSERVED: bool, const PAGED: bool>(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    let Some(segment) = Segment::ALL.get(segment as usize).copied() else {
        return -1;
    };
    BRIDGE.with_borrow_mut(|bridge| {
        let Some(operation) = bridge.x87_operations.get(operation) else {
            return -1;
        };
        let core = bridge.core.as_mut().unwrap();
        core.fault = None;
        core.segment_fault = None;
        core.alignment_fault = None;
        core.alignment_check = None;
        core.simd_exception = None;
        core.numeric_fault = None;
        core.general_protection = None;
        core.protection_fault = None;
        core.system_memory_fault = None;
        core.divide_failed = false;
        let pending = unsafe {
            let state = &mut *core.state.get();
            state.target = 0;
            state.fault_segment = Segment::Cs as u32;
            state.x87.pending()
        };
        if operation.is_waiting() && pending != 0 {
            return core.pending_x87_reason();
        }
        let width = operation.memory_bytes();
        let mut address = None;
        let source = if width == 0 {
            0
        } else {
            let access = if operation.is_store() {
                Access::Write
            } else {
                Access::Read
            };
            let Some(linear) = core.segment_address(offset, width, segment, access, false) else {
                return if core.segment_fault.is_some() {
                    SEGMENT
                } else {
                    DATA
                };
            };
            address = Some(linear);
            if operation.is_store() {
                if let Err(fault) = guest_access!(
                    core,
                    PAGED,
                    check_write_aligned(linear, width, operation.alignment_intent())
                ) {
                    core.access_failure(fault);
                    return core.memory_failure_reason();
                }
                0
            } else {
                match guest_access!(
                    core,
                    PAGED,
                    read_vector_aligned(linear, width, operation.alignment_intent())
                ) {
                    Ok(bytes) => u128::from_le_bytes(bytes),
                    Err(fault) => {
                        core.access_failure(fault);
                        return core.memory_failure_reason();
                    }
                }
            }
        };
        // Operands are admitted; only stores still need fallible payload publication.
        let mut staged = operation
            .is_store()
            .then(|| unsafe { (*core.state.get()).x87 });
        let effect = {
            let candidate = match staged.as_mut() {
                Some(candidate) => candidate,
                None => unsafe { &mut (*core.state.get()).x87 },
            };
            let effect = x87::eval(candidate, operation, source, flags);
            if operation.updates_environment() {
                candidate.instruction_pointer =
                    saved_pointer(&core.context, instruction_pc, Segment::Cs);
                if address.is_some() {
                    candidate.data_pointer = saved_pointer(&core.context, offset, segment);
                }
                if candidate.pending() != 0 {
                    candidate.opcode = operation.encoding;
                }
            }
            effect
        };
        if effect.store {
            let linear = address.expect("x87 memory store address");
            if let Err(fault) = guest_access!(
                core,
                PAGED,
                write_bytes_aligned(
                    linear,
                    &effect.value.to_le_bytes()[..width as usize],
                    operation.alignment_intent()
                )
            ) {
                core.access_failure(fault);
                return core.memory_failure_reason();
            }
        }
        unsafe {
            let state = &mut *core.state.get();
            if let Some(candidate) = staged {
                state.x87 = candidate;
            }
            if operation.writes_flags() {
                state.flags = effect.flags;
            }
            if effect.ax {
                state.registers[0] = (rax & !0xffff) | (effect.value as u64 & 0xffff);
            }
        }
        if OBSERVED && let Some(address) = address {
            if !operation.is_store() {
                core.debug_operand(address, width, Access::Read);
            }
            if effect.store {
                core.debug_operand(address, width, Access::Write);
            }
        }
        if address.is_some() && core.image.active_changed() {
            2
        } else {
            0
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_x87(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    core_x87_impl::<false, true>(operation, offset, segment, instruction_pc, flags, rax)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_x87_observed(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    core_x87_impl::<true, true>(operation, offset, segment, instruction_pc, flags, rax)
}

impl Core {
    pub(super) fn pending_x87_reason(&self) -> i32 {
        if self.paging.is_some_and(|bank| bank.cr0 & 0x20 == 0) {
            UNSUPPORTED
        } else {
            X87_EXCEPTION
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_operations_distinguish_colliding_opcodes_and_malformed_words() {
        let first = Op {
            kind: x87::Kind::Load,
            format: x87::Format::F32,
            ..Op::default()
        }
        .encode();
        let index = DecodedOperations::index(first);
        let collision = |bits| {
            (1..0x800)
                .map(|encoding| first | bits | (encoding << 19))
                .find(|&word| DecodedOperations::index(word) == index)
                .unwrap()
        };
        let second = collision(0);
        let malformed = collision(1 << 30);
        let mut cache = DecodedOperations::default();
        for word in [first, first, malformed, second, second, first] {
            assert_eq!(cache.get(word), Op::try_decode(word));
        }
    }
}
