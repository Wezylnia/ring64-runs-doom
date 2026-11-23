use super::*;
use crate::x86::float::{Outcome, Stage, eval, lane};
use rustc_apfloat::ieee::{DoubleS, SingleS};

fn dual_arithmetic<S: Semantics, W: Semantics>(
    kind: Kind,
    a: u64,
    b: u64,
    mxcsr: u32,
) -> (u64, u32) {
    let rc = round(mxcsr);
    let a = IeeeFloat::<S>::from_bits(u128::from(a));
    let b = IeeeFloat::<S>::from_bits(u128::from(b));
    let wa: StatusAnd<IeeeFloat<W>> = a.convert_r(rc, &mut false);
    let wb: StatusAnd<IeeeFloat<W>> = b.convert_r(rc, &mut false);
    let bounded = calculate(kind, a, b, rc);
    range_flags::<S, W>(
        bounded.value.to_bits() as u64,
        bounded.status,
        calculate(kind, wa.value, wb.value, rc),
        mxcsr,
    )
}

fn dual_convert(value: u64, mxcsr: u32) -> (u64, u32) {
    let a = Double::from_bits(u128::from(value));
    let rc = round(mxcsr);
    let bounded: StatusAnd<Single> = a.convert_r(rc, &mut false);
    let wide: StatusAnd<IeeeFloat<Wide32>> = a.convert_r(rc, &mut false);
    range_flags::<SingleS, Wide32>(bounded.value.to_bits() as u64, bounded.status, wide, mxcsr)
}

fn reference(op: Op, left: u128, right: u128, mxcsr: u32) -> Outcome {
    let bits = op.format.bits();
    let mask = u128::from(op.format.mask());
    let count = if op.scalar { 1 } else { 128 / bits };
    let result_bits = if op.kind == Kind::FloatConvert {
        32
    } else {
        bits
    };
    let inputs: Vec<_> = (0..count)
        .map(|i| {
            lane::prepare(
                op,
                ((left >> (i * bits)) & mask) as u64,
                ((right >> (i * bits)) & mask) as u64,
                mxcsr,
            )
        })
        .collect();
    let pre = inputs.iter().fold(0, |raised, input| raised | input.pre);
    let masks = (mxcsr >> 7) & 63;
    if pre & !masks != 0 {
        return Outcome {
            value: left,
            mxcsr: mxcsr | pre,
            raised: pre,
            stage: Some(Stage::PreComputation),
        };
    }
    let mut raised = pre;
    let mut output = if op.scalar {
        left & !((1u128 << result_bits) - 1)
    } else {
        0
    };
    for (index, input) in inputs.into_iter().enumerate() {
        let (value, post) = if input.resolved {
            (input.value, 0)
        } else if op.kind == Kind::FloatConvert {
            dual_convert(input.right, mxcsr)
        } else {
            match op.format {
                Format::F32 => {
                    dual_arithmetic::<SingleS, Wide32>(op.kind, input.left, input.right, mxcsr)
                }
                Format::F64 => {
                    dual_arithmetic::<DoubleS, Wide64>(op.kind, input.left, input.right, mxcsr)
                }
            }
        };
        raised |= post;
        output |= u128::from(value) << (index as u32 * result_bits);
    }
    let trapped = raised & !masks != 0;
    Outcome {
        value: if trapped { left } else { output },
        mxcsr: mxcsr | raised,
        raised,
        stage: trapped.then_some(Stage::PostComputation),
    }
}

fn check(op: Op, lhs: [u64; 4], rhs: [u64; 4], mxcsr: u32) {
    let pack = |lanes: [u64; 4]| {
        lanes
            .into_iter()
            .take((128 / op.format.bits()) as usize)
            .enumerate()
            .fold(0u128, |value, (i, lane)| {
                value | (u128::from(lane) << (i as u32 * op.format.bits()))
            })
    };
    let left = pack(lhs);
    let right = pack(rhs);
    assert_eq!(
        eval(op, left, right, mxcsr),
        reference(op, left, right, mxcsr),
        "{op:?} mxcsr={mxcsr:04x} left={left:032x} right={right:032x}"
    );
}

#[test]
fn interior_result_uses_only_the_bounded_precision_status() {
    let bounded = StatusAnd {
        status: Status::INEXACT,
        value: Single::from_bits(0x3f800001),
    };
    let result = flags(
        bounded,
        || -> StatusAnd<IeeeFloat<Wide32>> { panic!("interior result evaluated the wide path") },
        0,
    );
    assert_eq!(result, (0x3f800001, 32));
}

