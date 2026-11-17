use super::*;

pub(super) fn packed(kind: Kind, format: Format) -> Op {
    Op {
        kind,
        format,
        scalar: false,
        integer64: false,
        predicate: 0,
    }
}

pub(super) fn words(lanes: [u32; 4]) -> u128 {
    lanes
        .into_iter()
        .rev()
        .fold(0, |packet, word| (packet << 32) | u128::from(word))
}

#[test]
fn extended_kinds_keep_the_legacy_helper_encoding_and_lane_geometry() {
    for (kind, low) in [
        (Kind::AddSub, 0x80),
        (Kind::HorizontalAdd, 0x81),
        (Kind::HorizontalSub, 0x82),
    ] {
        for (format, selector) in [(Format::F32, 0), (Format::F64, 0x10)] {
            let op = packed(kind, format);
            assert_eq!(op.encode(), low | selector);
            assert_eq!(Op::decode(low | selector), op);
        }
    }
    for code in 0..=0x7f {
        assert_eq!(Op::decode(code | 0xdf00).encode(), code | 0xdf00);
    }

    for descriptor in [0xa1, 0xc1, 0x181, 0x8000_0081] {
        assert_eq!(Op::decode(descriptor).kind, Kind::Sub);
    }

    let ps = |lanes: [f32; 4]| words(lanes.map(f32::to_bits));
    let pd = |a: f64, b: f64| u128::from(a.to_bits()) | (u128::from(b.to_bits()) << 64);
    for (kind, sp, dp) in [
        (Kind::AddSub, [-1., 10., 5., 7.], [-1., 10.]),
        (Kind::HorizontalAdd, [23., 5., -2., 13.], [23., -2.]),
        (Kind::HorizontalSub, [-13., 11., 14., -7.], [-13., 14.]),
    ] {
        for (format, left, right, expected) in [
            (
                Format::F32,
                ps([5., 18., 8., -3.]),
                ps([6., -8., 3., 10.]),
                ps(sp),
            ),
            (Format::F64, pd(5., 18.), pd(6., -8.), pd(dp[0], dp[1])),
        ] {
            let got = eval(packed(kind, format), left, right, 0x1f80);
            assert_eq!(
                got,
                Outcome {
                    value: expected,
                    mxcsr: 0x1f80,
                    raised: 0,
                    stage: None
                }
            );
        }
    }
}

#[test]
fn horizontal_exception_arbitration_preserves_the_whole_original_destination() {
    let op = packed(Kind::HorizontalAdd, Format::F32);
    let left = words([0x7f7fffff, 0x7f7fffff, 0x3f800000, 0x33800000]);
    let right = words([0x7f800005, 0x3f800000, 0, 0]);
    let control = 0x1f80 & !(1 << 7);
    assert_eq!(
        eval(op, left, right, control),
        Outcome {
            value: left,
            mxcsr: control | 1,
            raised: 1,
            stage: Some(Stage::PreComputation)
        }
    );

    let control = 0x1f80 & !(1 << 12);
    assert_eq!(
        eval(op, left, 0, control),
        Outcome {
            value: left,
            mxcsr: control | 0x28,
            raised: 0x28,
            stage: Some(Stage::PostComputation)
        }
    );
}

#[test]
fn alternating_lanes_apply_local_operation_without_changing_nan_sign_or_rounding_control() {
    let op = packed(Kind::AddSub, Format::F32);
    let nan = words([0xffc00012, 0x7fc00013, 0xff800014, 0x7f800015]);
    let other = words([0x7fc00009; 4]);
    let got = eval(op, nan, other, 0x1f80);
    assert_eq!(
        got.value,
        words([0xffc00012, 0x7fc00013, 0xffc00014, 0x7fc00015])
    );
    assert_eq!((got.raised, got.stage), (1, None));

    let zeros = eval(op, 0, 0, 0x3f80);
    assert_eq!(zeros.value, words([0x80000000, 0, 0x80000000, 0]));
    let daz = eval(op, words([1; 4]), 0, 0x1fc0);
    assert_eq!((daz.value, daz.raised), (0, 0));
    let gradual = eval(op, words([1; 4]), 0, 0x1f80);
    assert_eq!((gradual.value, gradual.raised), (words([1; 4]), 2));
}
