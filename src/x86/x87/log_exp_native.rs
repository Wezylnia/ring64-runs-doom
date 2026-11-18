//! x86-64 Linux hardware comparison. FXSAVE captures the producer before a pending #MF.
use super::*;
use core::arch::{asm, x86_64::__cpuid};
use std::io::Write;

const ONE: u128 = 0x3fff8000000000000000;
const HALF: u128 = 0x3ffe8000000000000000;
const TWO: u128 = 0x40008000000000000000;
const SIGN: u128 = 1 << 79;
const INF: u128 = 0x7fff8000000000000000;

#[repr(align(16))]
pub(super) struct Area(pub(super) [u8; 512]);

pub(super) fn hardware(kind: Kind, state: &State) -> Area {
    let mut input = Area([0; 512]);
    input.0[..2].copy_from_slice(&state.control_word.to_le_bytes());
    input.0[2..4].copy_from_slice(&state.status_word.to_le_bytes());
    input.0[24..28].copy_from_slice(&0x1f80u32.to_le_bytes());
    // The FX tag bits are physical, while its 16-byte value slots are relative to TOP.
    for logical in 0..8u8 {
        let physical = state.physical(logical);
        input.0[4] |= u8::from(state.tag(physical) != 3) << physical;
        let at = 32 + usize::from(logical) * 16;
        input.0[at..at + 10].copy_from_slice(&state.registers[physical]);
    }
    let mut host = Area([0; 512]);
    let mut output = Area([0; 512]);
    macro_rules! instruction {
        ($op:literal) => {
            // SAFETY: all FX images are aligned, only valid control bits are loaded,
            // and the original thread context is restored before returning to Rust.
            unsafe { asm!(
                "fxsave64 [{host}]", "fxrstor64 [{input}]", $op,
                "fxsave64 [{output}]", "fxrstor64 [{host}]",
                host = in(reg) host.0.as_mut_ptr(), input = in(reg) input.0.as_ptr(),
                output = in(reg) output.0.as_mut_ptr(), options(nostack, preserves_flags),
            ) }
        }
    }
    match kind {
        Kind::Exp2MinusOne => instruction!("f2xm1"),
        Kind::MulLog2 => instruction!("fyl2x"),
        Kind::MulLog2PlusOne => instruction!("fyl2xp1"),
        Kind::Atan2 => instruction!("fpatan"),
        _ => unreachable!(),
    }
    output
}

// Distance on a single sign's finite binary80 grid. The integer bit is explicit;
// the largest subnormal and smallest normal must also be adjacent.
pub(super) fn rank(raw: u128) -> u128 {
    let exponent = (raw >> 64) & 0x7fff;
    (exponent.saturating_sub(1) << 63) + (raw & u128::from(u64::MAX))
}

#[derive(Clone, Copy)]
enum ResultCheck {
    Exact,
    Approximate,
    ExactMath,
}

fn compare(kind: Kind, x: Option<u128>, y: Option<u128>, cw: u16, check: ResultCheck) -> u16 {
    let mut cpu = State {
        control_word: cw,
        status_word: (7 << 11) | 0x200,
        ..State::default()
    };
    for (i, v) in [x, y, Some(0xbffddecafbad11223344)].into_iter().enumerate() {
        if let Some(raw) = v {
            cpu.put(cpu.physical(i as u8), raw);
        }
    }
    let output = hardware(kind, &cpu);
    let binary = kind != Kind::Exp2MinusOne;
    let op = Op {
        kind,
        format: if binary {
            Format::Register
        } else {
            Format::None
        },
        index: u8::from(binary),
        dest_index: binary,
        pop: u8::from(binary),
        ..Op::default()
    };
    let result = eval(&mut cpu, op, 0, 0x8d7);
    assert_eq!(result.flags, 0x8d7);
    let hw_sw = u16::from_le_bytes([output.0[2], output.0[3]]);
    // C0/C2/C3 are undefined. C1 can reflect the implementation's finite approximation.
    // At mathematical identities native approximate algorithms may additionally raise PE.
    let mask = if hw_sw & 0x40 != 0 || matches!(check, ResultCheck::Exact) && hw_sw & 0xbf == 0 {
        0xbaff // Stack underflow and exact successful non-rounding operations clear C1.
    } else if matches!(check, ResultCheck::ExactMath) {
        0xb8df
    } else {
        0xb8ff
    };
    assert_eq!(
        cpu.status_word & mask,
        hw_sw & mask,
        "{kind:?} x={x:x?} y={y:x?} cw={cw:x} status={hw_sw:x}"
    );
    assert_eq!(output.0[..2], cw.to_le_bytes());
    let new_top = usize::from((hw_sw >> 11) & 7);
    for logical in 0..8 {
        let physical = (new_top + logical) & 7;
        let present = output.0[4] & (1 << physical) != 0;
        assert_eq!(
            cpu.tag(physical) != 3,
            present,
            "{kind:?} physical tag {physical} cw={cw:x}"
        );
        if !present {
            continue;
        }
        let mut raw = [0; 16];
        raw[..10].copy_from_slice(&output.0[32 + logical * 16..42 + logical * 16]);
        let observed = u128::from_le_bytes(raw);
        let actual = cpu.raw(physical);
        if logical == 0 && !matches!(check, ResultCheck::Exact) {
            assert_eq!(actual & SIGN, observed & SIGN, "{kind:?} sign");
            // Two independent bounded approximations can differ by up to two ulps in
            // nearest and three in directed modes. MPFR separately checks the ISA bound.
            let allowance = if cw >> 10 & 3 == 0 { 2 } else { 3 };
            assert!(
                rank(actual).abs_diff(rank(observed)) <= allowance,
                "{kind:?} x={x:x?} y={y:x?} cw={cw:x}: emulator {actual:x}, hardware {observed:x}"
            );
        } else {
            assert_eq!(
                actual, observed,
                "{kind:?} ST({logical}) x={x:x?} y={y:x?} cw={cw:x}"
            );
        }
    }
    hw_sw
}

