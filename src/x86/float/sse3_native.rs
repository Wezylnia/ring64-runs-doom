use super::sse3_tests::{packed, words};
use super::*;
use core::arch::{asm, x86_64::__cpuid};
use std::io::Write;

#[test]
fn native_sse3_packed_arithmetic_matches_result_bits_and_status() {
    if !std::is_x86_feature_detected!("sse3") {
        let _ = writeln!(
            std::io::stderr(),
            "native SSE3 arithmetic oracle skipped: SSE3 unavailable"
        );
        return;
    }
    let nan32 = (
        words([0x7fc00011, 0xffc00022, 0x7f800033, 0xffc00044]),
        words([0xff800055, 0x7fc00066, 0xffc00077, 0x7f800088]),
    );
    let nan64 = (
        0xfff8_0000_0000_0022_7ff8_0000_0000_0011u128,
        0x7ff0_0000_0000_0066_fff8_0000_0000_0055,
    );
    let limits32 = (
        words([0x7f7fffff, 0x7f7fffff, 0x3f800000, 0x33800000]),
        words([0xff800000, 0x7f800000, 1, 0x80000000]),
    );
    let limits64 = (
        0x7fef_ffff_ffff_ffff_7fef_ffff_ffff_ffffu128,
        0x8000_0000_0000_0000_0000_0000_0000_0001,
    );
    for (format, inputs) in [
        (Format::F32, [nan32, limits32]),
        (Format::F64, [nan64, limits64]),
    ] {
        for (left, right) in inputs {
            for kind in [Kind::AddSub, Kind::HorizontalAdd, Kind::HorizontalSub] {
                for control in [0x1f80u32, 0x3f80, 0x5f80, 0x7f80] {
                    let mut saved = 0u32;
                    let mut flags = 0u32;
                    let mut bits = 0u128;
                    macro_rules! execute {
                        ($instruction:literal) => {
                            // SAFETY: feature detection precedes these register-only instructions.
                            // All exceptions are masked, and MXCSR is restored before returning to Rust.
                            unsafe { asm!(
                                "stmxcsr [{saved}]", "ldmxcsr [{control}]",
                                "movdqu xmm0, [{left}]", "movdqu xmm1, [{right}]",
                                concat!($instruction, " xmm0, xmm1"),
                                "movdqu [{bits}], xmm0", "stmxcsr [{flags}]", "ldmxcsr [{saved}]",
                                saved = in(reg) &mut saved, control = in(reg) &control,
                                left = in(reg) &left, right = in(reg) &right,
                                bits = in(reg) &mut bits, flags = in(reg) &mut flags,
                                out("xmm0") _, out("xmm1") _, options(nostack, preserves_flags),
                            ); }
                        };
                    }
                    match (kind, format) {
                        (Kind::AddSub, Format::F32) => execute!("addsubps"),
                        (Kind::AddSub, Format::F64) => execute!("addsubpd"),
                        (Kind::HorizontalAdd, Format::F32) => execute!("haddps"),
                        (Kind::HorizontalAdd, Format::F64) => execute!("haddpd"),
                        (Kind::HorizontalSub, Format::F32) => execute!("hsubps"),
                        (Kind::HorizontalSub, Format::F64) => execute!("hsubpd"),
                        _ => unreachable!(),
                    }
                    let got = eval(packed(kind, format), left, right, control);
                    assert_eq!(
                        (got.value, got.mxcsr),
                        (bits, flags),
                        "{kind:?}/{format:?} MXCSR={control:x} lhs={left:032x} rhs={right:032x}"
                    );
                }
            }
        }
    }
    let id = __cpuid(0);
    let bytes = [
        id.ebx.to_le_bytes(),
        id.edx.to_le_bytes(),
        id.ecx.to_le_bytes(),
    ]
    .concat();
    let vendor = String::from_utf8_lossy(&bytes);
    let _ = writeln!(
        std::io::stderr(),
        "native SSE3 arithmetic oracle verified 48 result and MXCSR packets on {vendor}"
    );
}
