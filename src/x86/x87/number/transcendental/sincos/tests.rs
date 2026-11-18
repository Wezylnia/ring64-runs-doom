use super::*;
use crate::x86::x87::number::{INDEFINITE, INFINITY, SIGN};

const ONE: u128 = 0x3fff_8000_0000_0000_0000;
const BEFORE_ONE: u128 = 0x3ffe_ffff_ffff_ffff_ffff;
const MIN_NORMAL: u128 = 0x0001_8000_0000_0000_0000;
const MAX_SUBNORMAL: u128 = 0x0000_7fff_ffff_ffff_ffff;

fn check(x: u128, cw: u16, expected: (u128, u128, u16, u16, bool)) {
    let (sine, cosine) = sincos(x, cw);
    assert_eq!(
        (sine.value, cosine, sine.pre, sine.post, sine.up),
        expected,
        "x={x:020x} cw={cw:04x}",
    );
    assert!(!sine.suppress);
}

#[test]
fn fsincos_exact_classes_and_range() {
    assert!(!sincos_out_of_range(0x403d_ffff_ffff_ffff_ffff));
    for sign in [0, SIGN] {
        let infinity = sign | INFINITY;
        let unsupported = sign | 0x7ffe_0000_0000_0000_0042;
        let quiet_nan = sign | 0x7fff_c000_0000_0000_0042;
        let signaling_nan = sign | 0x7fff_8000_0000_0000_0042;
        for x in [sign, infinity, unsupported, quiet_nan, signaling_nan] {
            assert!(!sincos_out_of_range(x), "x={x:020x}");
        }
        assert!(sincos_out_of_range(sign | 0x403e_8000_0000_0000_0000));
        assert!(sincos_out_of_range(sign | 0x7ffe_ffff_ffff_ffff_ffff));
        check(sign, 0x7f, (sign, ONE, 0, 0, false));
        check(infinity, 0x7f, (INDEFINITE, INDEFINITE, 1, 0, false));
        check(unsupported, 0x7f, (INDEFINITE, INDEFINITE, 1, 0, false));
        check(signaling_nan, 0x7f, (quiet_nan, quiet_nan, 1, 0, false));
        check(quiet_nan, 0x7f, (quiet_nan, quiet_nan, 0, 0, false));
    }
}

#[test]
fn fsincos_tiny_rounds_on_gradual_or_wrapped_grid() {
    for sign in [0, SIGN] {
        for (rc, cosine) in [ONE, BEFORE_ONE, ONE, BEFORE_ONE].into_iter().enumerate() {
            let cw = 0x037f | (rc as u16) << 10;
            let up = rc == 0 || rc == if sign == 0 { 2 } else { 1 };
            let (normal_sine, normal_wrapped, normal_flags) = if up {
                (MIN_NORMAL, MIN_NORMAL, 32)
            } else {
                (MAX_SUBNORMAL, 0x6000_ffff_ffff_ffff_ffff, 48)
            };
            let (sub_sine, sub_wrapped) = if up {
                (1, 0x5fc2_8000_0000_0000_0000)
            } else {
                (0, 0x5fc1_ffff_ffff_ffff_ffff)
            };
            check(sign | 1, cw, (sign | sub_sine, cosine, 2, 48, up));
            check(sign | 1, cw & !16, (sign | sub_wrapped, cosine, 2, 48, up));
            for (x, pre) in [(MIN_NORMAL, 0), (1 << 63, 2)] {
                check(
                    sign | x,
                    cw,
                    (sign | normal_sine, cosine, pre, normal_flags, up),
                );
                check(
                    sign | x,
                    cw & !16,
                    (sign | normal_wrapped, cosine, pre, normal_flags, up),
                );
            }
            for x in [sign | 1, sign | MAX_SUBNORMAL, sign | (1 << 63)] {
                let (sine, _) = sincos(x, cw & !2);
                assert_eq!((sine.pre, sine.post, sine.up), (2, 0, false));
            }
        }
    }
}

#[test]
fn fsincos_nearest_rounding_increment_comes_from_sine() {
    // At 1/4 cosine increments and sine does not; at 1/16 the reverse holds.
    for (x, sine, cosine, up) in [
        (
            0x3ffd_8000_0000_0000_0000,
            0x3ffc_fd57_76a7_98ab_b5d4,
            0x3ffe_f80a_a4fb_ef75_0ba8,
            false,
        ),
        (
            0x3ffb_8000_0000_0000_0000,
            0x3ffa_ffd5_5777_6a76_d5a6,
            0x3ffe_ff80_0aaa_4fa6_9a65,
            true,
        ),
        (
            0x3ffe_8000_0000_0000_0000,
            0x3ffd_f577_43a2_582f_7f44,
            0x3ffe_e0a9_4032_dbea_7cee,
            true,
        ),
        (
            0x403d_ffff_ffff_ffff_ffff,
            0x3ffe_e0ab_9300_da6d_2684,
            0x3ffd_f56e_c1e0_a37c_4176,
            false,
        ),
    ] {
        for sign in [0, SIGN] {
            for pc in [0, 2, 3] {
                check(sign | x, 0x7f | (pc << 8), (sign | sine, cosine, 0, 32, up));
            }
        }
    }
}

#[test]
fn fsincos_reduced_axis_preserves_the_period_residual_and_sign() {
    let x = 0x3fff_c90f_daa2_2168_c235;
    let cosine = SIGN | 0x3fbe_8000_0000_0000_0000;
    for sign in [0, SIGN] {
        for rc in 0..4 {
            let up = rc == 0 || rc == if sign == 0 { 2 } else { 1 };
            check(
                sign | x,
                0x037f | rc << 10,
                (
                    sign | if up { ONE } else { BEFORE_ONE },
                    cosine + u128::from(rc == 1),
                    0,
                    32,
                    up,
                ),
            );
        }
    }
}
