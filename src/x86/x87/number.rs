mod special;
mod transcendental;
use super::{Format, Kind};
pub(super) use special::{extract, remainder, scale};
use std::cmp::Ordering;
pub(super) use transcendental::{atan2, sincos, sincos_out_of_range, transcendental};
pub(super) const SIGN: u128 = 1 << 79;
pub(super) const QUIET: u128 = 1 << 62;
pub(super) const INDEFINITE: u128 = 0xffffc000000000000000;
pub(super) const INFINITY: u128 = 0x7fff8000000000000000;
#[derive(Clone, Copy, Debug)]
pub(super) struct Num {
    pub negative: bool,
    pub sig: u128,
    pub scale: i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Class {
    Zero,
    Finite,
    Infinity,
    QuietNan,
    SignalingNan,
    Unsupported,
}
pub(super) fn class(raw: u128) -> Class {
    let e = (raw >> 64) & 0x7fff;
    let s = raw as u64;
    if e == 0 {
        if s == 0 { Class::Zero } else { Class::Finite }
    } else if s >> 63 == 0 {
        Class::Unsupported
    } else if e != 0x7fff {
        Class::Finite
    } else if s == 1 << 63 {
        Class::Infinity
    } else if s & (1 << 62) != 0 {
        Class::QuietNan
    } else {
        Class::SignalingNan
    }
}
pub(super) fn denormal(raw: u128) -> bool {
    raw & 0x7fff0000000000000000 == 0 && raw as u64 != 0
}
pub(super) fn tag(raw: u128) -> u16 {
    let e = (raw >> 64) & 0x7fff;
    let s = raw as u64;
    if e == 0 && s == 0 {
        1
    } else if e == 0 || e == 0x7fff || s >> 63 == 0 {
        2
    } else {
        0
    }
}
pub(super) fn num(raw: u128) -> Num {
    let e = ((raw >> 64) & 0x7fff) as i32;
    Num {
        negative: raw & SIGN != 0,
        sig: (raw as u64) as u128,
        scale: if e == 0 { -16382 - 63 } else { e - 16383 - 63 },
    }
}
fn normalized(n: Num) -> (u128, i32) {
    // Operands decoded from binary80 and the rounded integer of FRNDINT fit in one word.
    debug_assert!(n.sig <= u128::from(u64::MAX));
    let sig = n.sig as u64;
    if sig == 0 {
        return (0, 0);
    }
    let shift = sig.leading_zeros();
    (u128::from(sig << shift), n.scale + (63 - shift as i32))
}

#[cfg(test)]
#[test]
fn one_word_normalization_matches_wide_shift_at_every_bit_position() {
    for scale in [-16445, -1, 0, 16320] {
        let input = Num {
            negative: false,
            sig: 0,
            scale,
        };
        assert_eq!(normalized(input), (0, 0));
        for bit in 0..64 {
            let leading = 1u128 << bit;
            let lower = leading - 1;
            for sig in [leading, leading | (lower / 2), leading | lower] {
                let index = 127 - sig.leading_zeros() as i32;
                let expected = (sig << (63 - index), scale + index);
                assert_eq!(normalized(Num { sig, ..input }), expected);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Result {
    pub value: u128,
    pub pre: u16,
    pub post: u16,
    pub up: bool,
    pub suppress: bool,
}
impl Result {
    pub fn raw(value: u128) -> Self {
        Self {
            value,
            pre: 0,
            post: 0,
            up: false,
            suppress: false,
        }
    }
    pub fn invalid(value: u128) -> Self {
        Self {
            value,
            pre: 1,
            ..Self::raw(value)
        }
    }
}
fn rc(cw: u16) -> u16 {
    (cw >> 10) & 3
}
pub(super) fn precision(cw: u16) -> u32 {
    match (cw >> 8) & 3 {
        0 => 24,
        2 => 53,
        _ => 64,
    }
}
fn quantize(sig: u128, shift: i32, negative: bool, round: u16) -> (u128, bool, bool) {
    if shift <= 0 {
        return (sig << (-shift), false, false);
    }
    let shift = shift as u32;
    // Keep the halfway bit next to the quotient and detect lower bits via ctz.
    let with_round_bit = if shift <= 128 { sig >> (shift - 1) } else { 0 };
    let q = with_round_bit >> 1;
    let trailing = sig.trailing_zeros();
    let inexact = sig != 0 && trailing < shift;
    let up = inexact
        && match round {
            0 => with_round_bit & 1 != 0 && (trailing < shift - 1 || q & 1 != 0),
            1 => negative,
            2 => !negative,
            _ => false,
        };
    (q + u128::from(up), inexact, up)
}
#[cfg(test)]
#[test]
fn rounding_bits_at_and_beyond_the_full_significand_width() {
    let half = 1u128 << 127;
    assert_eq!(quantize(half, 128, false, 0), (0, true, false));
    assert_eq!(quantize(half + 1, 128, false, 0), (1, true, true));
    assert_eq!(quantize(half + 1, 129, false, 0), (0, true, false));
    assert_eq!(quantize(half, 129, false, 2), (1, true, true));
    assert_eq!(quantize(half, 129, true, 1), (1, true, true));
    assert_eq!(quantize(0, 129, true, 1), (0, false, false));
    assert_eq!(quantize(3u128 << 63, 64, false, 0), (2, true, true));
    assert_eq!(quantize(5u128 << 63, 64, false, 0), (2, true, false));
}

fn pack(negative: bool, q: u64, e: i32, p: u32, target: Format, normal: bool) -> u128 {
    if target == Format::F32 || target == Format::F64 {
        let (sign_bit, bias) = if target == Format::F32 {
            (31, 127)
        } else {
            (63, 1023)
        };
        let sign = u64::from(negative) << sign_bit;
        let payload = if normal {
            (((e + bias) as u64) << (p - 1)) | (q - (1u64 << (p - 1)))
        } else {
            q
        };
        u128::from(sign | payload)
    } else {
        let exponent = if normal { (e + 16383) as u16 } else { 0 };
        let sign_exponent = (u16::from(negative) << 15) | exponent;
        (u128::from(sign_exponent) << 64) | u128::from(q << (64 - p))
    }
}
fn round_num(n: Num, p: u32, target: Format, cw: u16) -> Result {
    if n.sig == 0 {
        return Result::raw(pack(n.negative, 0, 0, p, target, false));
    }
    let emin = if target == Format::F32 {
        -126
    } else if target == Format::F64 {
        -1022
    } else {
        -16382
    };
    let emax = if target == Format::F32 {
        127
    } else if target == Format::F64 {
        1023
    } else {
        16383
    };
    let b = 127 - n.sig.leading_zeros() as i32;
    let mut e = n.scale + b;
    let (mut q, inexact, up) = quantize(n.sig, b + 1 - p as i32, n.negative, rc(cw));
    // Quantizing to p bits can round up to exactly 2^p. At full precision
    // this is the only result with a nonzero high word.
    let carry = if p == 64 {
        q >> 64 != 0
    } else {
        q as u64 == 1u64 << p
    };
    if carry {
        q >>= 1;
        e += 1
    }
    debug_assert!(q < 1u128 << p);
    let q = q as u64;
    if e > emax {
        let masked = cw & 8 != 0;
        let post = 8 | if masked || inexact { 32 } else { 0 };
        if !masked {
            return Result {
                value: if target == Format::F80 {
                    if e - 24576 > emax {
                        INFINITY | if n.negative { SIGN } else { 0 }
                    } else {
                        pack(n.negative, q, e - 24576, p, target, true)
                    }
                } else {
                    0
                },
                pre: 0,
                post,
                up,
                suppress: target != Format::F80,
            };
        }
        let infinity = rc(cw) == 0 || rc(cw) == 1 && n.negative || rc(cw) == 2 && !n.negative;
        let value = if infinity {
            if target == Format::F32 {
                0x7f800000 | if n.negative { 1 << 31 } else { 0 }
            } else if target == Format::F64 {
                0x7ff0000000000000 | if n.negative { 1 << 63 } else { 0 }
            } else {
                INFINITY | if n.negative { SIGN } else { 0 }
            }
        } else {
            pack(n.negative, u64::MAX >> (64 - p), emax, p, target, true)
        };
        return Result {
            value,
            pre: 0,
            post,
            up: infinity,
            suppress: false,
        };
    }
    if e < emin {
        if cw & 16 == 0 {
            return Result {
                value: if target == Format::F80 {
                    if e + 24576 < emin {
                        if n.negative { SIGN } else { 0 }
                    } else {
                        pack(n.negative, q, e + 24576, p, target, true)
                    }
                } else {
                    0
                },
                pre: 0,
                post: 16 | if inexact { 32 } else { 0 },
                up,
                suppress: target != Format::F80,
            };
        }
        let (q, inexact, up) = quantize(n.sig, emin - (p as i32 - 1) - n.scale, n.negative, rc(cw));
        debug_assert!(q <= 1u128 << (p - 1));
        let q = q as u64;
        return Result {
            value: pack(n.negative, q, emin, p, target, q >= 1u64 << (p - 1)),
            pre: 0,
            post: if inexact { 48 } else { 0 },
            up,
            suppress: false,
        };
    }
    Result {
        value: pack(n.negative, q, e, p, target, true),
        pre: 0,
        post: if inexact { 32 } else { 0 },
        up,
        suppress: false,
    }
}

#[cfg(test)]
#[test]
fn binary80_full_precision_packs_rounding_carry_and_underflow_promotion() {
    let carry = Num {
        negative: false,
        sig: (1u128 << 65) - 1,
        scale: -64,
    };
    let underflow = Num {
        negative: true,
        sig: u64::MAX as u128,
        scale: -16446,
    };
    for (input, cw, value, post, up) in [
        (carry, 0x037f, 0x4000_8000_0000_0000_0000, 32, true),
        (carry, 0x0f7f, 0x3fff_ffff_ffff_ffff_ffff, 32, false),
        (underflow, 0x037f, 0x8001_8000_0000_0000_0000, 48, true),
        (underflow, 0x0f7f, 0x8000_7fff_ffff_ffff_ffff, 48, false),
    ] {
        let result = round_num(input, 64, Format::F80, cw);
        assert_eq!(
            (
                result.value,
                result.pre,
                result.post,
                result.up,
                result.suppress
            ),
            (value, 0, post, up, false)
        );
    }
}

#[cfg(test)]
#[test]
fn reduced_precision_carry_matches_binary80_and_float_stores() {
    for (p, format, below80, below_store, carry_store) in [
        (
            24,
            Format::F32,
            0x3fff_ffff_ff00_0000_0000,
            0x3fff_ffff,
            0x4000_0000,
        ),
        (
            53,
            Format::F64,
            0x3fff_ffff_ffff_ffff_f800,
            0x3fff_ffff_ffff_ffff,
            0x4000_0000_0000_0000,
        ),
    ] {
        let midpoint = (0x3fffu128 << 64) | (((1u128 << (p + 1)) - 1) << (63 - p));
        for (cw, up, want80, want_store) in [
            (0x037f, true, 0x4000_8000_0000_0000_0000, carry_store),
            (0x0f7f, false, below80, below_store),
        ] {
            for (result, value) in [
                (round_num(num(midpoint), p, Format::F80, cw), want80),
                (store(midpoint, format, cw), want_store),
            ] {
                assert_eq!(
                    (
                        result.value,
                        result.pre,
                        result.post,
                        result.up,
                        result.suppress
                    ),
                    (value, 0, 32, up, false)
                );
            }
        }
    }
}

fn align_addend(sig: u64, difference: u32) -> u128 {
    // The addend starts with three low rounding bits. Keep the shifted high
    // word separate until the exponent difference removes all three bits.
    if difference < 3 {
        let left = 3 - difference;
        (u128::from(sig >> (64 - left)) << 64) | u128::from(sig << left)
    } else if difference < 67 {
        let right = difference - 3;
        u128::from((sig >> right) | u64::from(sig.trailing_zeros() < right))
    } else {
        u128::from(sig != 0)
    }
}

#[cfg(test)]
#[test]
fn aligned_addend_matches_wide_shift_and_sticky_bit() {
    let patterns = [0, u64::MAX, 0x8000_0000_0000_0001, 0xaaaa_aaaa_aaaa_aaaa];
    for sig in patterns.into_iter().chain((0..64).map(|bit| 1u64 << bit)) {
        let wide = u128::from(sig) << 3;
        for difference in 0..=130 {
            let expected = if difference >= 128 {
                u128::from(wide != 0)
            } else {
                (wide >> difference) | u128::from(wide & ((1u128 << difference) - 1) != 0)
            };
            assert_eq!(align_addend(sig, difference), expected);
        }
    }
}

fn add(a: Num, b: Num, round: u16) -> Num {
    if a.sig == 0 && b.sig == 0 {
        return Num {
            negative: if a.negative == b.negative {
                a.negative
            } else {
                round == 1
            },
            sig: 0,
            scale: 0,
        };
    }
    if a.sig == 0 {
        return b;
    }
    if b.sig == 0 {
        return a;
    }
    let (sa, ea) = normalized(a);
    let (sb, eb) = normalized(b);
    let (a, b, sa, sb, ea, eb) = if ea < eb || ea == eb && sa < sb {
        (b, a, sb, sa, eb, ea)
    } else {
        (a, b, sa, sb, ea, eb)
    };
    let left = sa << 3;
    let right = if ea == eb {
        sb << 3
    } else {
        align_addend(sb as u64, (ea - eb) as u32)
    };
    let sig = if a.negative == b.negative {
        left + right
    } else {
        left - right
    };
    Num {
        negative: if sig == 0 { round == 1 } else { a.negative },
        sig,
        scale: ea - 66,
    }
}
fn quotient_bit(remainder: &mut u64, divisor: u64) -> u64 {
    let carry = *remainder >> 63 != 0;
    let doubled = *remainder << 1;
    let subtract = carry || doubled >= divisor;
    *remainder = if subtract {
        doubled.wrapping_sub(divisor)
    } else {
        doubled
    };
    u64::from(subtract)
}

fn fractional_quotient(mut remainder: u64, divisor: u64) -> (u64, u64) {
    debug_assert!(divisor >= 1 << 63 && remainder < divisor);
    if remainder == 0 {
        return (0, 0);
    }

    const BASE: u64 = 1 << 32;
    let high = divisor >> 32;
    let low = u64::from(divisor as u32);
    let mut fraction = 0;
    // The low word of the numerator supplies two zero digits.
    for _ in 0..2 {
        let mut digit = (remainder / high).min(BASE - 1);
        let mut upper = remainder - digit * high;
        // A normalized divisor and remainder < divisor bound the estimate
        // to at most two decrements. Avoid forming upper * BASE when it would overflow.
        while upper < BASE && digit * low > upper << 32 {
            digit -= 1;
            upper += high;
        }
        // The exact remainder is below divisor, so its low word is sufficient.
        remainder = (upper << 32).wrapping_sub(digit * low);
        fraction = (fraction << 32) | digit;
    }
    (fraction, remainder)
}

#[cfg(test)]
#[test]
fn normalized_division_digits_match_wide_quotients_and_remainders() {
    let check = |remainder: u64, divisor: u64| {
        let numerator = u128::from(remainder) << 64;
        let wide = u128::from(divisor);
        assert_eq!(
            fractional_quotient(remainder, divisor),
            ((numerator / wide) as u64, (numerator % wide) as u64),
            "remainder={remainder:#018x} divisor={divisor:#018x}"
        );
    };
    for divisor in [
        0x8000_0000_0000_0000,
        0x8000_0000_0000_0001,
        0x8000_0000_ffff_ffff,
        0x8000_0001_0000_0000,
        0xffff_ffff_0000_0000,
        u64::MAX - 1,
        u64::MAX,
    ] {
        for remainder in [
            0,
            1,
            0xffff_ffff,
            1 << 32,
            divisor / 2,
            divisor - 2,
            divisor - 1,
        ] {
            check(remainder, divisor);
        }
    }
    // The first estimate needs both correction steps.
    check(0x7fff_ffff_0000_0000, 0x8000_0000_ffff_ffff);

    let mut seed = 0x8d79_26a4_cf50_b13eu64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..16384 {
        let divisor = next() | (1 << 63);
        check(next() % divisor, divisor);
    }
}

fn divide(a: Num, b: Num) -> Num {
    let (sa, ea) = normalized(a);
    let (sb, eb) = normalized(b);
    let sa = sa as u64;
    let sb = sb as u64;
    let mut e = ea - eb;
    let rem = if sa < sb {
        e -= 1;
        (sa << 1).wrapping_sub(sb)
    } else {
        sa - sb
    };
    let (fraction, mut rem) = fractional_quotient(rem, sb);
    let q = (1u128 << 64) | u128::from(fraction);
    let mut tail = 0u64;
    for _ in 0..2 {
        tail = (tail << 1) | quotient_bit(&mut rem, sb);
    }
    Num {
        negative: a.negative ^ b.negative,
        sig: (q << 2) | u128::from(tail | u64::from(rem != 0)),
        scale: e - 66,
    }
}
fn root(n: Num) -> Num {
    let (s, e) = normalized(n);
    let radicand = if e & 1 == 0 { s << 63 } else { s << 64 };
    let mut q = radicand.isqrt();
    let mut rem = radicand - q * q;
    // Append the same three zero pairs to recover the rounding bits.
    for _ in 0..3 {
        rem <<= 2;
        q <<= 1;
        let test = (q << 1) | 1;
        if rem >= test {
            rem -= test;
            q |= 1
        }
    }
    if rem != 0 {
        q |= 1
    }
    Num {
        negative: false,
        sig: q,
        scale: e.div_euclid(2) - 66,
    }
}
fn nan(a: u128, b: u128) -> Option<Result> {
    use Class::*;
    let ca = class(a);
    let cb = class(b);
    if ca == Unsupported || cb == Unsupported {
        return Some(Result::invalid(INDEFINITE));
    }
    let na = matches!(ca, QuietNan | SignalingNan);
    let nb = matches!(cb, QuietNan | SignalingNan);
    if !na && !nb {
        return None;
    }
    let raw = if ca == QuietNan && cb == SignalingNan {
        a
    } else if ca == SignalingNan && cb == QuietNan || !na {
        b
    } else if !nb || a as u64 > b as u64 {
        a
    } else if (a as u64) < b as u64 {
        b
    } else {
        a.min(b)
    };
    Some(Result {
        value: raw | QUIET,
        pre: if ca == SignalingNan || cb == SignalingNan {
            1
        } else {
            0
        },
        ..Result::raw(raw)
    })
}
#[inline]
fn normal_pc64_addition(kind: Kind, a: u128, b: u128, cw: u16, extra: u16) -> Option<Result> {
    if extra != 0
        || !matches!(kind, Kind::Add | Kind::Sub | Kind::SubReverse)
        || precision(cw) != 64
    {
        return None;
    }
    let a_exp_sign = (a >> 64) as u16;
    let b_exp_sign = (b >> 64) as u16;
    let a_exp = a_exp_sign & 0x7fff;
    let b_exp = b_exp_sign & 0x7fff;
    let a_sig = a as u64;
    let b_sig = b as u64;
    if a_exp == 0 || b_exp == 0 || a_exp == 0x7fff || b_exp == 0x7fff || (a_sig & b_sig) >> 63 == 0
    {
        return None;
    }
    let signs_differ = (a_exp_sign ^ b_exp_sign) >> 15 != 0;
    let subtract = signs_differ == (kind == Kind::Add);
    let a_negative = (a_exp_sign >> 15 != 0) ^ (kind == Kind::SubReverse);
    if subtract && a_exp == b_exp {
        if a_sig == b_sig {
            return Some(Result::raw(if rc(cw) == 1 { SIGN } else { 0 }));
        }
        let difference = a_sig.abs_diff(b_sig);
        let shift = difference.leading_zeros() as u16;
        if a_exp <= shift {
            return None;
        }
        let negative = a_negative ^ (a_sig < b_sig);
        let exponent_sign = (u16::from(negative) << 15) | (a_exp - shift);
        return Some(Result::raw(
            (u128::from(exponent_sign) << 64) | u128::from(difference << shift),
        ));
    }

    let (exponent, left, right, gap, negative) = if a_exp >= b_exp {
        (a_exp, a_sig, b_sig, a_exp - b_exp, a_negative)
    } else {
        (b_exp, b_sig, a_sig, b_exp - a_exp, a_negative ^ subtract)
    };
    let left = u128::from(left) << 3;
    let right = align_addend(right, u32::from(gap));
    let value = if subtract { left - right } else { left + right };
    // The aligned result has at most 68 bits. Round with its low four bits.
    let high = (value >> 64) as u64;
    let low = value as u64;
    let (significand, exponent, inexact, up) = if high == 0 {
        let shift = low.leading_zeros();
        (
            low << shift,
            i32::from(exponent) - 3 - shift as i32,
            false,
            false,
        )
    } else {
        let shift = u64::BITS - high.leading_zeros();
        let truncated = (high << (64 - shift)) | (low >> shift);
        let discarded = low & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        let inexact = discarded != 0;
        let up = inexact
            && match rc(cw) {
                0 => discarded > halfway || discarded == halfway && truncated & 1 != 0,
                1 => negative,
                2 => !negative,
                _ => false,
            };
        let (rounded, carry) = truncated.overflowing_add(u64::from(up));
        (
            rounded | (u64::from(carry) << 63),
            i32::from(exponent) + shift as i32 - 3 + i32::from(carry),
            inexact,
            up,
        )
    };
    if !(1..0x7fff).contains(&exponent) {
        return None;
    }
    let exponent_sign = (u16::from(negative) << 15) | exponent as u16;
    Some(Result {
        post: if inexact { 32 } else { 0 },
        up,
        ..Result::raw((u128::from(exponent_sign) << 64) | u128::from(significand))
    })
}

pub(super) fn binary(kind: Kind, a: u128, b: u128, cw: u16, extra: u16) -> Result {
    if let Some(result) = normal_pc64_addition(kind, a, b, cw, extra) {
        return result;
    }
    binary_general::<true>(kind, a, b, cw, extra)
}

#[cfg(test)]
#[test]
fn exact_pc64_difference_matches_general_binary80_arithmetic() {
    let value = |exponent, significand| (u128::from(exponent) << 64) | u128::from(significand);
    let difference =
        |kind, a, b, cw, extra| normal_pc64_addition(kind, a, b, cw, extra).map(|r| r.value);
    let fields = |result: Result| {
        (
            result.value,
            result.pre,
            result.post,
            result.up,
            result.suppress,
        )
    };
    let lo = 1u64 << 63;
    let one = value(0x3fffu16, lo);
    let next = value(0x3fffu16, lo + 1);
    let least_normal = value(1u16, lo);
    assert_eq!(
        difference(Kind::Sub, next, one, 0x37f, 0),
        Some(value(0x3fc0u16, lo))
    );
    assert_eq!(
        difference(Kind::SubReverse, next, one, 0x37f, 0),
        Some(value(0xbfc0u16, lo))
    );
    assert_eq!(
        difference(Kind::Add, next, next | SIGN, 0x77f, 0),
        Some(SIGN)
    );
    assert_eq!(difference(Kind::Sub, next, one, 0x27f, 0), None);
    assert_eq!(difference(Kind::Sub, next, one, 0x37f, 2), None);
    assert_eq!(
        difference(Kind::Sub, least_normal | 1, least_normal, 0x37f, 0),
        None
    );

    for exponent in [0u16, 1, 2, 63, 64, 0x3fff, 0x7ffe, 0x7fff] {
        for asig in [0, lo - 1, lo, lo + 1, lo | 0x13, u64::MAX] {
            for bsig in [0, lo - 1, lo, lo + 1, lo | 0x13, u64::MAX] {
                for signs in [(0, 0), (0, SIGN), (SIGN, 0), (SIGN, SIGN)] {
                    let a = value(exponent, asig) | signs.0;
                    let b = value(exponent, bsig) | signs.1;
                    for pc in 0..4 {
                        for round in 0..4 {
                            for masks in [0, 0x3f] {
                                let cw = 0x40 | (pc << 8) | (round << 10) | masks;
                                for kind in [Kind::Add, Kind::Sub, Kind::SubReverse, Kind::Mul] {
                                    let expected =
                                        fields(binary_general::<false>(kind, a, b, cw, 0));
                                    assert_eq!(
                                        fields(binary(kind, a, b, cw, 0)),
                                        expected,
                                        "{kind:?}, {a:#x}, {b:#x}, {cw:#x}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[test]
fn normal_pc64_addition_matches_general_rounding_across_exponent_gaps() {
    let value = |exponent, significand| (u128::from(exponent) << 64) | u128::from(significand);
    let fields = |result: Result| {
        (
            result.value,
            result.pre,
            result.post,
            result.up,
            result.suppress,
        )
    };
    let lo = 1u64 << 63;
    let one = value(0x3fffu16, lo);
    let half = value(0x3ffeu16, lo);
    let pair = (
        fields(binary(Kind::Add, one, half, 0x37f, 0)),
        fields(binary_general::<false>(Kind::Add, one, half, 0x37f, 0)),
    );
    assert_eq!(pair.0, pair.1);
    assert!(normal_pc64_addition(Kind::Add, one, half, 0x37f, 0).is_some());
    assert!(normal_pc64_addition(Kind::Add, one, one, 0x37f, 0).is_some());
    assert!(normal_pc64_addition(Kind::Sub, one, half, 0x37f, 0).is_some());
    assert!(normal_pc64_addition(Kind::Add, one, half, 0x27f, 0).is_none());
    assert!(normal_pc64_addition(Kind::Add, one, half, 0x37f, 2).is_none());
    assert!(
        normal_pc64_addition(
            Kind::Add,
            value(0x7ffeu16, lo),
            value(0x7ffeu16, lo),
            0x37f,
            0
        )
        .is_none()
    );
    assert!(
        normal_pc64_addition(Kind::Sub, value(2u16, lo), value(1u16, u64::MAX), 0x37f, 0).is_none()
    );

    let mut seed = 0xfbc5_4110_5121_90a3u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for ae in [
        1u16, 2, 63, 64, 65, 66, 67, 68, 127, 129, 0x3fff, 0x7ffd, 0x7ffe,
    ] {
        for gap in [0, 1, 2, 3, 4, 31, 32, 62, 63, 64, 65, 66, 67, 68, 128, 1024] {
            for (asig, bsig) in [
                (lo, lo),
                (lo, u64::MAX),
                (u64::MAX, lo),
                (u64::MAX, u64::MAX),
                (lo | next(), lo | next()),
                (u64::MAX, lo + 1),
            ] {
                let a = value(ae, asig);
                let b = value(ae.saturating_sub(gap).max(1), bsig);
                for signs in [(0, 0), (0, SIGN), (SIGN, 0), (SIGN, SIGN)] {
                    for (a, b) in [(a | signs.0, b | signs.1), (b | signs.1, a | signs.0)] {
                        for round in 0..4 {
                            for cw in [0x340 | (round << 10), 0x37f | (round << 10)] {
                                for kind in [Kind::Add, Kind::Sub, Kind::SubReverse] {
                                    assert_eq!(
                                        fields(binary(kind, a, b, cw, 0)),
                                        fields(binary_general::<false>(kind, a, b, cw, 0)),
                                        "{kind:?}, {a:#x}, {b:#x}, {cw:#x}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[inline(never)]
fn normal_pc64_quotient(a: u128, b: u128, cw: u16, extra: u16) -> Option<Result> {
    if extra != 0 || precision(cw) != 64 {
        return None;
    }
    let a_exp = ((a >> 64) as u16) & 0x7fff;
    let b_exp = ((b >> 64) as u16) & 0x7fff;
    let a_sig = a as u64;
    let b_sig = b as u64;
    if a_exp == 0 || b_exp == 0 || a_exp == 0x7fff || b_exp == 0x7fff || (a_sig & b_sig) >> 63 == 0
    {
        return None;
    }
    let mut exponent = i32::from(a_exp) - i32::from(b_exp) + 16383;
    let remainder = if a_sig < b_sig {
        exponent -= 1;
        (a_sig << 1).wrapping_sub(b_sig)
    } else {
        a_sig - b_sig
    };
    let (fraction, remainder) = fractional_quotient(remainder, b_sig);
    let truncated = (1 << 63) | (fraction >> 1);
    // The last computed quotient bit is halfway and the exact remainder is sticky.
    let halfway = fraction & 1 != 0;
    let inexact = halfway || remainder != 0;
    let negative = (a ^ b) & SIGN != 0;
    let up = inexact
        && match rc(cw) {
            0 => halfway && (remainder != 0 || truncated & 1 != 0),
            1 => negative,
            2 => !negative,
            _ => false,
        };
    let (rounded, carry) = truncated.overflowing_add(u64::from(up));
    exponent += i32::from(carry);
    if !(1..0x7fff).contains(&exponent) {
        return None;
    }
    let significand = rounded | (u64::from(carry) << 63);
    let exponent_sign = (u16::from(negative) << 15) | exponent as u16;
    Some(Result {
        post: if inexact { 32 } else { 0 },
        up,
        ..Result::raw((u128::from(exponent_sign) << 64) | u128::from(significand))
    })
}

#[cfg(test)]
#[test]
fn normal_pc64_quotient_matches_general_binary80_division() {
    let value = |exponent, significand| (u128::from(exponent) << 64) | u128::from(significand);
    let fields = |r: Result| (r.value, r.pre, r.post, r.up, r.suppress);
    let lo = 1u64 << 63;
    let one = value(0x3fffu16, lo);
    let next = value(0x3fffu16, lo + 1);
    assert_eq!(
        normal_pc64_quotient(next, one, 0x37f, 0).map(fields),
        Some((next, 0, 0, false, false))
    );
    assert!(normal_pc64_quotient(next, next, 0x27f, 0).is_none());
    assert!(normal_pc64_quotient(next, next, 0x37f, 2).is_none());
    assert!(normal_pc64_quotient(value(1u16, lo), value(0x7ffeu16, lo), 0x37f, 0).is_none());
    assert!(normal_pc64_quotient(value(0x7ffeu16, lo), value(1u16, lo), 0x37f, 0).is_none());

    let mut seed = 0xb79d_64fe_4358_2961u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for a_exp in [0u16, 1, 2, 0x3ffe, 0x3fff, 0x4000, 0x7ffd, 0x7ffe, 0x7fff] {
        for b_exp in [0u16, 1, 2, 0x3ffe, 0x3fff, 0x4000, 0x7ffd, 0x7ffe, 0x7fff] {
            for (a, b) in [
                (0, lo),
                (lo - 1, lo),
                (lo, lo),
                (lo + 1, lo + 1),
                (lo + (lo >> 1), lo + (lo >> 1)),
                (u64::MAX, u64::MAX),
                (lo | next(), lo | next()),
                (lo | next(), lo | next()),
            ] {
                for signs in [(0, 0), (SIGN, 0), (0, SIGN), (SIGN, SIGN)] {
                    let a = value(a_exp, a) | signs.0;
                    let b = value(b_exp, b) | signs.1;
                    for round in 0..4 {
                        for cw in [0x140 | (round << 10), 0x37f | (round << 10)] {
                            for kind in [Kind::Div, Kind::DivReverse] {
                                let expected = binary_general::<false>(kind, a, b, cw, 0);
                                assert_eq!(
                                    fields(binary(kind, a, b, cw, 0)),
                                    fields(expected),
                                    "{kind:?}, {a:#x}, {b:#x}, {cw:#x}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

fn binary_general<const DIRECT_DIVISION: bool>(
    kind: Kind,
    mut a: u128,
    mut b: u128,
    cw: u16,
    extra: u16,
) -> Result {
    use Class::*;
    if let Some(mut r) = nan(a, b) {
        r.pre |= extra & 1;
        return r;
    }
    if kind == Kind::Sub {
        b ^= SIGN
    } else if kind == Kind::SubReverse {
        a ^= SIGN
    } else if kind == Kind::DivReverse {
        std::mem::swap(&mut a, &mut b)
    }
    let ca = class(a);
    let cb = class(b);
    let add_kind = matches!(kind, Kind::Add | Kind::Sub | Kind::SubReverse);
    let mul_kind = kind == Kind::Mul;
    let invalid = if add_kind {
        ca == Infinity && cb == Infinity && (a ^ b) & SIGN != 0
    } else if mul_kind {
        ca == Infinity && cb == Zero || ca == Zero && cb == Infinity
    } else {
        ca == Infinity && cb == Infinity || ca == Zero && cb == Zero
    };
    if invalid {
        return Result::invalid(INDEFINITE);
    }
    let negative = (a ^ b) & SIGN;
    if !add_kind && !mul_kind && cb == Zero && ca != Infinity {
        return Result {
            value: INFINITY | negative,
            pre: 4,
            ..Result::raw(0)
        };
    }
    let pre = extra | if denormal(a) || denormal(b) { 2 } else { 0 };
    if pre & !cw & 63 != 0 {
        return Result {
            pre,
            suppress: true,
            ..Result::raw(a)
        };
    }
    if ca == Infinity || cb == Infinity {
        let value = if add_kind {
            if ca == Infinity { a } else { b }
        } else if mul_kind || ca == Infinity {
            INFINITY | negative
        } else {
            negative
        };
        return Result {
            pre,
            ..Result::raw(value)
        };
    }
    let na = num(a);
    let nb = num(b);
    let n = if add_kind {
        add(na, nb, rc(cw))
    } else if mul_kind {
        Num {
            negative: na.negative ^ nb.negative,
            sig: na.sig * nb.sig,
            scale: na.scale + nb.scale,
        }
    } else if na.sig == 0 {
        Num {
            negative: na.negative ^ nb.negative,
            sig: 0,
            scale: 0,
        }
    } else {
        if DIRECT_DIVISION && let Some(result) = normal_pc64_quotient(a, b, cw, pre) {
            return result;
        }
        divide(na, nb)
    };
    let mut r = round_num(n, precision(cw), Format::F80, cw);
    r.pre = pre;
    r
}
pub(super) fn unary(kind: Kind, raw: u128, cw: u16) -> Result {
    if let Some(r) = nan(raw, raw) {
        return r;
    }
    let c = class(raw);
    if kind == Kind::Sqrt && raw & SIGN != 0 && c != Class::Zero {
        return Result::invalid(INDEFINITE);
    }
    let pre = if denormal(raw) { 2 } else { 0 };
    if pre & !cw != 0 {
        return Result {
            pre,
            suppress: true,
            ..Result::raw(raw)
        };
    }
    if matches!(c, Class::Zero | Class::Infinity) {
        return Result {
            pre,
            ..Result::raw(raw)
        };
    }
    let n = num(raw);
    let mut r = if kind == Kind::Sqrt {
        round_num(root(n), precision(cw), Format::F80, cw)
    } else {
        let (q, inexact, up) = if n.scale >= 0 {
            return Result {
                pre,
                ..Result::raw(raw)
            };
        } else {
            quantize(n.sig, -n.scale, n.negative, rc(cw))
        };
        // A fractional binary80 value rounds to an integer of at most 2^63.
        // Its normalized significand is exact, including a carry from a tie.
        let (sig, exponent) = normalized(Num {
            sig: q,
            scale: 0,
            ..n
        });
        Result {
            post: if inexact { 32 } else { 0 },
            up,
            ..Result::raw(pack(
                n.negative,
                sig as u64,
                exponent,
                64,
                Format::F80,
                q != 0,
            ))
        }
    };
    r.pre = pre;
    r
}

fn exact_f80(sign: u128, significand: u64, scale: i32) -> u128 {
    if significand == 0 {
        return sign;
    }
    let shift = significand.leading_zeros();
    let biased = 16383 + scale + (63 - shift) as i32;
    sign | ((biased as u128) << 64) | u128::from(significand << shift)
}

pub(super) fn widen(value: u128, format: Format) -> u128 {
    match format {
        Format::F80 => value,
        Format::F32 | Format::F64 => {
            let (p, bias, bits) = if format == Format::F32 {
                (23, 127, 32)
            } else {
                (52, 1023, 64)
            };
            let value = value as u64;
            let negative = value & (1u64 << (bits - 1)) != 0;
            let fraction = value & ((1u64 << p) - 1);
            let e = ((value >> p) & ((1u64 << (bits - p - 1)) - 1)) as i32;
            let sign = if negative { SIGN } else { 0 };
            if e == 2 * bias + 1 {
                return sign | INFINITY | u128::from(fraction << (63 - p));
            }
            if e == 0 {
                return exact_f80(sign, fraction, 1 - bias - p);
            }
            let biased = (e - bias + 16383) as u128;
            let significand = (fraction | (1u64 << p)) << (63 - p);
            sign | (biased << 64) | u128::from(significand)
        }
        Format::I16 | Format::I32 | Format::I64 => {
            let value = match format {
                Format::I16 => i64::from(value as i16),
                Format::I32 => i64::from(value as i32),
                _ => value as i64,
            };
            exact_f80(if value < 0 { SIGN } else { 0 }, value.unsigned_abs(), 0)
        }
        _ => value,
    }
}
pub(super) fn memory_denormal(value: u128, format: Format) -> bool {
    match format {
        Format::F32 => value & 0x7f800000 == 0 && value & 0x7fffff != 0,
        Format::F64 => value & 0x7ff0000000000000 == 0 && value & 0xfffffffffffff != 0,
        _ => false,
    }
}
pub(super) fn load(value: u128, format: Format) -> Result {
    if format == Format::Bcd {
        return special::load_bcd(value);
    }
    let raw = widen(value, format);
    if matches!(format, Format::F32 | Format::F64) {
        let mut r = Result::raw(raw);
        if class(raw) == Class::SignalingNan {
            r.value |= QUIET;
            r.pre |= 1
        }
        if memory_denormal(value, format) {
            r.pre |= 2
        }
        r
    } else {
        Result::raw(raw)
    }
}
#[inline(always)]
pub(super) fn store(raw: u128, format: Format, cw: u16) -> Result {
    if format == Format::F80 || format == Format::Register {
        return Result::raw(raw);
    }
    if format == Format::F64
        && let Some(result) = normal_f64_store(raw, cw)
    {
        return result;
    }
    store_converted(raw, format, cw)
}

fn normal_f64_store(raw: u128, cw: u16) -> Option<Result> {
    let exponent_sign = (raw >> 64) as u16;
    let exponent = (exponent_sign & 0x7fff).wrapping_sub(0x3c00);
    let significand = raw as u64;
    // Both the truncated result and a possible rounding carry remain normal.
    if !(1..0x7fe).contains(&exponent) || significand >> 63 == 0 {
        return None;
    }
    let retained = significand >> 11;
    let tail = significand & 0x7ff;
    let negative = exponent_sign >> 15 != 0;
    let up = tail != 0
        && match rc(cw) {
            0 => tail + (retained & 1) > 0x400,
            1 => negative,
            2 => !negative,
            _ => false,
        };
    let magnitude = (u64::from(exponent - 1) << 52) + retained + u64::from(up);
    let value = (u64::from(negative) << 63) | magnitude;
    Some(Result {
        post: if tail == 0 { 0 } else { 32 },
        up,
        ..Result::raw(u128::from(value))
    })
}

#[cfg(test)]
#[test]
fn normal_binary64_store_matches_general_rounding() {
    let fields = |r: Result| (r.value, r.pre, r.post, r.up, r.suppress);
    for exponent in [0x3c01u16, 0x3fff, 0x4000, 0x43fd] {
        for sign in [0, 0x8000] {
            for round in 0..4 {
                for masks in [0, 63] {
                    let cw = (round << 10) | masks;
                    for retained in [1 << 63, (1 << 63) | (1 << 11), !0x7ffu64] {
                        for tail in 0..=0x7ff {
                            let raw =
                                (u128::from(exponent | sign) << 64) | u128::from(retained | tail);
                            assert_eq!(
                                fields(normal_f64_store(raw, cw).unwrap()),
                                fields(store_converted(raw, Format::F64, cw)),
                                "{raw:#x}, {cw:#x}"
                            );
                        }
                    }
                }
            }
        }
    }
    for exponent in [0, 0x3c00u16, 0x43fe, 0x7fff] {
        let raw = (u128::from(exponent) << 64) | (1 << 63);
        assert!(normal_f64_store(raw, 0x037f).is_none());
    }
    assert!(normal_f64_store(0x3fff_4000_0000_0000_0000, 0x037f).is_none());
}

fn store_converted(raw: u128, format: Format, cw: u16) -> Result {
    if format == Format::Bcd {
        return special::store_bcd(raw, cw);
    }
    if matches!(format, Format::I16 | Format::I32 | Format::I64) {
        let width = if format == Format::I16 {
            16
        } else if format == Format::I32 {
            32
        } else {
            64
        };
        let indefinite = 1u128 << (width - 1);
        if !matches!(class(raw), Class::Zero | Class::Finite) {
            return Result::invalid(indefinite);
        }
        let n = num(raw);
        let (q, inexact, up) = if n.scale >= 0 {
            if n.scale >= 128 || 128 - n.sig.leading_zeros() + n.scale as u32 > 127 {
                return Result::invalid(indefinite);
            }
            (n.sig << n.scale, false, false)
        } else {
            quantize(n.sig, -n.scale, n.negative, rc(cw))
        };
        if q > indefinite || q == indefinite && !n.negative {
            return Result::invalid(indefinite);
        }
        return Result {
            value: if n.negative {
                0u128.wrapping_sub(q) & ((1u128 << width) - 1)
            } else {
                q
            },
            pre: 0,
            post: if inexact { 32 } else { 0 },
            up,
            suppress: false,
        };
    }
    let c = class(raw);
    let (bits, p) = if format == Format::F32 {
        (32, 23)
    } else {
        (64, 52)
    };
    let sign = if raw & SIGN != 0 {
        1u128 << (bits - 1)
    } else {
        0
    };
    let inf = if bits == 32 {
        0x7f800000
    } else {
        0x7ff0000000000000
    };
    match c {
        Class::Unsupported => Result::invalid((1u128 << (bits - 1)) | inf | (1u128 << (p - 1))),
        Class::QuietNan | Class::SignalingNan => Result {
            value: sign
                | inf
                | (((raw as u64) as u128 & !(1 << 63)) >> (63 - p))
                | (1u128 << (p - 1)),
            pre: if c == Class::SignalingNan { 1 } else { 0 },
            ..Result::raw(0)
        },
        Class::Infinity => Result::raw(sign | inf),
        _ => {
            let mut r = round_num(num(raw), p + 1, format, cw);
            if r.suppress {
                r.post &= !32;
                r.up = false
            }
            r
        }
    }
}
pub(super) fn compare(a: u128, b: u128) -> Option<Ordering> {
    use Class::*;
    if matches!(class(a), QuietNan | SignalingNan | Unsupported)
        || matches!(class(b), QuietNan | SignalingNan | Unsupported)
    {
        return None;
    }
    if class(a) == Zero && class(b) == Zero {
        return Some(Ordering::Equal);
    }
    let neg_a = a & SIGN != 0;
    let neg_b = b & SIGN != 0;
    if neg_a != neg_b {
        return Some(if neg_a {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let abs = if class(a) == Infinity {
        if class(b) == Infinity {
            Ordering::Equal
        } else {
            Ordering::Greater
        }
    } else if class(b) == Infinity {
        Ordering::Less
    } else {
        let (sa, ea) = normalized(num(a));
        let (sb, eb) = normalized(num(b));
        if sa == 0 {
            if sb == 0 {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        } else if sb == 0 {
            Ordering::Greater
        } else {
            ea.cmp(&eb).then(sa.cmp(&sb))
        }
    };
    Some(if neg_a { abs.reverse() } else { abs })
}
#[inline(always)]
pub(super) fn constant(index: u8, cw: u16) -> u128 {
    // The low two bits are the extra bits of the Intel 66-significant-bit ROM.
    const ROM: [(u16, u64, u8); 7] = [
        (0x3fff, 0x8000000000000000, 0),
        (0x4000, 0xd49a784bcd1b8afe, 1),
        (0x3fff, 0xb8aa3b295c17f0bb, 3),
        (0x4000, 0xc90fdaa22168c234, 3),
        (0x3ffd, 0x9a209a84fbcff798, 3),
        (0x3ffe, 0xb17217f7d1cf79ab, 3),
        (0, 0, 0),
    ];
    let (e, s, tail) = ROM[usize::from(index)];
    let up = match rc(cw) {
        0 => tail > 2 || tail == 2 && s & 1 != 0,
        2 => tail != 0,
        _ => false,
    };
    ((e as u128) << 64) | u128::from(s + u64::from(up))
}
