use super::*;

fn conversion(kind: Kind) -> Op {
    Op {
        kind,
        format: Format::F32,
        scalar: false,
        integer64: false,
        predicate: 0,
    }
}

#[test]
fn pair_conversions_ignore_inactive_lanes_without_hiding_active_conditions() {
    let left = 0x1122334455667788_99aabbccddeeff00;
    let poison = 0x7f8000017fc12345_3fc000003f800000;
    let masked = eval_pair(conversion(Kind::FloatToInt), left, poison, 0x1f80);
    assert_eq!(masked.value, 0x0000000200000001);
    assert_eq!(masked.mxcsr, 0x1fa0);
    assert_eq!(masked.raised, 0x20);
    assert_eq!(masked.stage, None);
    let fault = eval_pair(conversion(Kind::FloatToInt), left, poison, 0x0f00);
    assert_eq!(fault.value, left);
    assert_eq!(fault.mxcsr, 0x0f20);
    assert_eq!(fault.raised, 0x20);
    assert_eq!(fault.stage, Some(Stage::PostComputation));
}

#[test]
fn pair_conversion_keeps_whole_instruction_pre_and_post_exception_priority() {
    let input = 0x7f8000017fc12345_3fc000007fc12345;
    let left = 0x778899aabbccddeeff;
    for (mxcsr, raised, stage) in [
        (0x1f00, 1, Stage::PreComputation),
        (0x0f80, 0x21, Stage::PostComputation),
    ] {
        let result = eval_pair(conversion(Kind::FloatToInt), left, input, mxcsr);
        assert_eq!(result.value, left);
        assert_eq!(result.raised, raised);
        assert_eq!(result.mxcsr, mxcsr | raised);
        assert_eq!(result.stage, Some(stage));
    }
    let packed = eval_pair(
        conversion(Kind::IntToFloat),
        0,
        0x7fffffff80000000_feffffff01000001,
        0x1f80,
    );
    assert_eq!(packed.value, 0xcb8000004b800000);
    assert_eq!(packed.raised, 0x20);
}

#[test]
fn packed_integer_conversions_keep_lane_priority_and_aliased_destinations() {
    for (format, input) in [
        (Format::F32, 0xc0200000_80000001_7fc01234_3fc00000u128),
        (Format::F64, 0x7ff0000000000000_3ff8000000000000),
        (Format::F64, 0x8000000000000001_bfe0000000000000),
    ] {
        let count = 128 / format.bits();
        for index in 0..count {
            let source = input.rotate_left(index * format.bits());
            for kind in [Kind::FloatToInt, Kind::TruncToInt] {
                let op = Op {
                    format,
                    ..conversion(kind)
                };
                for controls in [0, 0x2040, 0x4000, 0x6040] {
                    for masks in [0x1f80, 0x1f00, 0x0f80, 0x0f00] {
                        let mxcsr = masks | controls | 4;
                        for old in [source, !source] {
                            assert_eq!(
                                eval(op, old, source, mxcsr),
                                eval_lanes(op, old, source, mxcsr, count),
                            );
                            assert_eq!(
                                eval_pair(op, old, source, mxcsr),
                                eval_lanes(op, old, source, mxcsr, 2),
                            );
                        }
                    }
                }
            }
        }
    }
}
