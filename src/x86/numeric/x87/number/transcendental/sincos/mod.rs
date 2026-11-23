use super::{
    Class, Format, INDEFINITE, Num, ONE, Result, SIGN, class, denormal,
    finite::{finish, polynomial, reciprocal},
    nan, normalized, num, round_num,
};
use rustc_apfloat::{Float, ieee::Quad};

// P = PERIOD_GRID × 2^-64; P/2 is one PERIOD_GRID on the reduction grid.
const PERIOD_GRID: u128 = 0x3_243f_6a88_85a3_08d3;
const PHASE_SCALE: u128 = 0x3fff_0000_0000_0000_0000_0614_638b_68ba; // RN binary128 pi/P
const TINY_LIMIT: u128 = 0x3fde_8000_0000_0000_0000; // 2^-33
const RANGE_LIMIT: u128 = 0x403e_8000_0000_0000_0000; // 2^63

const fn coefficients<const N: usize>(start_order: u128) -> [u128; N] {
    let mut coefficients = [0; N];
    let mut order = start_order;
    let mut factorial = start_order; // 1! or 2!
    let mut i = 0;
    while i < N {
        coefficients[i] = reciprocal(factorial) | ((i as u128 & 1) << 127);
        order += 2;
        factorial *= order * (order - 1);
        i += 1;
    }
    coefficients
}

const SINE_RATIO: [u128; 14] = coefficients(1); // sin(t)/t
const COSINE_LOSS: [u128; 14] = coefficients(2); // (1-cos(t))/t^2

fn reduce(x: u128) -> (Quad, u128) {
    let (sig, exponent) = normalized(num(x));
    if exponent >= -1 {
        // On [1/2, 2^63) the binary80 input is exactly representable on
        // the 2^-65 grid, and the largest shift leaves the value in u128.
        let grid = sig << (exponent + 2);
        if grid > PERIOD_GRID / 2 {
            let q = grid / PERIOD_GRID;
            let r = grid % PERIOD_GRID;
            let (q, r) = if r > PERIOD_GRID / 2 {
                (q + 1, r as i128 - PERIOD_GRID as i128)
            } else {
                (q, r as i128)
            };
            // PERIOD_GRID is odd and larger than sig: no tie or zero
            // residual. The exact signed residual fits in binary128.
            let residual = Quad::from_i128(r).value.scalbn(-65);
            return ((residual * Quad::from_bits(PHASE_SCALE)).value, q & 3);
        }
    }
    (Quad::from_u128(sig).value.scalbn(exponent - 63), 0)
}

fn just_below(raw: u128, cw: u16) -> Result {
    // The exact magnitude lies strictly below this raw80 anchor and
    // within half of its preceding ulp. Preserve that side on both grids.
    let (sig, exponent) = normalized(num(raw));
    round_num(
        Num {
            negative: raw & SIGN != 0,
            sig: (sig << 64) - 1,
            scale: exponent - 127,
        },
        64,
        Format::F80,
        cw,
    )
}

fn finite(x: u128, cw: u16) -> (Result, Result) {
    if x & !SIGN <= TINY_LIMIT {
        return (just_below(x, cw), just_below(ONE, cw));
    }
    let (t, quadrant) = reduce(x);
    let z = (t * t).value;
    let s = (t * polynomial(z, &SINE_RATIO)).value;
    // Keep the small loss separate until final subtraction, including
    // close to an axis where the binary128 cosine can round to one.
    let c = (-z)
        .mul_add(polynomial(z, &COSINE_LOSS), Quad::from_u128(1).value)
        .value;
    let (s, c) = match quadrant {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    };
    let s = if x & SIGN != 0 { -s } else { s };
    (finish(s, 0, true, cw), finish(c, 0, true, cw))
}

pub(in crate::x86::numeric::x87) fn sincos_out_of_range(x: u128) -> bool {
    x & !SIGN >= RANGE_LIMIT && class(x) == Class::Finite
}

#[inline(never)]
pub(in crate::x86::numeric::x87) fn sincos(x: u128, cw: u16) -> (Result, u128) {
    if let Some(result) = nan(x, x) {
        return (result, result.value);
    }
    match class(x) {
        Class::Infinity => return (Result::invalid(INDEFINITE), INDEFINITE),
        Class::Zero => return (Result::raw(x), ONE),
        _ => {}
    }
    let pre = if denormal(x) { 2 } else { 0 };
    if pre & !cw != 0 {
        return (
            Result {
                pre,
                ..Result::raw(x)
            },
            x,
        );
    }
    let (mut sine, cosine) = finite(x, cw);
    sine.pre = pre;
    sine.post |= cosine.post;
    (sine, cosine.value)
}

#[cfg(test)]
mod tests;
