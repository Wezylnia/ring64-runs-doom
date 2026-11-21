use super::{Format, Kind, Op};
use rustc_apfloat::ieee::{Double, IeeeFloat, Semantics, Single};
use rustc_apfloat::{Float, FloatConvert, Round, Status, StatusAnd};

#[cfg(test)]
#[path = "arithmetic_differential.rs"]
mod differential;

struct Wide32;
impl Semantics for Wide32 {
    const BITS: usize = 39;
    const EXP_BITS: usize = 15;
}
struct Wide64;
impl Semantics for Wide64 {
    const BITS: usize = 68;
    const EXP_BITS: usize = 15;
}

pub(super) fn round(mxcsr: u32) -> Round {
    match (mxcsr >> 13) & 3 {
        0 => Round::NearestTiesToEven,
        1 => Round::TowardNegative,
        2 => Round::TowardPositive,
        _ => Round::TowardZero,
    }
}

fn calculate<F: Float>(kind: Kind, a: F, b: F, rc: Round) -> StatusAnd<F> {
    match kind {
        Kind::Add => a.add_r(b, rc),
        Kind::Sub => a.sub_r(b, rc),
        Kind::Mul => a.mul_r(b, rc),
        Kind::Div => a.div_r(b, rc),
        _ => unreachable!(),
    }
}

fn flags<S: Semantics, W: Semantics>(
    bounded: StatusAnd<IeeeFloat<S>>,
    wide: impl FnOnce() -> StatusAnd<IeeeFloat<W>>,
    mxcsr: u32,
) -> (u64, u32) {
    let value = bounded.value.to_bits() as u64;
    let exponent_mask = (1u64 << S::EXP_BITS) - 1;
    let exponent = (value >> (S::PRECISION - 1)) & exponent_mask;
    if (2..exponent_mask - 1).contains(&exponent) {
        // Both formats have the same precision here. An interior normal
        // result cannot have been clipped at either exponent boundary.
        return (
            value,
            u32::from(bounded.status.contains(Status::INEXACT)) << 5,
        );
    }
    if exponent == 0 && bounded.status == Status::OK && value & !(1 << (S::BITS - 1)) != 0 {
        // An exact nonzero subnormal is also exact in the wider format.
        return tiny_flags::<S>(value, false, false, mxcsr);
    }
    range_flags::<S, W>(value, bounded.status, wide(), mxcsr)
}

fn tiny_flags<S: Semantics>(
    value: u64,
    bounded_inexact: bool,
    unbounded_inexact: bool,
    mxcsr: u32,
) -> (u64, u32) {
    if mxcsr & (1 << 11) == 0 {
        (value, 16 | (u32::from(unbounded_inexact) << 5))
    } else if mxcsr & (1 << 15) != 0 {
        (value & (1 << (S::BITS - 1)), 48)
    } else {
        (value, if bounded_inexact { 48 } else { 0 })
    }
}

fn range_flags<S: Semantics, W: Semantics>(
    value: u64,
    bounded_status: Status,
    wide: StatusAnd<IeeeFloat<W>>,
    mxcsr: u32,
) -> (u64, u32) {
    let overflow = wide.value.is_finite()
        && !wide.value.is_zero()
        && wide.value.ilogb() > IeeeFloat::<S>::MAX_EXP;
    let tiny = !wide.value.is_zero()
        && wide.value.is_finite()
        && wide.value.ilogb() < IeeeFloat::<S>::MIN_EXP;
    let unbounded_inexact = wide.status.contains(Status::INEXACT);
    let mut raised = 0;
    if overflow {
        raised |= 8;
        if mxcsr & (1 << 10) != 0 || unbounded_inexact {
            raised |= 32;
        }
    } else if tiny {
        return tiny_flags::<S>(
            value,
            bounded_status.contains(Status::INEXACT),
            unbounded_inexact,
            mxcsr,
        );
    } else if bounded_status.contains(Status::INEXACT) {
        raised |= 32;
    }
    (value, raised)
}

fn arithmetic<S: Semantics, W: Semantics>(
    kind: Kind,
    left: u64,
    right: u64,
    mxcsr: u32,
) -> (u64, u32) {
    let rc = round(mxcsr);
    let a = IeeeFloat::<S>::from_bits(left as u128);
    let b = IeeeFloat::<S>::from_bits(right as u128);
    flags(
        calculate(kind, a, b, rc),
        || {
            let wa: StatusAnd<IeeeFloat<W>> = a.convert_r(rc, &mut false);
            let wb: StatusAnd<IeeeFloat<W>> = b.convert_r(rc, &mut false);
            calculate(kind, wa.value, wb.value, rc)
        },
        mxcsr,
    )
}

