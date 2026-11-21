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
