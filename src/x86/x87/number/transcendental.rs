use super::*;

mod finite;

const ONE: u128 = 0x3fff_8000_0000_0000_0000;
// Largest binary80 magnitude within the FYL2XP1 interval 1 − sqrt(2)/2.
const LOG1P_LIMIT: u128 = 0x3ffd_95f6_1998_0c43_36f7;

fn exponential_special(x: u128) -> Option<Result> {
    let magnitude = x & !SIGN;
    if magnitude == INFINITY {
        return Some(Result::raw(if x & SIGN != 0 { SIGN | ONE } else { x }));
    }
    if magnitude == 0 || magnitude > ONE {
        // The finite out-of-range result is model-defined.
        return Some(Result::raw(x));
    }
    None
}

fn logarithm_special(plus_one: bool, x: u128, y: u128) -> Option<Result> {
    let ax = x & !SIGN;
    let ay = y & !SIGN;
    let y_sign = y & SIGN;
    // For log2(1+x), the logarithm has x's sign, including negative zero.
    let log_sign = if plus_one {
        x & SIGN
    } else if x < ONE {
        SIGN
    } else {
        0
    };
    let result_sign = log_sign ^ y_sign;

    if x == (SIGN | INFINITY) || !plus_one && x & SIGN != 0 && ax != 0 {
        return Some(Result::invalid(INDEFINITE));
    }
    if !plus_one && ax == 0 {
        return Some(match ay {
            0 => Result::invalid(INDEFINITE),
            INFINITY => Result::raw(INFINITY | (SIGN ^ y_sign)),
            _ => Result {
                pre: 4,
                ..Result::raw(INFINITY | (SIGN ^ y_sign))
            },
        });
    }
    let zero_log = if plus_one { ax == 0 } else { x == ONE };
    if zero_log && ay == INFINITY || ax == INFINITY && ay == 0 {
        return Some(Result::invalid(INDEFINITE));
    }
    if ax == INFINITY || ay == INFINITY {
        return Some(Result::raw(INFINITY | result_sign));
    }
    if zero_log || ay == 0 {
        return Some(Result::raw(result_sign));
    }
    if plus_one && ax > LOG1P_LIMIT {
        return Some(Result::raw(x));
    }
    None
}

pub(in crate::x86::x87) fn transcendental(kind: Kind, x: u128, y: u128, cw: u16) -> Result {
    let unary = kind == Kind::Exp2MinusOne;
    if let Some(result) = nan(x, if unary { x } else { y }) {
        return result;
    }
    let special = if unary {
        exponential_special(x)
    } else {
        logarithm_special(kind == Kind::MulLog2PlusOne, x, y)
    };
    if let Some(result) = special
        && result.pre != 0
    {
        // Domain errors and zero-divide precede a denormal in the other operand.
        return result;
    }
    let pre = if denormal(x) || !unary && denormal(y) {
        2
    } else {
        0
    };
    if pre & !cw != 0 {
        return Result {
            pre,
            ..Result::raw(x)
        };
    }
    let mut result = special.unwrap_or_else(|| {
        let mut result = finite::evaluate(kind, x, y, cw);
        // The x87 approximation signals precision even for nonzero endpoints
        // of F2XM1 and exact powers of two in the ordinary FYL2X path.
        result.post |= 32;
        result
    });
    result.pre = pre;
    result
}
