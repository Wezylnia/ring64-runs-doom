use super::*;
use crate::x86::x87::number::{INDEFINITE, INFINITY, SIGN};

const ONE: u128 = 0x3fff_8000_0000_0000_0000;
const MIN_NORMAL: u128 = 0x0001_8000_0000_0000_0000;
const MAX_SUBNORMAL: u128 = 0x7fff_ffff_ffff_ffff;

#[test]
fn fpatan_dyadic_slopes_round_from_strictly_below_the_ratio() {
    let tiny = 0x3f9b_8000_0000_0000_0000;
    let previous = 0x3f9a_ffff_ffff_ffff_ffff;
    for (y, values) in [
        (tiny, [tiny, previous, tiny, previous]),
        (
            SIGN | tiny,
            [SIGN | tiny, SIGN | tiny, SIGN | previous, SIGN | previous],
        ),
        (
            MIN_NORMAL,
            [MIN_NORMAL, MAX_SUBNORMAL, MIN_NORMAL, MAX_SUBNORMAL],
        ),
        (
            SIGN | MIN_NORMAL,
            [
                SIGN | MIN_NORMAL,
                SIGN | MIN_NORMAL,
                SIGN | MAX_SUBNORMAL,
                SIGN | MAX_SUBNORMAL,
            ],
        ),
    ] {
        for (rc, value) in values.into_iter().enumerate() {
            let result = atan2(ONE, y, 0x037f | (rc as u16) << 10);
            let masked_underflow = value & !SIGN == MAX_SUBNORMAL;
            assert_eq!(result.value, value, "y={y:x} rc={rc}");
            assert_eq!(
                (result.pre, result.post),
                (0, if masked_underflow { 48 } else { 32 })
            );
        }
    }
}

#[test]
fn fpatan_tiny_results_pack_once() {
    // atan(1.5 minsub) is immediately below the gradual-rounding tie.
    let x = 0x403f_8000_0000_0000_0000; // 2^64
    let y = 0x0002_c000_0000_0000_0000; // 3 minnormal
    let masked = atan2(x, y, 0x037f);
    assert_eq!(
        (masked.value, masked.pre, masked.post, masked.up),
        (1, 0, 48, false)
    );
    let wrapped = atan2(x, y, 0x036f);
    assert_eq!(
        (wrapped.value, wrapped.pre, wrapped.post, wrapped.up),
        (0x5fc2_c000_0000_0000_0000, 0, 48, true)
    );
}

#[test]
fn fpatan_signed_axes_and_diagonals_obey_rc_and_ignore_pc() {
    let pi_down = 0x4000_c90f_daa2_2168_c234;
    let half_pi_down = 0x3fff_c90f_daa2_2168_c234;
    let quarter_pi_down = 0x3ffe_c90f_daa2_2168_c234;
    let three_quarters_down = 0x4000_96cb_e3f9_990e_91a7;
    for pc in [0, 2, 3] {
        for rc in 0..4 {
            for sign in [0, SIGN] {
                let cw = 0x7f | (pc << 8) | (rc << 10);
                let up = rc == 0 || (rc == 1 && sign != 0) || (rc == 2 && sign == 0);
                for (x, y, down) in [
                    (0, 0, 0),
                    (INFINITY, ONE, 0),
                    (SIGN, 0, pi_down),
                    (SIGN | INFINITY, ONE, pi_down),
                    (0, ONE, half_pi_down),
                    (ONE, INFINITY, half_pi_down),
                    (ONE, ONE, quarter_pi_down),
                    (INFINITY, INFINITY, quarter_pi_down),
                    (SIGN | ONE, ONE, three_quarters_down),
                    (SIGN | INFINITY, INFINITY, three_quarters_down),
                ] {
                    let actual = atan2(x, sign | y, cw);
                    let inexact = down != 0;
                    assert_eq!(actual.value, sign | (down + u128::from(up && inexact)));
                    assert_eq!(
                        (actual.pre, actual.post, actual.up),
                        (0, if inexact { 32 } else { 0 }, up && inexact)
                    );
                }
            }
        }
    }
}

#[test]
fn fpatan_operand_classes_choose_inherited_nan_and_denormal_priority() {
    let quiet = 0x7fff_c000_0000_0000_0001;
    let signaling = 0x7fff_bfff_ffff_ffff_ffff;
    let pseudo_denormal = 0x8000_0000_0000_0000;
    for (x, y, pre, value) in [
        (quiet | SIGN, signaling, 1, quiet | SIGN),
        (quiet | SIGN, quiet, 0, quiet),
        (0x7fff_0000_0000_0000_0001, quiet, 1, INDEFINITE),
        (quiet, 1, 0, quiet),
        (0, pseudo_denormal, 2, 0x3fff_c90f_daa2_2168_c235),
        (pseudo_denormal, SIGN, 2, SIGN),
    ] {
        for (x, y) in [(x, y), (y, x)]
            .into_iter()
            .take(if pre == 2 { 1 } else { 2 })
        {
            let result = atan2(x, y, 0x037f);
            assert_eq!((result.value, result.pre), (value, pre));
            assert_eq!(
                result.post,
                if pre == 2 && value & !SIGN != 0 {
                    32
                } else {
                    0
                }
            );
        }
    }
    for (x, y) in [(0, 1), (pseudo_denormal, INFINITY), (ONE, pseudo_denormal)] {
        let result = atan2(x, y, 0x037d);
        assert_eq!((result.pre, result.post, result.up), (2, 0, false));
    }
}

#[test]
fn fpatan_adjacent_operands_keep_order_at_the_diagonal_and_reduction_seams() {
    let split = 0x3ffd_d800_0000_0000_0000; // 27/64
    let near = [
        [ONE - (1 << 63) - 1, ONE, ONE + 1],
        [split - 1, split, split + 1],
        [
            0x3fbe_ffff_ffff_ffff_ffff,
            0x3fbf_8000_0000_0000_0000,
            0x3fbf_8000_0000_0000_0001,
        ],
        [
            0x3f86_ffff_ffff_ffff_ffff,
            0x3f87_8000_0000_0000_0000,
            0x3f87_8000_0000_0000_0001,
        ],
    ];
    for rc in 0..4 {
        let cw = 0x037f | (rc << 10);
        for values in near {
            let inc = values.map(|y| atan2(ONE, y, cw).value);
            let dec_y = values.map(|y| atan2(SIGN | ONE, y, cw).value);
            let dec_x = values.map(|x| atan2(x, ONE, cw).value);
            assert!(inc[0] <= inc[1] && inc[1] <= inc[2], "{rc} {values:x?}");
            assert!(
                dec_y[0] >= dec_y[1] && dec_y[1] >= dec_y[2],
                "{rc} {values:x?}"
            );
            assert!(
                dec_x[0] >= dec_x[1] && dec_x[1] >= dec_x[2],
                "{rc} {values:x?}"
            );
        }
    }
}