#[test]
fn exact_subnormal_uses_only_bounded_value_and_mxcsr() {
    let bounded = Status::OK.and(Single::from_bits(0x807fffff));
    let result = flags(
        bounded,
        || -> StatusAnd<IeeeFloat<Wide32>> { panic!("exact subnormal evaluated the wide path") },
        0x9f80,
    );
    assert_eq!(result, (0x80000000, 0x30));
}

#[test]
fn signed_exact_tiny_results_preserve_flush_and_trap_priority() {
    let high = 0xcafe_d00d_9876_5432u128 << 64;
    for format in [Format::F32, Format::F64] {
        let min = 1u64 << format.fraction();
        let one = ((1u64 << (format.exponent_bits() - 1)) - 1) << format.fraction();
        let half = one - min;
        let op = Op {
            kind: Kind::Mul,
            format,
            scalar: true,
            integer64: false,
            predicate: 0,
        };
        for sign in [0, format.sign()] {
            let left = high | u128::from(sign | min);
            for (rhs, fraction) in [(half, min / 2), (one - 2, min - 1)] {
                for (controls, flush, raised, trapped) in [
                    (0x1f80, false, 0, false),
                    (0x9f80, true, 0x30, false),
                    (0x1780, false, 0x10, true),
                    (0x9780, false, 0x10, true),
                    (0x8f80, false, 0x30, true),
                ] {
                    for sticky in [0, 0x20] {
                        let mxcsr = controls | sticky;
                        let expected = Outcome {
                            value: if trapped {
                                left
                            } else {
                                high | u128::from(sign | if flush { 0 } else { fraction })
                            },
                            mxcsr: mxcsr | raised,
                            raised,
                            stage: trapped.then_some(Stage::PostComputation),
                        };
                        assert_eq!(eval(op, left, u128::from(rhs), mxcsr), expected);
                        assert_eq!(reference(op, left, u128::from(rhs), mxcsr), expected);
                    }
                }
            }
        }
    }
}

