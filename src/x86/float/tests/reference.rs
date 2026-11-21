use super::super::*;

// Every f32 is exact in f64. Round in host arithmetic, then test the signed
// destination's half-open range before the Rust integer cast can saturate.
pub(super) fn integer_conversion(
    op: Op,
    old: u128,
    input: u128,
    mxcsr: u32,
    count: u32,
) -> Outcome {
    let width = if op.integer64 { 64 } else { 32 };
    let limit = 2_f64.powi(width - 1);
    let mut value = 0;
    let mut invalid = false;
    let mut precision = false;
    for index in 0..count {
        let bits = (input >> (index * op.format.bits())) as u64;
        let (number, subnormal) = match op.format {
            Format::F32 => {
                let n = f32::from_bits(bits as u32);
                (f64::from(n), n.is_subnormal())
            }
            Format::F64 => {
                let n = f64::from_bits(bits);
                (n, n.is_subnormal())
            }
        };
        let number = if subnormal && mxcsr & 64 != 0 {
            0.0
        } else {
            number
        };
        let rounded = match (op.kind, (mxcsr >> 13) & 3) {
            (Kind::TruncToInt, _) | (_, 3) => number.trunc(),
            (_, 1) => number.floor(),
            (_, 2) => number.ceil(),
            _ => number.round_ties_even(),
        };
        let integer = if (-limit..limit).contains(&rounded) {
            precision |= number != rounded;
            rounded as i64 as u64
        } else {
            invalid = true;
            1u64 << (width - 1)
        };
        value |= (u128::from(integer) & ((1u128 << width) - 1)) << (index * width as u32);
    }
    let pre_fault = invalid && mxcsr & 0x80 == 0;
    let raised = u32::from(invalid) | (u32::from(precision && !pre_fault) << 5);
    let stage = if pre_fault {
        Some(Stage::PreComputation)
    } else if precision && mxcsr & 0x1000 == 0 {
        Some(Stage::PostComputation)
    } else {
        None
    };
    Outcome {
        value: if stage.is_none() { value } else { old },
        mxcsr: mxcsr | raised,
        raised,
        stage,
    }
}

fn step(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn finite_default_rounding_matches_independent_host_ieee_operations_and_square_roots() {
    let mut seed = 0x3a82fd94bc560127u64;
    for sample in 0..4096 {
        let a = step(&mut seed);
        let b = step(&mut seed);
        for format in [Format::F32, Format::F64] {
            let (a, b) = if format == Format::F32 {
                (a as u32 as u64 & 0xff7fffff, b as u32 as u64 & 0xff7fffff)
            } else {
                (a & 0xffefffffffffffff, b & 0xffefffffffffffff)
            };
            for kind in [Kind::Add, Kind::Sub, Kind::Mul, Kind::Div] {
                let expected = if format == Format::F32 {
                    let x = f32::from_bits(a as u32);
                    let y = f32::from_bits(b as u32);
                    match kind {
                        Kind::Add => x + y,
                        Kind::Sub => x - y,
                        Kind::Mul => x * y,
                        _ => x / y,
                    }
                    .to_bits() as u64
                } else {
                    let x = f64::from_bits(a);
                    let y = f64::from_bits(b);
                    match kind {
                        Kind::Add => x + y,
                        Kind::Sub => x - y,
                        Kind::Mul => x * y,
                        _ => x / y,
                    }
                    .to_bits()
                };
                let operation = Op {
                    kind,
                    format,
                    scalar: true,
                    integer64: false,
                    predicate: 0,
                };
                let r = eval(operation, a as u128, b as u128, 0x1f80);
                assert_eq!(
                    r.value as u64 & format.mask(),
                    expected,
                    "sample{sample} {format:?} {kind:?} {a:x} {b:x}"
                );
            }
            let input = a & !format.sign();
            let expected = if format == Format::F32 {
                f32::from_bits(input as u32).sqrt().to_bits() as u64
            } else {
                f64::from_bits(input).sqrt().to_bits()
            };
            let r = eval(
                Op {
                    kind: Kind::Sqrt,
                    format,
                    scalar: true,
                    integer64: false,
                    predicate: 0,
                },
                0,
                input as u128,
                0x1f80,
            );
            assert_eq!(
                r.value as u64, expected,
                "sample{sample} {format:?} SQRT {input:x}"
            );
        }
    }
}

#[test]
fn native_square_classification_matches_restoring_integer_root_for_every_round() {
    let mut seed = 0xb2f408ca5de76319u64;
    for sample in 0..8192 {
        for format in [Format::F32, Format::F64] {
            let random = step(&mut seed);
            let mut input = if format == Format::F32 {
                random as u32 as u64 & 0x7f7fffff
            } else {
                random & 0x7fefffffffffffff
            };
            if sample % 8 == 0 {
                input &= format.fraction_mask();
            }
            if sample % 8 == 1 {
                input = (input & format.fraction_mask()) | 1 << format.fraction();
            }
            for rc in 0..4 {
                let mxcsr = 0x1f80 | (rc << 13);
                let expected = arithmetic::restoring_sqrt(format, input, mxcsr);
                let actual = arithmetic::sqrt(format, input, mxcsr);
                assert_eq!(
                    actual, expected,
                    "sample{sample} {format:?} SQRT{input:x} rc{rc}"
                );
            }
        }
    }
}
