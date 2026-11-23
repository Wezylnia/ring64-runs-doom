use super::{
    Format, INFINITY, Num, Result, SIGN, denormal,
    finite::{finish, polynomial, reciprocal},
    nan, normalized, num, round_num,
};
use rustc_apfloat::{Float, ieee::Quad};

const QUAD_EXPONENT: u128 = 1 << 112;
const QUAD_PI: u128 = 0x4000_921f_b544_42d1_8469_898c_c517_01b8;
const HALF_PI: u128 = QUAD_PI - QUAD_EXPONENT;
const QUARTER_PI: u128 = QUAD_PI - 2 * QUAD_EXPONENT;

const fn coefficients() -> [u128; 40] {
    let mut terms = [0; 40];
    let mut i = 0;
    while i < terms.len() {
        terms[i] = reciprocal((2 * i + 1) as u128) | ((i as u128 & 1) << 127);
        i += 1;
    }
    terms
}

const ATAN: [u128; 40] = coefficients();

fn series(t: Quad) -> Quad {
    let exponent = ((t.to_bits() >> 112) as i32 & 0x7fff) - 0x3fff;
    // The reduced argument is at most 27/64. Every truncation has
    // relative error below 2^-104, from t^(2n)/(2n+1).
    let terms = match exponent {
        ..=-17 => 4,
        -16..=-9 => 7,
        -8..=-5 => 13,
        -4..=-3 => 26,
        _ => ATAN.len(),
    };
    (t * polynomial((t * t).value, &ATAN[..terms])).value
}

struct Ratio {
    numerator: u128,
    denominator: u128,
    exponent: i32,
}

impl Ratio {
    fn new((low_e, low): (i32, u128), (high_e, high): (i32, u128)) -> Self {
        let shift = u32::from(low < high);
        Self {
            numerator: low << shift,
            denominator: high,
            exponent: low_e - high_e - shift as i32,
        }
    }

    fn angle(&self) -> Quad {
        // A correction this small cannot affect binary128 reconstruction.
        if self.exponent < -120 {
            return Quad::ZERO;
        }
        if self.exponent >= -2 && (self.numerator << 6) > (27 * self.denominator) << -self.exponent
        {
            // Form the difference in the raw80 input scale before division.
            let high = self.denominator << -self.exponent;
            let difference = Quad::from_u128(high - self.numerator).value;
            let sum = Quad::from_u128(high + self.numerator).value;
            return (Quad::from_bits(QUARTER_PI) - series((difference / sum).value)).value;
        }
        let numerator = Quad::from_u128(self.numerator).value;
        let denominator = Quad::from_u128(self.denominator).value;
        let ratio = (numerator / denominator).value.scalbn(self.exponent);
        if self.exponent <= -65 {
            ratio
        } else {
            series(ratio)
        }
    }

    fn tiny_angle(&self, negative: bool, cw: u16) -> Result {
        // In this range atan(t) is strictly below t. The correction is too
        // small to cross a rounding boundary unless t is exactly there.
        let high = (self.numerator - self.denominator) << 64;
        let low = (high % self.denominator) << 63;
        let significand = (1 << 127) | ((high / self.denominator) << 63) | (low / self.denominator);
        let sig = if low.is_multiple_of(self.denominator) {
            significand - 1
        } else {
            significand | 1
        };
        round_num(
            Num {
                negative,
                sig,
                scale: self.exponent - 127,
            },
            64,
            Format::F80,
            cw,
        )
    }
}

fn finite(x: u128, y: u128, cw: u16) -> Result {
    let ay = y & !SIGN;
    let ax = x & !SIGN;
    let negative_x = x & SIGN != 0;
    let negative_y = y & SIGN != 0;
    let pi = Quad::from_bits(QUAD_PI);
    let half_pi = Quad::from_bits(HALF_PI);
    let quarter_pi = Quad::from_bits(QUARTER_PI);
    let magnitude = if ay == 0 {
        if !negative_x {
            return Result::raw(y & SIGN);
        }
        pi
    } else if ay == INFINITY {
        if ax != INFINITY {
            half_pi
        } else if negative_x {
            (half_pi + quarter_pi).value
        } else {
            quarter_pi
        }
    } else if ax == 0 {
        half_pi
    } else if ax == INFINITY {
        if !negative_x {
            return Result::raw(y & SIGN);
        }
        pi
    } else {
        let (sx, ex) = normalized(num(x));
        let (sy, ey) = normalized(num(y));
        let x = (ex, sx);
        let y = (ey, sy);
        match (x.cmp(&y), negative_x) {
            (std::cmp::Ordering::Equal, false) => quarter_pi,
            (std::cmp::Ordering::Equal, true) => (half_pi + quarter_pi).value,
            (std::cmp::Ordering::Less, false) => (half_pi - Ratio::new(x, y).angle()).value,
            (std::cmp::Ordering::Less, true) => (half_pi + Ratio::new(x, y).angle()).value,
            (std::cmp::Ordering::Greater, true) => (pi - Ratio::new(y, x).angle()).value,
            (std::cmp::Ordering::Greater, false) => {
                let ratio = Ratio::new(y, x);
                if ratio.exponent <= -65 {
                    return ratio.tiny_angle(negative_y, cw);
                }
                ratio.angle()
            }
        }
    };
    let signed = if negative_y { -magnitude } else { magnitude };
    finish(signed, 0, true, cw)
}

#[inline(never)]
pub(in crate::x86::x87) fn atan2(x: u128, y: u128, cw: u16) -> Result {
    if let Some(result) = nan(x, y) {
        return result;
    }
    let pre = if denormal(x) || denormal(y) { 2 } else { 0 };
    if pre & !cw != 0 {
        return Result {
            pre,
            ..Result::raw(x)
        };
    }
    let mut result = finite(x, y, cw);
    if result.value & !SIGN != 0 || result.post != 0 {
        result.post |= 32;
    }
    result.pre = pre;
    result
}

#[cfg(test)]
mod tests;
