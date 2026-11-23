use super::*;
use Kind::{Exp2MinusOne, MulLog2, MulLog2PlusOne};

const UNIT: u64 = 1 << 63;
const LOG2_E_DOWN: u64 = 0xb8aa_3b29_5c17_f0bb;

fn normal(exponent: i32, sig: u64) -> u128 {
    ((exponent + 0x3fff) as u128) << 64 | u128::from(sig)
}

#[test]
fn checked_coefficients_match_binary128_division() {
    let one = Quad::from_bits(QUAD_ONE);
    let mut factorial = 1;
    for (i, &bits) in EXP.iter().enumerate() {
        factorial *= (i + 1) as u128;
        assert_eq!(
            bits,
            (one / Quad::from_u128(factorial).value).value.to_bits()
        );
    }
    for (i, &bits) in LOG.iter().enumerate() {
        assert_eq!(
            bits,
            (one / Quad::from_u128((2 * i + 1) as u128).value)
                .value
                .to_bits()
        );
    }
}

#[test]
fn smallest_log1p_input_times_largest_power_of_two_survives_until_final_round() {
    // y*log2(1+x) is within 2^-16444 relative of 2^-62 * log2(e).
    // These adjacent binary80 magnitudes bracket the latter exact value.
    let y = normal(16383, UNIT);
    for sign_x in [0, SIGN] {
        for sign_y in [0, SIGN] {
            let sign = sign_x ^ sign_y;
            for rc in 0..4 {
                let result = evaluate(MulLog2PlusOne, sign_x | 1, sign_y | y, 0x37f | (rc << 10));
                let up = rc == 0 || (rc == 1 && sign != 0) || (rc == 2 && sign == 0);
                assert_eq!(
                    result.value,
                    sign | normal(-62, LOG2_E_DOWN + u64::from(up))
                );
                assert_eq!((result.pre, result.post, result.up), (0, 32, up));
            }
        }
    }
}

#[test]
fn subnormal_rounding_and_unmasked_result_exponents_use_full_scale() {
    for rc in 0..4 {
        let cw = 0x37f | (rc << 10);
        for sign in [0, SIGN] {
            let tiny = evaluate(Exp2MinusOne, sign | 1, 0, cw);
            let up = rc == 0 || (rc == 1 && sign != 0) || (rc == 2 && sign == 0);
            assert_eq!(tiny.value, sign | u128::from(up));
            assert_eq!((tiny.post, tiny.up), (48, up));
        }
    }
    for sign in [0, SIGN] {
        let masked = evaluate(MulLog2PlusOne, sign | 1, 1, 0x037f);
        assert_eq!((masked.value, masked.post), (sign, 48));
        let wrapped = evaluate(MulLog2PlusOne, sign | 1, 1, 0x036f);
        assert_eq!(
            wrapped.value,
            sign | normal(-2 * 16445 + 24576, LOG2_E_DOWN + 1)
        );
        assert_eq!(wrapped.post, 48);
    }

    let huge = evaluate(
        MulLog2,
        normal(2, UNIT),
        normal(16383, u64::MAX),
        0x37f & !8,
    );
    assert_eq!(huge.value, normal(16384 - 24576, u64::MAX));
    assert_eq!(huge.post, 8); // exact product before architectural overflow handling
}

fn ordered(raw: u128) -> u128 {
    if raw & SIGN == 0 {
        raw | SIGN
    } else {
        (SIGN - 1) - (raw & !SIGN)
    }
}

#[test]
fn adjacent_arguments_preserve_order_across_the_three_reductions() {
    let one = normal(0, UNIT);
    let tiny_boundary = [
        normal(-107, u64::MAX),
        normal(-106, UNIT),
        normal(-106, UNIT + 1),
    ];
    let mantissa_boundary = normal(0, UNIT | (1 << 62));
    let near_one = [normal(-1, u64::MAX), one, normal(0, UNIT + 1)];
    for rc in 0..4 {
        let cw = 0x37f | (rc << 10);
        for (kind, points) in [
            (Exp2MinusOne, tiny_boundary),
            (MulLog2PlusOne, tiny_boundary),
            (
                MulLog2,
                [
                    mantissa_boundary - 1,
                    mantissa_boundary,
                    mantissa_boundary + 1,
                ],
            ),
        ] {
            let values = points.map(|x| ordered(evaluate(kind, x, one, cw).value));
            assert!(
                values[0] <= values[1] && values[1] <= values[2],
                "{kind:?}, rc={rc}"
            );
        }
        // log2(1) itself is answered by the class owner before the finite kernel.
        assert!(evaluate(MulLog2, near_one[0], one, cw).value & SIGN != 0);
        assert!(evaluate(MulLog2, near_one[2], one, cw).value & SIGN == 0);
    }
}

#[test]
fn directed_rounding_cannot_jump_two_lower_ulps_across_a_binade() {
    let power = normal(0, UNIT);
    // A true result immediately below one may approximate just above it.
    // Returning 1+2^-63 would be two units of its 2^-64 grid.
    for fraction in [0, 1, 1 << (112 - 96), 1 << (112 - 95)] {
        let value = Quad::from_bits(QUAD_ONE | fraction);
        let positive = finish(value, 0, true, 0x0b7f);
        let negative = finish(-value, 0, true, 0x077f);
        assert_eq!(
            (positive.value, positive.post, positive.up),
            (power, 32, true)
        );
        assert_eq!(
            (negative.value, negative.post, negative.up),
            (SIGN | power, 32, true)
        );
    }
    // Exact rational products on the boundary are exempt.
    let exact = finish(Quad::from_bits(QUAD_ONE), 0, false, 0x037f);
    assert_eq!((exact.value, exact.post, exact.up), (power, 0, false));
}

#[test]
fn integer_log_products_match_exact_quad_at_ties_and_exponent_limits() {
    for (x, exponent) in [
        (1, -16445),
        (normal(-1, UNIT), -1),
        (normal(3, UNIT), 3),
        (normal(16383, UNIT), 16383),
    ] {
        for y in [
            1,
            normal(-16382, UNIT + 1),
            normal(0, UNIT + 1),
            normal(16383, u64::MAX),
        ] {
            for sign in [0, SIGN] {
                let y = y | sign;
                let (my, ey) = parts(y);
                let product = (my * Quad::from_i128(exponent).value).value;
                for masks in [0x037f, 0x0367] {
                    for rc in 0..4 {
                        let cw = masks | (rc << 10);
                        let got = evaluate(MulLog2, x, y, cw);
                        let expected = finish(product, ey, false, cw);
                        assert_eq!(
                            (got.value, got.post, got.up),
                            (expected.value, expected.post, expected.up)
                        );
                    }
                }
            }
        }
    }
}