#[test]
fn finite_arithmetic_matches_unconditional_widening() {
    let operations = [Kind::Add, Kind::Sub, Kind::Mul, Kind::Div];
    for format in [Format::F32, Format::F64] {
        let sign = format.sign();
        let min = 1 << format.fraction();
        let one = ((1u64 << (format.exponent_bits() - 1)) - 1) << format.fraction();
        let infinity = format.infinity();
        let edges = [
            0,
            sign,
            1,
            sign | 1,
            min - 1,
            min,
            min + 1,
            2 * min,
            one - 1,
            one,
            one + 1,
            sign | (one + 1),
            infinity - min,
            infinity - 2,
            infinity - 1,
            infinity,
            infinity | 1,
            infinity | format.quiet(),
        ];
        let op = |kind, scalar| Op {
            kind,
            format,
            scalar,
            integer64: false,
            predicate: 0,
        };
        for rc in 0..4 {
            for (i, a) in edges.into_iter().enumerate() {
                for (j, b) in edges.into_iter().enumerate() {
                    let daz = ((i + j) & 1) as u32;
                    let ftz = ((i + j) & 2) as u32;
                    let mxcsr = rc << 13 | daz << 6 | ftz << 14 | 0x1f80 | 0x14;
                    for kind in operations {
                        check(op(kind, true), [a, 0, one, sign], [b, one, 1, 0], mxcsr);
                    }
                }
            }
            for masks in 0..64 {
                for controls in 0..4 {
                    for sticky in [0, 63] {
                        let mxcsr = masks << 7
                            | rc << 13
                            | (controls & 1) << 6
                            | (controls & 2) << 14
                            | sticky;
                        for kind in operations {
                            // DIV includes masked and unmasked zero division
                            // next to denormal inputs and potential precision.
                            check(
                                op(kind, false),
                                [one, min, infinity - 1, min],
                                [0, 1, one + 1, one + 1],
                                mxcsr,
                            );
                            check(
                                op(kind, false),
                                [one, infinity - 1, 1, one],
                                [one + 1, one + 1, infinity - 1, 0],
                                mxcsr,
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn narrowing_matches_unconditional_widening() {
    let min = f64::from(f32::MIN_POSITIVE).to_bits();
    let max = f64::from(f32::MAX).to_bits();
    let one = 1f64.to_bits();
    let edges = [
        0,
        1 << 63,
        1,
        min - 1,
        min,
        min + 1,
        one - 1,
        one,
        one + 1,
        max - 1,
        max,
        max + 1,
        f64::INFINITY.to_bits(),
        f64::NAN.to_bits(),
    ];
    for rc in 0..4 {
        for masks in 0..64 {
            for (i, value) in edges.into_iter().enumerate() {
                let op = Op {
                    kind: Kind::FloatConvert,
                    format: Format::F64,
                    scalar: i % 2 == 0,
                    integer64: false,
                    predicate: 0,
                };
                let mxcsr = rc << 13
                    | masks << 7
                    | (i as u32 & 1) << 6
                    | (i as u32 & 2) << 14
                    | (i as u32 & 63);
                check(
                    op,
                    [one, max, 0, 0],
                    [value, edges[(i + 5) % edges.len()], 0, 0],
                    mxcsr,
                );
            }
        }
    }
}

#[test]
fn varied_bit_patterns_match_unconditional_widening() {
    let mut seed = 0x5eed_6d69_6e6d_6178u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for format in [Format::F32, Format::F64] {
        let operations = [
            Kind::Add,
            Kind::Sub,
            Kind::Mul,
            Kind::Div,
            Kind::FloatConvert,
        ];
        let count = if format == Format::F32 { 4 } else { 5 };
        for index in 0..12_000 {
            let op = Op {
                kind: operations[index % count],
                format,
                scalar: index & 1 == 0,
                integer64: false,
                predicate: 0,
            };
            let left = std::array::from_fn(|_| next() & format.mask());
            let right = std::array::from_fn(|_| next() & format.mask());
            check(op, left, right, next() as u32 & 0xffff);
        }
    }
}

#[test]
fn exact_f64_widening_rounds_f32_normal_results_in_each_direction() {
    let one = 0x3f80_0000;
    let half_ulp = 0x3380_0000;
    let sign = 0x8000_0000;
    for (kind, left, right, expected) in [
        (Kind::Add, one, half_ulp, [one, one, one + 1, one]),
        (
            Kind::Add,
            one + 1,
            half_ulp,
            [one + 2, one + 1, one + 2, one + 1],
        ),
        (
            Kind::Add,
            0x3fff_ffff,
            half_ulp,
            [0x4000_0000, 0x3fff_ffff, 0x4000_0000, 0x3fff_ffff],
        ),
        (
            Kind::Add,
            0x7eff_ffff,
            0x7280_0000,
            [0x7f00_0000, 0x7eff_ffff, 0x7f00_0000, 0x7eff_ffff],
        ),
        (
            Kind::Sub,
            sign | one,
            half_ulp,
            [sign | one, sign | (one + 1), sign | one, sign | one],
        ),
        (
            Kind::Mul,
            one + 1,
            one + 1,
            [one + 2, one + 2, one + 3, one + 2],
        ),
        (
            Kind::Mul,
            one + 1,
            0x3fc0_0000,
            [0x3fc0_0002, 0x3fc0_0001, 0x3fc0_0002, 0x3fc0_0001],
        ),
        (
            Kind::Mul,
            sign | (one + 3),
            0x3fc0_0000,
            [0xbfc0_0004, 0xbfc0_0005, 0xbfc0_0004, 0xbfc0_0004],
        ),
    ] {
        for (rc, bits) in expected.into_iter().enumerate() {
            let mxcsr = (rc as u32) << 13;
            assert_eq!(
                normal_f32_binary(kind, left, right, mxcsr),
                Some((u64::from(bits), 32))
            );
            assert_eq!(
                dual_arithmetic::<SingleS, Wide32>(kind, u64::from(left), u64::from(right), mxcsr),
                (u64::from(bits), 32)
            );
        }
    }
    assert_eq!(
        normal_f32_binary(Kind::Mul, one, one, 0),
        Some((u64::from(one), 0))
    );
}

#[test]
fn normal_f32_widening_keeps_unproved_endpoints_and_signed_zero_checked() {
    let one = 0x3f80_0000;
    for (kind, left, right) in [
        (Kind::Add, one, 0x3100_0000),         // exponent difference 29
        (Kind::Sub, one, one),                 // rounding-dependent cancellation sign
        (Kind::Mul, 0x0080_0000, one),         // minimum normal
        (Kind::Mul, 0x7f7f_ffff, one),         // maximum finite
        (Kind::Mul, 0x0080_0000, 0x3f00_0000), // exact subnormal
        (Kind::Mul, 1, one),                   // subnormal operand
        (Kind::Mul, 0x7f80_0000, one),         // infinity
    ] {
        assert_eq!(normal_f32_binary(kind, left, right, 0), None);
    }
}
