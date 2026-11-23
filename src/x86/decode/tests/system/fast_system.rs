use super::super::*;
use crate::x86::context::{ExecutionContext, Mode};
use crate::x86::fast_system::Instruction as Fast;

fn decoded(
    bytes: &[u8],
    paged: bool,
    context: &ExecutionContext,
) -> Result<Instruction, DecodeError> {
    crate::x86::decode::decode_with_profile(0x1000, context, paged, |pc| {
        bytes
            .get((pc - 0x1000) as usize)
            .copied()
            .ok_or(DecodeError::Fetch(missing(pc)))
    })
}

#[test]
fn fast_transfer_widths_ignore_redundant_prefixes_and_never_invent_a_word_return() {
    let context = ExecutionContext::default();
    for (bytes, expected) in [
        (&[0x0f, 0x05][..], Fast::Syscall),
        (&[0xf3, 0x66, 0x67, 0x65, 0x0f, 0x05][..], Fast::Syscall),
        (&[0x66, 0x0f, 0x07][..], Fast::Sysret { wide: false }),
        (
            &[0xf2, 0x66, 0x48, 0x0f, 0x07][..],
            Fast::Sysret { wide: true },
        ),
        (&[0x0f, 0x34][..], Fast::Sysenter),
        (&[0xf2, 0x66, 0x0f, 0x35][..], Fast::Sysexit { wide: false }),
        (&[0x66, 0x48, 0x0f, 0x35][..], Fast::Sysexit { wide: true }),
        (&[0x0f, 0x01, 0xf8][..], Fast::Swapgs),
        (&[0xf3, 0x66, 0x4f, 0x0f, 0x01, 0xf8][..], Fast::Swapgs),
    ] {
        let instruction = decoded(bytes, true, &context).unwrap();
        assert_eq!(instruction.operation, Operation::FastSystem(expected));
        assert_eq!(instruction.next_pc, 0x1000 + bytes.len() as u64);
        assert!(matches!(
            decoded(&bytes[..bytes.len() - 1], true, &context),
            Err(DecodeError::Fetch(_))
        ));
    }
}

#[test]
fn paged_fast_legality_does_not_change_linear_frontiers_or_legacy_rex_interpretation() {
    let mut context = ExecutionContext::default();
    for bytes in [
        &[0x0f, 0x05][..],
        &[0x0f, 0x07],
        &[0x0f, 0x34],
        &[0x0f, 0x35],
        &[0x0f, 0x01, 0xf8],
    ] {
        assert_eq!(
            decoded(bytes, false, &context),
            Err(DecodeError::Unsupported)
        );
        let mut locked = vec![0xf0];
        locked.extend_from_slice(bytes);
        assert_eq!(
            decoded(&locked, true, &context),
            Err(DecodeError::UndefinedEncoding)
        );
    }
    context.set_mode_and_cpl(Mode::Compatibility, 0);
    for bytes in [&[0x0f, 0x05][..], &[0x0f, 0x07], &[0x0f, 0x01, 0xf8]] {
        assert_eq!(
            decoded(bytes, true, &context),
            Err(DecodeError::UndefinedEncoding)
        );
    }
    assert_eq!(
        decoded(&[0x66, 0x0f, 0x35], true, &context)
            .unwrap()
            .operation,
        Operation::FastSystem(Fast::Sysexit { wide: false })
    );
    assert!(!matches!(
        decoded(&[0x48, 0x0f, 0x35], true, &context)
            .unwrap()
            .operation,
        Operation::FastSystem(_)
    ));
}

#[test]
fn segment_base_uses_the_raw_selector_and_effective_rex_for_32_or_64_bit_gprs() {
    use crate::x86::context::Segment::{Fs, Gs};
    let context = ExecutionContext::default();
    for (selector, segment, write) in [(0, Fs, false), (1, Gs, false), (2, Fs, true), (3, Gs, true)]
    {
        for low in 0..8 {
            for rex in 0x40..=0x4f {
                let bytes = [0xf3, rex, 0x0f, 0xae, 0xc0 | selector << 3 | low];
                let instruction = decoded(&bytes, true, &context).unwrap();
                assert_eq!(instruction.encoded_len(), 5);
                assert_eq!(
                    instruction.operation,
                    Operation::SegmentBase {
                        segment,
                        register: low | (rex & 1) << 3,
                        write,
                        width: if rex & 8 != 0 { Width::W64 } else { Width::W32 },
                    }
                );
            }
        }
    }
    for (prefix, register, width) in [
        (&[0xf3][..], 4, Width::W32),
        (&[0xf3, 0x66], 4, Width::W32),
        (&[0x66, 0xf3], 4, Width::W32),
        (&[0xf3, 0x49, 0x67], 4, Width::W32),
        (&[0x49, 0xf3], 4, Width::W32),
        (&[0x66, 0x65, 0x67, 0xf3, 0x49], 12, Width::W64),
    ] {
        let code = [prefix, &[0x0f, 0xae, 0xdc]].concat();
        assert_eq!(
            decoded(&code, true, &context).unwrap().operation,
            Operation::SegmentBase {
                segment: Gs,
                register,
                width,
                write: true
            }
        );
    }
}

