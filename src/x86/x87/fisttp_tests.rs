use super::*;

fn operand(format: Format) -> Op {
    Op {
        kind: Kind::StoreTruncated,
        format,
        pop: 1,
        ..Op::default()
    }
}

#[test]
fn fisttp_accepts_only_integer_memory_pop_forms() {
    for format in [Format::I16, Format::I32, Format::I64] {
        let op = operand(format);
        assert_eq!(Op::try_decode(op.encode()), Some(op));
        assert!(op.is_waiting() && op.is_store() && op.updates_environment());
        for bad in [
            Op { pop: 0, ..op },
            Op { pop: 2, ..op },
            Op { index: 1, ..op },
            Op {
                dest_index: true,
                ..op
            },
            Op { condition: 1, ..op },
            Op {
                format: Format::Register,
                ..op
            },
            Op {
                format: Format::Bcd,
                ..op
            },
            Op {
                format: Format::F32,
                ..op
            },
        ] {
            assert_eq!(Op::try_decode(bad.encode()), None);
        }
    }
}

#[test]
fn fisttp_chops_before_range_check_independent_of_rounding_control_and_exception_masks() {
    for (format, width) in [(Format::I16, 16), (Format::I32, 32), (Format::I64, 64)] {
        let limit = 1u128 << (width - 1);
        let cutoff = ((0x3fff + width - 1) as u128) << 64 | 1u128 << 63;
        for rc in 0..4 {
            for (raw, mask, status, stored) in [
                (Some(0x3fffe000000000000000), 0x37f, 0x20, Some(1)),
                (
                    Some(0xbfffe000000000000000),
                    0x35f,
                    0x80a0,
                    Some(2 * limit - 1),
                ),
                (Some(1), 0x37d, 0x20, Some(0)),
                (Some(cutoff | (1 << 79)), 0x37e, 0, Some(limit)),
                (Some(cutoff), 0x37f, 1, Some(limit)),
                (Some(cutoff), 0x37e, 0x8081, None),
                (Some(0x7fff8000000000000000), 0x37e, 0x8081, None),
                (Some(0x7fffc000000000000005), 0x37f, 1, Some(limit)),
                (Some(0x3fff0000000000000011), 0x37e, 0x8081, None),
                (None, 0x37f, 0x41, Some(limit)),
                (None, 0x37e, 0x80c1, None),
            ] {
                let cw = mask | rc << 10;
                let mut state = State {
                    control_word: cw,
                    status_word: 0x200,
                    ..State::default()
                };
                if let Some(value) = raw {
                    state.put(0, value);
                }
                let before = state;
                let output = eval(&mut state, operand(format), 0, 0xcd7);
                assert_eq!(output.store.then_some(output.value), stored);
                assert_eq!(
                    state.status_word,
                    status | if output.store { 1 << 11 } else { 0 }
                );
                assert_eq!(state.control_word, cw);
                assert_eq!(state.registers, before.registers);
                assert_eq!(
                    state.tag_word,
                    if output.store {
                        0xffff
                    } else {
                        before.tag_word
                    }
                );
                assert_eq!(output.flags, 0xcd7);
            }
        }
        // One representable step below +2^(width-1) truncates to the largest signed integer.
        let raw = (((0x3fff + width - 2) as u128) << 64) | u128::from(u64::MAX);
        let mut state = State {
            control_word: 0xb7f,
            ..State::default()
        };
        state.put(0, raw);
        let output = eval(&mut state, operand(format), 0, 0);
        assert_eq!(output.store.then_some(output.value), Some(limit - 1));
        assert_eq!(state.status_word, 0x820);
    }
}

// A translated x86 runtime may ignore unmasked x87 exceptions. Compare on x86-64 Linux CI.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn fisttp_matches_native_store_pop_and_deferred_exceptions() {
    use core::arch::asm;
    use std::io::Write;

    #[repr(align(16))]
    struct Area([u8; 512]);

    if !std::is_x86_feature_detected!("sse3") {
        let _ = writeln!(
            std::io::stderr(),
            "native FISTTP oracle skipped: SSE3 unavailable"
        );
        return;
    }
    let mut packets = 0;
    for format in [Format::I16, Format::I32, Format::I64] {
        let op = operand(format);
        for raw in [
            Some(0x3fffe000000000000000),
            Some(0xbfffe000000000000000),
            Some(0x7fff8000000000000000),
            None,
        ] {
            for mask in [0x37f, 0x35f, 0x37e] {
                let mut cpu = State {
                    control_word: mask | 0x800,
                    status_word: 0x200,
                    ..State::default()
                };
                let mut initial = Area([0; 512]);
                initial.0[0..2].copy_from_slice(&cpu.control_word.to_le_bytes());
                initial.0[2..4].copy_from_slice(&cpu.status_word.to_le_bytes());
                initial.0[24..28].copy_from_slice(&0x1f80u32.to_le_bytes());
                if let Some(value) = raw {
                    cpu.put(0, value);
                    initial.0[4] = 1;
                    initial.0[32..42].copy_from_slice(&value.to_le_bytes()[..10]);
                }
                let mut host = Area([0; 512]);
                let mut observed = Area([0; 512]);
                let mut target = [0xa5; 8];
                macro_rules! convert {
                    ($width:literal) => {
                        // SAFETY: all FX images are aligned. FXSAVE/FXRSTOR are non-waiting.
                        // Restore the host environment before calling back into Rust.
                        unsafe { asm!(
                            "fxsave64 [{host}]", "fxrstor64 [{initial}]",
                            concat!("fisttp ", $width, " ptr [{target}]"),
                            "fxsave64 [{observed}]", "fxrstor64 [{host}]",
                            host = in(reg) host.0.as_mut_ptr(), initial = in(reg) initial.0.as_ptr(),
                            observed = in(reg) observed.0.as_mut_ptr(), target = in(reg) target.as_mut_ptr(),
                            options(nostack, preserves_flags),
                        ) }
                    };
                }
                match format {
                    Format::I16 => convert!("word"),
                    Format::I32 => convert!("dword"),
                    Format::I64 => convert!("qword"),
                    _ => unreachable!(),
                }
                let result = eval(&mut cpu, op, 0, 0);
                let mut expected = [0xa5; 8];
                let width = op.memory_bytes() as usize;
                if result.store {
                    expected[..width].copy_from_slice(&result.value.to_le_bytes()[..width]);
                }
                assert_eq!(target, expected, "{format:?} {raw:x?} {mask:x}");
                let hw_status = u16::from_le_bytes(observed.0[2..4].try_into().unwrap());
                assert_eq!(
                    hw_status & 0xbaff,
                    cpu.status_word & 0xbaff,
                    "{format:?} {raw:x?} {mask:x}"
                );
                assert_eq!(observed.0[4] & 1 != 0, cpu.tag(0) != 3);
                assert_eq!(observed.0[0..2], cpu.control_word.to_le_bytes());
                packets += 1;
            }
        }
    }
    let _ = writeln!(
        std::io::stderr(),
        "native FISTTP oracle verified {packets} store, pop and exception packets"
    );
}
