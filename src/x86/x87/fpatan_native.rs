//! Native x86 captures the producer with a non-waiting FXSAVE before any #MF.
use super::{
    log_exp_native::{hardware, rank},
    *,
};

const SIGN: u128 = 1 << 79;
const HALF: u128 = 0x3ffe8000000000000000;
const ONE: u128 = 0x3fff8000000000000000;
const FOUR: u128 = 0x40018000000000000000;
const INFINITY: u128 = 0x7fff8000000000000000;
const MIN_NORMAL: u128 = 0x00018000000000000000;

fn compare(x: Option<u128>, y: Option<u128>, cw: u16, rounded: bool, check_c1: bool) {
    let mut state = State {
        status_word: (7 << 11) | 0x200,
        control_word: cw,
        ..State::default()
    };
    for (logical, raw) in [x, y, Some(0xbffddecafbad11223344)].into_iter().enumerate() {
        if let Some(raw) = raw {
            state.put(state.physical(logical as u8), raw);
        }
    }
    let output = hardware(Kind::Atan2, &state).0;
    let effect = eval(
        &mut state,
        Op {
            kind: Kind::Atan2,
            format: Format::Register,
            index: 1,
            dest_index: true,
            pop: 1,
            ..Op::default()
        },
        0,
        0x8d7,
    );
    assert_eq!(effect.flags, 0x8d7);
    assert!(!effect.store && !effect.ax);
    assert_eq!(state.control_word, cw);
    assert_eq!(output[..2], cw.to_le_bytes());
    let status = u16::from_le_bytes([output[2], output[3]]);
    let mask = if check_c1 { 0xbaff } else { 0xb8ff };
    assert_eq!(
        state.status_word & mask,
        status & mask,
        "FPATAN x={x:x?} y={y:x?} CW={cw:04x}, native FSW={status:04x}"
    );
    let top = usize::from((status >> 11) & 7);
    for logical in 0..8 {
        let physical = (top + logical) & 7;
        let present = output[4] & (1 << physical) != 0;
        assert_eq!(state.tag(physical) != 3, present);
        if !present {
            continue;
        }
        let mut raw = [0; 16];
        let offset = 32 + logical * 16;
        raw[..10].copy_from_slice(&output[offset..offset + 10]);
        let native = u128::from_le_bytes(raw);
        let actual = state.raw(physical);
        if rounded && logical == 0 && top == 0 {
            // Independent finite approximations may pick different last bits.
            // The absolute error against the exact angle is tested separately.
            let allowance = if cw >> 10 & 3 == 0 { 2 } else { 3 };
            assert_eq!(actual & SIGN, native & SIGN);
            assert!(
                rank(actual).abs_diff(rank(native)) <= allowance,
                "FPATAN x={x:x?} y={y:x?} CW={cw:04x}: Ring64 {actual:020x}, native {native:020x}"
            );
        } else {
            assert_eq!(
                actual, native,
                "FPATAN ST({logical}) x={x:x?} y={y:x?} CW={cw:04x}"
            );
        }
    }
}

#[test]
fn fpatan_native_preconditions_and_tag_wrap() {
    for (x, y) in [(None, Some(ONE)), (Some(ONE), None), (None, None)] {
        for cw in [0x37f, 0x37e] {
            compare(x, y, cw, false, true);
        }
    }
    for (x, y) in [
        (0x7fffa000000000000045, ONE), // signaling nan
        (ONE, 0x7fffa000000000000045),
        (0x40000000000000000011, ONE), // unsupported integer bit
        (0x7fffc000000000000042, ONE),
        (0x7fffa000000000000045, 0xffffc000000000000044),
        (0x7fffc000000000000042, 1),
        (0x40000000000000000011, 0x7fffa000000000000045),
    ] {
        for cw in [0x37f, 0x37e] {
            compare(Some(x), Some(y), cw, false, false);
        }
    }
    for (x, y) in [
        (1, FOUR),
        (FOUR, 1),
        (1 << 63, ONE),
        (ONE, 1 << 63),
        (INFINITY, 1),
    ] {
        compare(Some(x), Some(y), 0x37d, false, false);
    }
    for (x, y) in [(0, SIGN), (INFINITY, ONE), (INFINITY, SIGN | ONE)] {
        compare(Some(x), Some(y), 0x37f, false, true);
    }
}

#[test]
fn fpatan_native_precision_and_underflow_publish_before_waiting() {
    for pc in [0, 2, 3] {
        for rc in 0..4 {
            for masks in [0x7f, 0x5f] {
                let cw = masks | (pc << 8) | (rc << 10);
                for (x, y) in [(ONE, HALF), (ONE, SIGN | HALF), (SIGN | ONE, 0)] {
                    compare(Some(x), Some(y), cw, true, false);
                }
            }
        }
    }
    // both inputs are normal, which separates post-result ue from input de.
    for rc in 0..4 {
        for base in [0x37f, 0x36f] {
            let cw = base | (rc << 10);
            compare(Some(FOUR), Some(MIN_NORMAL), cw, true, false);
            compare(Some(SIGN | FOUR), Some(MIN_NORMAL), cw, true, false);
            // native intel and amd can choose different neighbors and c1 here.
            compare(
                Some(0x403f8000000000000000),
                Some(0x0002c000000000000000),
                cw,
                true,
                false,
            );
        }
    }
}