#[test]
fn segment_base_requires_long64_and_a_control_bank_after_fetching_the_register() {
    let mut context = ExecutionContext::default();
    for mode in [
        Mode::Long64,
        Mode::Compatibility,
        Mode::Protected,
        Mode::Real,
        Mode::Virtual8086,
    ] {
        context.set_mode_and_cpl(mode, if mode == Mode::Virtual8086 { 3 } else { 0 });
        for paged in [false, true] {
            let bytes = [0xf3, 0x0f, 0xae, 0xc5];
            assert_eq!(
                decoded(&bytes[..3], paged, &context),
                Err(DecodeError::Fetch(missing(0x1003)))
            );
            let result = decoded(&bytes, paged, &context);
            if mode == Mode::Long64 && paged {
                assert!(matches!(
                    result.unwrap().operation,
                    Operation::SegmentBase { .. }
                ));
            } else {
                assert_eq!(
                    result,
                    Err(DecodeError::UndefinedEncoding),
                    "{mode:?} paged={paged}"
                );
            }
        }
    }
}

#[test]
fn segment_base_classifies_lock_and_reserved_memory_only_after_required_fetches() {
    let context = ExecutionContext::default();
    for prefix in [&[0xf3][..], &[0xf0, 0xf3], &[0xf3, 0xf0]] {
        for selector in 0..4 {
            for (tail, memory) in [
                (vec![0xc0 | selector << 3], false),
                (vec![0x05 | selector << 3, 1, 2, 3, 4], true),
                (vec![0x84 | selector << 3, 0x8b, 1, 2, 3, 4], true),
            ] {
                let code = [prefix, &[0x0f, 0xae], &tail].concat();
                for len in 0..code.len() {
                    assert_eq!(
                        decoded(&code[..len], true, &context),
                        Err(DecodeError::Fetch(missing(0x1000 + len as u64))),
                        "{code:02x?}/{len}"
                    );
                }
                let result = decoded(&code, true, &context);
                if memory {
                    assert_eq!(result, Err(DecodeError::Invalid));
                } else if prefix.contains(&0xf0) {
                    assert_eq!(result, Err(DecodeError::UndefinedEncoding));
                } else {
                    assert!(result.is_ok());
                }
            }
        }
        for selector in 4..8 {
            for modrm in [0x84, 0xc0] {
                let code = [prefix, &[0x0f, 0xae, modrm | selector << 3]].concat();
                assert_eq!(
                    decoded(&code, true, &context),
                    Err(DecodeError::Unsupported)
                );
            }
        }
    }
    for prefix in [&[0xf0, 0xf3][..], &[0xf3, 0xf0]] {
        for neighbor in [&[0xcd][..], &[0x0f, 0x38], &[0x0f, 0xae, 0x24]] {
            let bytes = [prefix, neighbor].concat();
            assert_eq!(
                decoded(&bytes, true, &context),
                Err(DecodeError::Unsupported)
            );
        }
    }
    for prefix in [0x66, 0xf2] {
        assert_eq!(
            decoded(&[prefix, 0x0f, 0xae], true, &context),
            Err(DecodeError::Unsupported)
        );
    }
    let mut allowed = vec![0x67; 11];
    allowed.extend([0xf3, 0x0f, 0xae, 0xc0]);
    assert_eq!(decoded(&allowed, true, &context).unwrap().encoded_len(), 15);
    allowed.insert(0, 0x67);
    assert_eq!(
        decoded(&allowed, true, &context),
        Err(DecodeError::InstructionLength)
    );
}

#[test]
fn segment_base_reserved_memory_fetch_uses_the_selected_address_size_in_legacy_code() {
    for mode in ["protected", "compatibility"] {
        for code32 in [false, true] {
            let context = mode_context(mode, code32);
            for overridden in [false, true] {
                let mut bytes = vec![0xf3];
                if overridden {
                    bytes.push(0x67);
                }
                bytes.extend([0x0f, 0xae, 0x84]); // mod=10, r/m=4
                if code32 != overridden {
                    bytes.extend([0x24, 1, 2, 3, 4]); // SIB + disp32
                } else {
                    bytes.extend([1, 2]); // SI + disp16
                }
                for length in 0..bytes.len() {
                    assert_eq!(
                        decoded(&bytes[..length], true, &context),
                        Err(DecodeError::Fetch(missing(0x1000 + length as u64)))
                    );
                }
                assert_eq!(decoded(&bytes, true, &context), Err(DecodeError::Invalid));
            }
        }
    }
}
