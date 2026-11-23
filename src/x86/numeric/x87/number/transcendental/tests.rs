use super::*;
use crate::x86::x87::{self, Op, State};

const TWO: u128 = 0x4000_8000_0000_0000_0000;
const HALF: u128 = 0x3ffe_8000_0000_0000_0000;

fn execute(kind: Kind, x: Option<u128>, y: Option<u128>, cw: u16) -> State {
    let mut state = State {
        status_word: (7 << 11) | 0x200,
        control_word: cw,
        ..State::default()
    };
    if let Some(x) = x {
        state.put(7, x);
    }
    if let Some(y) = y {
        state.put(0, y);
    }
    state.put(5, TWO);
    let unary = kind == Kind::Exp2MinusOne;
    let effect = x87::eval(
        &mut state,
        Op {
            kind,
            format: if unary {
                Format::None
            } else {
                Format::Register
            },
            index: u8::from(!unary),
            dest_index: !unary,
            pop: u8::from(!unary),
            ..Op::default()
        },
        0,
        0xcd7,
    );
    assert_eq!(effect.flags, 0xcd7);
    assert!(!effect.store && !effect.ax);
    assert_eq!(state.raw(5), TWO);
    assert_eq!(state.tag(5), 0);
    assert_eq!(state.control_word, cw);
    state
}

#[test]
fn logarithm_tables_preserve_signs_and_check_domain_errors_before_denormals() {
    use Kind::{MulLog2, MulLog2PlusOne};
    for (kind, x, y, value, pre) in [
        (MulLog2, 0, ONE, SIGN | INFINITY, 4),
        (MulLog2, SIGN, SIGN | ONE, INFINITY, 4),
        (MulLog2, 0, 1, SIGN | INFINITY, 4),
        (MulLog2, SIGN, INFINITY, SIGN | INFINITY, 0),
        (MulLog2, 0, 0, INDEFINITE, 1),
        (MulLog2, SIGN | 1, ONE, INDEFINITE, 1),
        (MulLog2, ONE, INFINITY, INDEFINITE, 1),
        (MulLog2, INFINITY, 0, INDEFINITE, 1),
        (MulLog2, INFINITY, SIGN | HALF, SIGN | INFINITY, 0),
        (MulLog2, HALF, SIGN, 0, 0),
        (MulLog2, HALF, 0, SIGN, 0),
        (MulLog2, TWO, SIGN, SIGN, 0),
        (MulLog2, ONE, SIGN | 1, SIGN, 2),
        (MulLog2PlusOne, SIGN, ONE, SIGN, 0),
        (MulLog2PlusOne, SIGN, SIGN | ONE, 0, 0),
        (MulLog2PlusOne, 0, SIGN, SIGN, 0),
        (MulLog2PlusOne, 0, INFINITY, INDEFINITE, 1),
        (MulLog2PlusOne, SIGN | 1, INFINITY, SIGN | INFINITY, 2),
        (MulLog2PlusOne, SIGN | 1, SIGN, 0, 2),
    ] {
        let state = execute(kind, Some(x), Some(y), 0x37f);
        assert_eq!((state.top(), state.raw(0), state.tag(7)), (0, value, 3));
        assert_eq!(state.status_word, pre, "{kind:?} {x:x} {y:x}");
        if pre != 0 {
            let state = execute(kind, Some(x), Some(y), 0x37f & !pre);
            assert_eq!((state.top(), state.raw(7), state.raw(0)), (7, x, y));
            assert_eq!(state.status_word, (7 << 11) | 0x8080 | pre);
        }
    }
}

#[test]
fn nan_stack_and_denormal_priority_prevent_spurious_results_and_pops() {
    use Kind::{Exp2MinusOne, MulLog2, MulLog2PlusOne};
    let snan = INFINITY | 0x456;
    let qnan = SIGN | INFINITY | QUIET | 0x123;
    let pseudo_denormal = 1u128 << 63;
    let unnormal = 2u128 << 64 | 17;
    for kind in [Exp2MinusOne, MulLog2, MulLog2PlusOne] {
        let unary = kind == Exp2MinusOne;
        for (x, y, value, expected) in [
            (unnormal, qnan, INDEFINITE, 1),
            (snan, pseudo_denormal, snan | QUIET, 1),
            (qnan, 1, qnan, 0),
        ] {
            let state = execute(kind, Some(x), Some(y), 0x37f);
            assert_eq!(state.raw(state.top()), value);
            assert_eq!(state.status_word & 0x82ff, expected);
        }
        for (x, y) in [(None, Some(snan)), (Some(snan), None)] {
            if unary && x.is_some() {
                continue;
            }
            for masked in [false, true] {
                let state = execute(kind, x, y, 0x37e | u16::from(masked));
                assert_eq!(state.top(), if unary || !masked { 7 } else { 0 });
                assert_eq!(
                    state.status_word & 0x82ff,
                    if masked { 0x41 } else { 0x80c1 }
                );
                if masked {
                    assert_eq!(state.raw(state.top()), INDEFINITE);
                } else {
                    assert_eq!(state.tag(7) == 3, x.is_none());
                    assert_eq!(state.tag(0) == 3, y.is_none());
                }
            }
        }
        let state = execute(kind, Some(pseudo_denormal), Some(ONE), 0x37d);
        assert_eq!(state.top(), 7);
        assert_eq!(state.raw(7), pseudo_denormal);
        assert_eq!(state.status_word, (7 << 11) | 0x8082);
    }
}

#[test]
fn post_exceptions_publish_register_results_and_complete_the_required_pop() {
    use Kind::{Exp2MinusOne, MulLog2};
    for (kind, x, y, value, status, cw) in [
        (Exp2MinusOne, 0, INFINITY, 0, 0, 0x37f),
        (Exp2MinusOne, SIGN, INFINITY, SIGN, 0, 0x37f),
        (Exp2MinusOne, INFINITY, 0, INFINITY, 0, 0x37f),
        (Exp2MinusOne, SIGN | INFINITY, 0, SIGN | ONE, 0, 0x37f),
        (Exp2MinusOne, ONE, 0, ONE, 0x80a0, 0x35f),
        (Exp2MinusOne, SIGN | ONE, 0, SIGN | HALF, 0x80a0, 0x35f),
        (MulLog2, TWO, ONE, ONE, 0x80a0, 0x35f),
        // exp2m1(min-subnormal) rounds up to min-subnormal. Unmasking UE
        // publishes the normalized result scaled by 2^24576 instead.
        (Exp2MinusOne, 1, 0, 1, 0x232, 0x37f),
        (
            Exp2MinusOne,
            1,
            0,
            0x5fc1_b172_17f7_d1cf_79ac,
            0x82b2,
            0x36f,
        ),
        // log2(4) * 2^16383 overflows. The register receives 2^(16384−24576).
        (
            MulLog2,
            TWO + (1u128 << 64),
            0x7ffe_8000_0000_0000_0000,
            0x1fff_8000_0000_0000_0000,
            0x80a8,
            0x377,
        ),
    ] {
        let state = execute(kind, Some(x), Some(y), cw);
        assert_eq!(state.top(), if kind == Exp2MinusOne { 7 } else { 0 });
        assert_eq!(state.raw(state.top()), value);
        assert_eq!(state.status_word & 0x82ff, status);
    }
}
