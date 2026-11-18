use super::super::{Format, Kind, Num, Result, SIGN, normalized, num, round_num};
use rustc_apfloat::{Float, ieee::Quad};

const FRACTION: u128 = (1 << 112) - 1;
const QUAD_ONE: u128 = 0x3fff << 112;
// Nearest binary128 values of ln(2) and 1/ln(2).
const LN_2: u128 = 0x3ffe_62e4_2fef_a39e_f357_93c7_6730_07e6;
const LOG2_E: u128 = 0x3fff_7154_7652_b82f_e177_7d0f_fda0_d23a;

// Generate nearest binary128 1/d by integer long division at compile time.
// The largest denominator, 25!, fits in 84 bits.
const fn reciprocal(d: u128) -> u128 {
    let exponent = u128::BITS - (d - 1).leading_zeros();
    let mut remainder = (1 << exponent) - d;
    let mut significand = 1;
    let mut bits = 0;
    while bits < 112 {
        remainder <<= 1;
        significand <<= 1;
        if remainder >= d {
            remainder -= d;
            significand |= 1;
        }
        bits += 1;
    }
    if 2 * remainder > d || (2 * remainder == d && significand & 1 != 0) {
        significand += 1;
    }
    ((0x3fff - exponent as u128) << 112) | (significand & FRACTION)
}

const fn coefficients<const N: usize>(factorial: bool) -> [u128; N] {
    let mut coefficients = [QUAD_ONE; N];
    let mut d = 1;
    let mut i = 1;
    while i < N {
        d = if factorial {
            d * (i + 1) as u128
        } else {
            (2 * i + 1) as u128
        };
        coefficients[i] = reciprocal(d);
        i += 1;
    }
    coefficients
}

const EXP: [u128; 25] = coefficients(true); // 1/(i+1)!
const LOG: [u128; 21] = coefficients(false); // 1/(2i+1)

fn polynomial(x: Quad, coefficients: &[u128]) -> Quad {
    let (highest, lower) = coefficients.split_last().unwrap();
    lower.iter().rev().fold(Quad::from_bits(*highest), |p, &c| {
        p.mul_add(x, Quad::from_bits(c)).value
    })
}

// Signed normalized significand and an independent power of two. This keeps
// the tiniest input alive even if the final multiplier is the largest raw80.
fn parts(raw: u128) -> (Quad, i32) {
    let (sig, exponent) = normalized(num(raw));
    let fraction = ((sig as u64 & !(1 << 63)) as u128) << 49;
    (
        Quad::from_bits(((raw & SIGN) << 48) | QUAD_ONE | fraction),
        exponent,
    )
}

fn finish(value: Quad, scale: i32, irrational: bool, cw: u16) -> Result {
    // All working values here are normal, including when the final result
    // underflows or overflows raw80. One final conversion owns RC and wrapping.
    let mut bits = value.to_bits();
    // Across a power of two, an outward step on the upper grid spans two
    // ulps on the lower grid. If our 2^-96 relative error could have crossed
    // that boundary, round from the last Quad below it. This monotone plateau
    // spans at most 2^-95 of the power and leaves exact products untouched.
    if irrational && bits & FRACTION <= 1 << (112 - 95) {
        bits = (bits & !FRACTION) - 1;
    }
    let sig = (bits & FRACTION) | (1 << 112);
    let exponent = ((bits >> 112) & 0x7fff) as i32 - 0x3fff;
    round_num(
        Num {
            negative: value.is_negative(),
            sig: (sig << 1) | u128::from(irrational),
            scale: scale + exponent - 113,
        },
        64,
        Format::F80,
        cw,
    )
}

// log2(1+x)/x = (2/(2+x)) * log2(e) * atanh(u)/u, u=x/(2+x).
// The caller keeps |u| <= 1/5. The relative omitted odd-series tail is
// at most (1/5)^42 / (43*(1-(1/5)^2)), less than 2^-102.
fn log2p1_ratio(x: Quad) -> Quad {
    let two = Quad::from_bits(QUAD_ONE + (1 << 112));
    let factor = (two / (two + x).value).value;
    let u = (x * factor.scalbn(-1)).value;
    let series = polynomial((u * u).value, &LOG);
    ((series * factor).value * Quad::from_bits(LOG2_E)).value
}

pub(super) fn evaluate(kind: Kind, x: u128, y: u128, cw: u16) -> Result {
    let (mx, ex) = parts(x);
    match kind {
        Kind::Exp2MinusOne => {
            if x & !SIGN == super::ONE {
                return Result::raw(if mx.is_negative() {
                    SIGN | (super::ONE - (1 << 64))
                } else {
                    super::ONE
                });
            }
            let ln2 = Quad::from_bits(LN_2);
            let ratio = if ex < -106 {
                ln2
            } else {
                // For |z| <= ln(2), the relative tail of exprel(z) after
                // z^24/25! is below 2^-100, including negative z.
                let z = (mx.scalbn(ex) * ln2).value;
                (ln2 * polynomial(z, &EXP)).value
            };
            finish((mx * ratio).value, ex, true, cw)
        }
        Kind::MulLog2PlusOne => {
            let (my, ey) = parts(y);
            let ratio = if ex < -106 {
                Quad::from_bits(LOG2_E)
            } else {
                log2p1_ratio(mx.scalbn(ex))
            };
            finish(((mx * my).value * ratio).value, ex + ey, true, cw)
        }
        Kind::MulLog2 => {
            if mx.to_bits() == QUAD_ONE {
                // log2(x) is integral. Its magnitude times a raw80 significand
                // fits in 79 bits, including for a subnormal power of two.
                let mut product = num(y);
                product.negative ^= ex < 0;
                product.sig *= u128::from(ex.unsigned_abs());
                return round_num(product, 64, Format::F80, cw);
            }
            let (my, ey) = parts(y);
            // m in [0.75, 1.5), so m−1 is exact even next to 1 and the
            // integer exponent never cancels a fraction close to ±1.
            let (m, e) = if mx.to_bits() >= QUAD_ONE + (1 << 111) {
                (mx.scalbn(-1), ex + 1)
            } else {
                (mx, ex)
            };
            let delta = (m - Quad::from_bits(QUAD_ONE)).value;
            let log = delta
                .mul_add(log2p1_ratio(delta), Quad::from_i128(e.into()).value)
                .value;
            finish((my * log).value, ey, true, cw)
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