#[test]
fn transcendental_results_tags_and_exception_publication_match_native_x87() {
    use Kind::{Exp2MinusOne as Exp, MulLog2 as Log, MulLog2PlusOne as Log1p};
    use ResultCheck::{Approximate, Exact};
    let mut packets = 0;
    for (kind, x, y) in [
        (Exp, Some(0), None),
        (Exp, None, None),
        (Exp, Some(SIGN), None),
        (Log, Some(ONE), Some(SIGN | TWO)),
        (Log, Some(0), Some(ONE)),
        (Log, Some(0), Some(INF)),
        (Log, Some(0), Some(0)),
        (Log, Some(ONE | SIGN), Some(ONE)),
        (Log1p, Some(SIGN), Some(ONE)),
        (Log1p, Some(0), Some(INF)),
        (Log, Some(0x7fffa000000000000042), Some(ONE)),
        (Log1p, Some(0x7fffc000000000000042), Some(ONE)),
        (Exp, Some(0x40000000000000000011), None),
        (Log, None, Some(ONE)),
        (Log1p, Some(HALF), None),
    ] {
        for cw in [0x37f, 0x37e, 0x37b] {
            compare(kind, x, y, cw, Exact);
            packets += 1;
        }
    }
    for (kind, x, y) in [
        (Exp, HALF, ONE),
        (Log, ONE + (1 << 62), TWO),
        (Log1p, HALF - (2 << 64), SIGN | TWO),
    ] {
        for pc in [0, 2, 3] {
            for rc in 0..4 {
                for masks in [0x7f, 0x5f] {
                    compare(
                        kind,
                        Some(x),
                        Some(y),
                        masks | (pc << 8) | (rc << 10),
                        Approximate,
                    );
                    packets += 1;
                }
            }
        }
    }
    // Input-denormal unmasked: the old destination and TOP survive, even for a pseudo-denormal.
    for (kind, x, y) in [
        (Exp, 1, ONE),
        (Log, 1, TWO),
        (Log1p, 1, ONE),
        (Log, ONE, 1 << 63),
    ] {
        compare(kind, Some(x), Some(y), 0x37d, Exact);
        packets += 1;
    }
    // Capture exponent-wrapped unmasked result, pop status and deferred exception.
    for (kind, x, y, cw) in [
        (Exp, 1, ONE, 0x36f),
        (Log1p, 1, ONE, 0x36f),
        (Log, 0x4000c000000000000000, 0x7ffeffffffffffffffff, 0x377),
    ] {
        compare(kind, Some(x), Some(y), cw, Approximate);
        packets += 1;
    }
    let _ = writeln!(
        std::io::stderr(),
        "native x87 verified {packets} binary80 log/exp arithmetic, tag and exception packets"
    );
}

#[test]
fn native_x87_records_exact_mathematical_endpoint_precision() {
    use Kind::{Exp2MinusOne as Exp, MulLog2 as Log};
    // These values are exact mathematically. The internal approximation may raise
    // precision. Record each chip's masked and unmasked behavior independently.
    let id = __cpuid(0);
    let vendor = [
        id.ebx.to_le_bytes(),
        id.edx.to_le_bytes(),
        id.ecx.to_le_bytes(),
    ]
    .concat();
    let _ = writeln!(
        std::io::stderr(),
        "native x87 {} family/model/stepping {:x}",
        String::from_utf8_lossy(&vendor),
        __cpuid(1).eax
    );
    for (kind, x) in [(Exp, ONE), (Exp, ONE | SIGN), (Log, TWO), (Log, HALF)] {
        for rc in 0..4 {
            let cw = 0x37f | (rc << 10);
            let masked = compare(kind, Some(x), Some(ONE), cw, ResultCheck::ExactMath);
            let mut state = State {
                control_word: cw & !0x20,
                status_word: 7 << 11,
                ..State::default()
            };
            state.put(7, x);
            state.put(0, ONE);
            let hw = hardware(kind, &state);
            let sw = u16::from_le_bytes([hw.0[2], hw.0[3]]);
            let top = usize::from((sw >> 11) & 7);
            assert_eq!(
                top,
                if kind == Exp { 7 } else { 0 },
                "{kind:?} completed result"
            );
            assert_eq!(
                sw & 0x8080,
                if sw & 0x20 != 0 { 0x8080 } else { 0 },
                "pending PE"
            );
            let _ = writeln!(
                std::io::stderr(),
                "native x87 {kind:?}({x:x},1) RC={rc} masked SW={masked:04x} unmasked SW={sw:04x}"
            );
        }
    }
}