fn normal_f32_binary(kind: Kind, left: u32, right: u32, mxcsr: u32) -> Option<(u64, u32)> {
    let left_exp = (left >> 23) & 0xff;
    let right_exp = (right >> 23) & 0xff;
    if !(1..255).contains(&left_exp) || !(1..255).contains(&right_exp) {
        return None;
    }
    // A product needs at most 48 significant bits. For the sum or
    // difference, aligning two 24-bit significands and allowing a carry
    // fits in binary64 at this exponent distance.
    if kind != Kind::Mul && left_exp.abs_diff(right_exp) > 28 {
        return None;
    }
    let a = f64::from(f32::from_bits(left));
    let b = f64::from(f32::from_bits(right));
    let exact = match kind {
        Kind::Add => a + b,
        Kind::Sub => a - b,
        Kind::Mul => a * b,
        _ => unreachable!(),
    };
    let bits = exact.to_bits();
    const BIAS_DELTA: u64 = 1023 - 127;
    let exponent = (bits >> 52) & 0x7ff;
    if !(BIAS_DELTA + 2..BIAS_DELTA + 254).contains(&exponent) {
        // Leave signed cancellation, tiny results and overflow to APFloat.
        return None;
    }
    const DISCARDED: u64 = (1 << (52 - 23)) - 1;
    const HALF: u64 = 1 << (52 - 23 - 1);
    let sign = ((bits >> 32) as u32) & 0x8000_0000;
    let fraction = ((bits >> (52 - 23)) as u32) & 0x7f_ffff;
    let magnitude = ((exponent - BIAS_DELTA) as u32) << 23 | fraction;
    let remainder = bits & DISCARDED;
    if remainder == 0 {
        return Some((u64::from(sign | magnitude), 0));
    }
    let increment = match (mxcsr >> 13) & 3 {
        0 => remainder > HALF || remainder == HALF && fraction & 1 != 0,
        1 => sign != 0,
        2 => sign == 0,
        _ => false,
    };
    Some((u64::from(sign | (magnitude + u32::from(increment))), 32))
}

pub(super) fn binary(kind: Kind, format: Format, left: u64, right: u64, mxcsr: u32) -> (u64, u32) {
    match format {
        Format::F32 => {
            if matches!(kind, Kind::Add | Kind::Sub | Kind::Mul)
                && let Some(result) = normal_f32_binary(kind, left as u32, right as u32, mxcsr)
            {
                return result;
            }
            arithmetic::<rustc_apfloat::ieee::SingleS, Wide32>(kind, left, right, mxcsr)
        }
        Format::F64 => arithmetic::<rustc_apfloat::ieee::DoubleS, Wide64>(kind, left, right, mxcsr),
    }
}

pub(super) fn convert(format: Format, value: u64, mxcsr: u32) -> (u64, u32) {
    let rc = round(mxcsr);
    match format {
        Format::F32 => {
            let a = Single::from_bits(value as u128);
            let result: StatusAnd<Double> = a.convert_r(rc, &mut false);
            (result.value.to_bits() as u64, 0)
        }
        Format::F64 => {
            let a = Double::from_bits(value as u128);
            let bounded: StatusAnd<Single> = a.convert_r(rc, &mut false);
            flags(
                bounded,
                || -> StatusAnd<IeeeFloat<Wide32>> { a.convert_r(rc, &mut false) },
                mxcsr,
            )
        }
    }
}

pub(super) fn from_integer(format: Format, value: i128, mxcsr: u32) -> (u64, u32) {
    let rc = round(mxcsr);
    match format {
        Format::F32 => {
            let r = Single::from_i128_r(value, rc);
            (
                r.value.to_bits() as u64,
                if r.status.contains(Status::INEXACT) {
                    32
                } else {
                    0
                },
            )
        }
        Format::F64 => {
            let r = Double::from_i128_r(value, rc);
            (
                r.value.to_bits() as u64,
                if r.status.contains(Status::INEXACT) {
                    32
                } else {
                    0
                },
            )
        }
    }
}

pub(super) fn to_integer(op: Op, value: u64, mxcsr: u32) -> (u64, u32) {
    let width = if op.scalar && op.integer64 { 64 } else { 32 };
    let rc = if op.kind == Kind::TruncToInt {
        Round::TowardZero
    } else {
        round(mxcsr)
    };
    let r = match op.format {
        Format::F32 => Single::from_bits(value as u128).to_i128_r(width, rc, &mut false),
        Format::F64 => Double::from_bits(value as u128).to_i128_r(width, rc, &mut false),
    };
    if r.status.contains(Status::INVALID_OP) {
        (1u64 << (width - 1), 1)
    } else {
        (
            r.value as u64
                & if width == 32 {
                    u32::MAX as u64
                } else {
                    u64::MAX
                },
            if r.status.contains(Status::INEXACT) {
                32
            } else {
                0
            },
        )
    }
}

pub(super) fn compare(format: Format, a: u64, b: u64) -> std::cmp::Ordering {
    match format {
        Format::F32 => Single::from_bits(a as u128).partial_cmp(&Single::from_bits(b as u128)),
        Format::F64 => Double::from_bits(a as u128).partial_cmp(&Double::from_bits(b as u128)),
    }
    .unwrap()
}

#[cfg(test)]
fn root(value: u128) -> (u128, u128) {
    let mut remainder = value;
    let mut result = 0u128;
    let mut bit = 1u128 << 126;
    while bit > remainder {
        bit >>= 2;
    }
    while bit != 0 {
        if remainder >= result + bit {
            remainder -= result + bit;
            result = (result >> 1) + bit;
        } else {
            result >>= 1;
        }
        bit >>= 2;
    }
    (result, remainder)
}

#[cfg(test)]
pub(super) fn restoring_sqrt(format: Format, value: u64, mxcsr: u32) -> (u64, u32) {
    if format.zero(value) || format.infinite(value) {
        return (value, 0);
    }
    let p = format.fraction();
    let bias = (1i32 << (format.exponent_bits() - 1)) - 1;
    let encoded_exp = ((value >> p) & ((1 << format.exponent_bits()) - 1)) as i32;
    let mut significand = value & format.fraction_mask();
    let mut exponent = if encoded_exp == 0 {
        1 - bias
    } else {
        significand |= 1 << p;
        encoded_exp - bias
    };
    while significand & (1 << p) == 0 {
        significand <<= 1;
        exponent -= 1;
    }
    if exponent & 1 != 0 {
        significand <<= 1;
        exponent -= 1;
    }
    let radicand = (significand as u128) << p;
    let (mut result, remainder) = root(radicand);
    let increment = match (mxcsr >> 13) & 3 {
        0 => remainder > result,
        2 => remainder != 0,
        _ => false,
    };
    if increment {
        result += 1;
    }
    let mut output_exponent = exponent / 2;
    if result == 1u128 << (p + 1) {
        result >>= 1;
        output_exponent += 1;
    }
    let bits = (((output_exponent + bias) as u64) << p) | (result as u64 & format.fraction_mask());
    (bits, if remainder != 0 { 32 } else { 0 })
}

pub(super) fn sqrt(format: Format, value: u64, mxcsr: u32) -> (u64, u32) {
    if format.zero(value) || format.infinite(value) {
        return (value, 0);
    }
    let p = format.fraction();
    let bias = (1i32 << (format.exponent_bits() - 1)) - 1;
    let encoded_exp = ((value >> p) & ((1 << format.exponent_bits()) - 1)) as i32;
    let mut significand = value & format.fraction_mask();
    let mut exponent = if encoded_exp == 0 {
        1 - bias
    } else {
        significand |= 1 << p;
        encoded_exp - bias
    };
    while significand & (1 << p) == 0 {
        significand <<= 1;
        exponent -= 1;
    }
    if exponent & 1 != 0 {
        significand <<= 1;
        exponent -= 1;
    }
    let radicand = (significand as u128) << p;
    let mut rounded = match format {
        Format::F32 => f32::from_bits(value as u32).sqrt().to_bits() as u64,
        Format::F64 => f64::from_bits(value).sqrt().to_bits(),
    };
    let rounded_exp = ((rounded >> p) & ((1 << format.exponent_bits()) - 1)) as i32 - bias;
    let rounded_significand = ((rounded & format.fraction_mask()) | (1 << p)) as u128;
    // The exact integer square decides precision and the adjacent directed result.
    let scaled_significand = rounded_significand << (rounded_exp - exponent / 2);
    let square = scaled_significand * scaled_significand;
    match (mxcsr >> 13) & 3 {
        1 | 3 if square > radicand => rounded -= 1,
        2 if square < radicand => rounded += 1,
        _ => {}
    }
    (rounded, if square == radicand { 0 } else { 32 })
}

pub(super) fn approximate(kind: Kind, format: Format, value: u64) -> u64 {
    let mut result = match format {
        Format::F32 => {
            let x = f32::from_bits(value as u32);
            (1.0f32 / if kind == Kind::Rsqrt { x.sqrt() } else { x }).to_bits() as u64
        }
        Format::F64 => {
            let x = f64::from_bits(value);
            (1.0f64 / if kind == Kind::Rsqrt { x.sqrt() } else { x }).to_bits()
        }
    };
    if kind == Kind::Rcp && format.denormal(result) {
        result &= format.sign();
    }
    result
}
