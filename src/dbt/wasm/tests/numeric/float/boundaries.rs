use super::*;

fn access(op: Numeric, left: u128, right: u128, result: u128, reason: u32, mxcsr: u32) -> Value {
    json!({"kind":"float","args":[op.encode(),format!("{:x}",left as u64),format!("{:x}",left>>64),format!("{:x}",right as u64),format!("{:x}",right>>64)],"result":{"value":format!("{result:x}"),"reason":reason,"mxcsr":mxcsr}})
}

fn packed_f32(values: [f32; 4]) -> u128 {
    values.iter().enumerate().fold(0, |bits, (lane, value)| {
        bits | (u128::from(value.to_bits()) << (32 * lane))
    })
}

#[test]
fn cmpnleps_accepts_ordered_lanes_with_live_mxcsr_and_keeps_memory_priority() {
    let left = packed_f32([-0.0, f32::INFINITY, f32::MIN_POSITIVE, f32::NEG_INFINITY]);
    let right = packed_f32([0.0, f32::MAX, 0.0, f32::NEG_INFINITY]);
    let ordered_mask = u128::from(u64::MAX) << 32;
    let address = Address {
        base: AddressBase::Register(0),
        ..Address::default()
    };
    let mut cases = Vec::new();
    for predicate in [6, 0xfe] {
        let op = Numeric {
            predicate,
            scalar: false,
            ..numeric(Kind::Cmp)
        };
        for mxcsr in [0x1f80, 0xffe5, 0x4000] {
            for src in [
                Location::Register(RegisterView::low(1)),
                Location::Memory(address),
            ] {
                let mut p = packet(
                    vec![Operation::Float(FloatInstruction { op, dst: 0, src })],
                    &[(0, left), (1, right)],
                    &[(0, ordered_mask)],
                    1,
                    1,
                );
                p["mxcsr"] = json!(mxcsr);
                p["expected"]["mxcsr"] = json!(mxcsr);
                if src == Location::Memory(address) {
                    p["accesses"] = json!([{
                        "kind": "vector_load",
                        "args": ["1122334400000000", 16, Segment::Ds as u32, 16],
                        "result": format!("{right:x}")
                    }]);
                }
                cases.push(p);
            }
        }
    }
    let op = Numeric {
        predicate: 6,
        scalar: false,
        ..numeric(Kind::Cmp)
    };
    let mut p = packet(
        vec![Operation::Float(FloatInstruction {
            op,
            dst: 0,
            src: Location::Memory(address),
        })],
        &[(0, packed_f32([f32::NAN; 4]))],
        &[],
        crate::embedding::layout::DATA as u32,
        0,
    );
    p["mxcsr"] = json!(0x1f00);
    p["expected"]["mxcsr"] = json!(0x1f00);
    p["accesses"] = json!([
        {"kind":"vector_load","args":["1122334400000000",16,Segment::Ds as u32,16],"result":null},
        {"kind":"access_reason","args":[],"result":crate::embedding::layout::DATA}
    ]);
    cases.push(p);
    run_cases(cases);
}

#[test]
fn cmpnleps_defers_nan_and_denormal_lanes_on_either_side() {
    let op = Numeric {
        predicate: 6,
        scalar: false,
        ..numeric(Kind::Cmp)
    };
    let operation = Operation::Float(FloatInstruction {
        op,
        dst: 0,
        src: Location::Register(RegisterView::low(1)),
    });
    let base_left = packed_f32([-0.0, f32::INFINITY, f32::MIN_POSITIVE, 0.0]);
    let base_right = packed_f32([0.0, f32::MAX, 0.0, 0.0]);
    let ordered_mask = u128::from(u64::MAX) << 32;
    let mut cases = Vec::new();
    for (bits, nan) in [
        (0x7fc0_0011u32, true),
        (0x7f80_0022u32, true),
        (0x0000_0001u32, false),
        (0x8000_0001u32, false),
    ] {
        let last_lane = u128::from(bits) << 96;
        for left_special in [true, false] {
            let (left, right) = if left_special {
                (base_left | last_lane, base_right)
            } else {
                (base_left, base_right | last_lane)
            };
            let masked = 0xdfa0;
            let exception = if nan { 1 } else { 2 };
            for mxcsr in [masked, masked & !(exception << 7), masked | 0x40] {
                let daz = mxcsr & 0x40 != 0;
                let raised = if nan || !daz { exception } else { 0 };
                let trap = mxcsr & (raised << 7) != raised << 7;
                let last_true = nan || !daz && ((bits >> 31 != 0) != left_special);
                let expected = if trap {
                    left
                } else if last_true {
                    ordered_mask | (u128::from(u32::MAX) << 96)
                } else {
                    ordered_mask
                };
                let outcome = crate::x86::float::eval(op, left, right, mxcsr);
                assert_eq!(outcome.value, expected);
                assert_eq!(outcome.raised, raised);
                assert_eq!(outcome.mxcsr, mxcsr | raised);
                assert_eq!(
                    outcome.stage,
                    trap.then_some(crate::x86::float::Stage::PreComputation)
                );
                let helper_reason = if trap { 13 } else { 0 };
                let mut p = packet(
                    vec![operation],
                    &[(0, left), (1, right)],
                    &[(0, expected)],
                    if trap { 13 } else { 1 },
                    u64::from(!trap),
                );
                p["mxcsr"] = json!(mxcsr);
                p["expected"]["mxcsr"] = json!(mxcsr | raised);
                p["accesses"] = json!([access(
                    op,
                    left,
                    right,
                    expected,
                    helper_reason,
                    mxcsr | raised
                )]);
                cases.push(p);
            }
        }
    }
    run_cases(cases);
}

#[test]
fn cmpnleps_keeps_the_numeric_helper_at_a_tight_artifact_cap() {
    let op = Numeric {
        predicate: 6,
        scalar: false,
        ..numeric(Kind::Cmp)
    };
    let operation = Operation::Float(FloatInstruction {
        op,
        dst: 0,
        src: Location::Register(RegisterView::low(1)),
    });
    let unit = unit(vec![instruction(0, operation)]);
    let direct = emit(&unit);
    let capped = emit_profile(&unit, false, false, direct.len() - 1);
    assert!(capped.len() < direct.len());
    let left = packed_f32([1.0; 4]);
    let right = packed_f32([0.0; 4]);
    let mut p = packet(
        vec![operation],
        &[(0, left), (1, right)],
        &[(0, u128::MAX)],
        1,
        1,
    );
    p["bytes"] = json!(capped);
    p["accesses"] = json!([access(op, left, right, u128::MAX, 0, 0x1f80)]);
    run_cases(vec![p]);
}

#[test]
fn packed_f32_sticky_precision_accepts_interior_lanes_and_provable_signed_zero() {
    let address = Address {
        base: AddressBase::Register(0),
        ..Address::default()
    };
    let mut cases = Vec::new();
    for (kind, left, right, result) in [
        (
            Kind::Add,
            [1.0, -0.0, 0.0, -4.0],
            [2.0, -0.0, -0.0, 4.0],
            [3.0, -0.0, 0.0, 0.0],
        ),
        (
            Kind::Sub,
            [3.0, -0.0, 0.0, -4.0],
            [2.0, 0.0, -0.0, -4.0],
            [1.0, -0.0, 0.0, 0.0],
        ),
        (
            Kind::Mul,
            [2.0, -0.0, 0.0, -4.0],
            [2.0, 3.0, -5.0, 0.0],
            [4.0, -0.0, -0.0, -0.0],
        ),
    ] {
        let [left, right, result] = [left, right, result].map(packed_f32);
        let op = Numeric {
            scalar: false,
            ..numeric(kind)
        };
        for src in [
            Location::Register(RegisterView::low(1)),
            Location::Memory(address),
        ] {
            let mut p = packet(
                vec![Operation::Float(FloatInstruction { op, dst: 0, src })],
                &[(0, left), (1, right)],
                &[(0, result)],
                1,
                1,
            );
            p["mxcsr"] = json!(0x9fe0);
            p["expected"]["mxcsr"] = p["mxcsr"].clone();
            if src == Location::Memory(address) {
                p["accesses"] = json!([{
                    "kind": "vector_load", "args": ["1122334400000000", 16, Segment::Ds as u32, 16],
                    "result": format!("{right:x}")
                }]);
            }
            cases.push(p);
        }
    }
    run_cases(cases);
}

#[test]
fn packed_f32_sticky_precision_excludes_nonzero_underflow_and_invalid_lanes() {
    let mut cases = Vec::new();
    for (kind, left, right, mxcsr) in [
        // multiplication can round to zero without an exactly zero operand
        (
            Kind::Mul,
            [2.0, 2.0, 2.0, f32::MIN_POSITIVE],
            [2.0, 2.0, 2.0, f32::MIN_POSITIVE],
            0x97a0,
        ),
        // a zero-cancellation proof must reject infinity minus itself
        (
            Kind::Sub,
            [2.0, 2.0, 2.0, f32::INFINITY],
            [2.0, 2.0, 2.0, f32::INFINITY],
            0x1f20,
        ),
        (
            Kind::Mul,
            [2.0, 2.0, 2.0, f32::INFINITY],
            [2.0, 2.0, 2.0, 0.0],
            0x1fa0,
        ),
        (
            Kind::Add,
            [2.0, 2.0, 2.0, f32::MAX],
            [2.0, 2.0, 2.0, f32::MAX],
            0x1fa0,
        ),
        (
            Kind::Add,
            [2.0, 2.0, 2.0, f32::from_bits(1)],
            [2.0, 2.0, 2.0, 0.0],
            0x1fe0,
        ),
        (
            Kind::Add,
            [2.0, 2.0, 2.0, 1.0],
            [2.0, 2.0, 2.0, f32::from_bits(0x33800000)],
            0x0fa0,
        ),
        (
            Kind::Sub,
            [2.0, 2.0, 2.0, 1.0],
            [2.0, 2.0, 2.0, 1.0],
            0x3fa0,
        ),
        (
            Kind::Sub,
            [2.0, 2.0, 2.0, 1.0],
            [2.0, 2.0, 2.0, 1.0],
            0x1f80,
        ),
    ] {
        let [left, right] = [left, right].map(packed_f32);
        let op = Numeric {
            scalar: false,
            ..numeric(kind)
        };
        let outcome = crate::x86::float::eval(op, left, right, mxcsr);
        let helper_reason = if outcome.stage.is_some() { 13 } else { 0 };
        let mut p = packet(
            vec![Operation::Float(FloatInstruction {
                op,
                dst: 0,
                src: Location::Register(RegisterView::low(1)),
            })],
            &[(0, left), (1, right)],
            &[(0, outcome.value)],
            if helper_reason == 0 { 1 } else { helper_reason },
            u64::from(helper_reason == 0),
        );
        p["mxcsr"] = json!(mxcsr);
        p["expected"]["mxcsr"] = json!(outcome.mxcsr);
        p["accesses"] = json!([access(
            op,
            left,
            right,
            outcome.value,
            helper_reason,
            outcome.mxcsr
        )]);
        cases.push(p);
    }
    run_cases(cases);
}

#[test]
fn tight_artifact_cap_restores_the_helper_for_packed_f32_arithmetic() {
    let add = Numeric {
        scalar: false,
        ..numeric(Kind::Add)
    };
    let operation = Operation::Float(FloatInstruction {
        op: add,
        dst: 0,
        src: Location::Register(RegisterView::low(1)),
    });
    let unit = unit(vec![instruction(0, operation)]);
    let direct = emit(&unit);
    let fallback = emit_profile(&unit, false, false, direct.len() - 1);
    assert!(fallback.len() < direct.len());
    let [left, right, result] = [
        [1.0, -2.0, -0.0, 0.0],
        [2.0, 2.0, -0.0, -0.0],
        [3.0, 0.0, -0.0, 0.0],
    ]
    .map(packed_f32);
    let mut p = packet(
        vec![operation],
        &[(0, left), (1, right)],
        &[(0, result)],
        1,
        1,
    );
    p["bytes"] = json!(fallback);
    p["mxcsr"] = json!(0x1fa0);
    p["expected"]["mxcsr"] = json!(0x1fa0);
    p["accesses"] = json!([access(add, left, right, result, 0, 0x1fa0)]);
    run_cases(vec![p]);
}

#[test]
fn tight_artifact_cap_keeps_direct_compare_but_uses_the_helper_for_subtraction() {
    let sub = numeric(Kind::Sub);
    let operations = [sub, numeric(Kind::Ucomi)].map(|op| {
        Operation::Float(FloatInstruction {
            op,
            dst: 0,
            src: Location::Register(RegisterView::low(1)),
        })
    });
    let unit = unit(
        operations
            .into_iter()
            .enumerate()
            .map(|(index, operation)| instruction(index as u64, operation))
            .collect(),
    );
    let direct = emit(&unit);
    let fallback = emit_profile(&unit, false, false, direct.len() - 1);
    assert!(fallback.len() < direct.len());
    let mut case = packet(
        operations.to_vec(),
        &[(0, 0x4040_0000), (1, 0x4000_0000)],
        &[(0, 0x3f80_0000)],
        1,
        2,
    );
    case["bytes"] = json!(fallback);
    case["expected"]["flags"] = json!("403");
    case["accesses"] = json!([access(
        sub,
        0x4040_0000,
        0x4000_0000,
        0x3f80_0000,
        0,
        0x1f80
    )]);
    run_cases(vec![case]);
}

#[test]
fn scalar_f32_add_subtract_and_multiply_preserve_lanes_and_precision_without_a_helper() {
    let mut cases = vec![];
    for (kind, a, b, result, inexact) in [
        (Kind::Add, 0x3f80_0000, 0x4000_0000, 0x4040_0000, false),
        (Kind::Add, 0x3f80_0000, 0x3380_0000, 0x3f80_0000, true),
        (Kind::Add, 0x3f80_0000, 0x0080_0000, 0x3f80_0000, true),
        (Kind::Add, 0x0080_0000, 0x3f80_0000, 0x3f80_0000, true),
        (Kind::Add, 0x3f80_0000, 0xbf80_0000, 0, false),
        (Kind::Add, 0x8000_0000, 0x8000_0000, 0x8000_0000, false),
        (Kind::Sub, 0x4040_0000, 0x4000_0000, 0x3f80_0000, false),
        (Kind::Sub, 0x3f80_0000, 0x0080_0000, 0x3f80_0000, true),
        (Kind::Sub, 0x0080_0000, 0x3f80_0000, 0xbf80_0000, true),
        (Kind::Sub, 0x0080_0000, 0xbf80_0000, 0x3f80_0000, true),
        (Kind::Sub, 0xbf80_0000, 0xbf80_0000, 0, false),
        (Kind::Sub, 0x8000_0000, 0, 0x8000_0000, false),
        (Kind::Sub, 0x8000_0000, 0x8000_0000, 0, false),
        (Kind::Sub, 0, 0x8000_0000, 0, false),
        (Kind::Mul, 0x3f80_0001, 0x3f80_0001, 0x3f80_0002, true),
        (Kind::Mul, 0xc040_0000, 0x4080_0000, 0xc140_0000, false),
        (Kind::Mul, 0x8000_0000, 0x4000_0000, 0x8000_0000, false),
    ] {
        let upper = 0x112233445566778899aabbcc00000000u128;
        let left = upper | a;
        let register_right = 0xaaaaaaaa55555555bbbbbbbb00000000u128 | b;
        let address = Address {
            base: AddressBase::Register(0),
            ..Address::default()
        };
        for (src, right, bank) in [
            (Location::Register(RegisterView::low(1)), register_right, 1),
            (Location::Memory(address), b, 1),
            (Location::Register(RegisterView::low(0)), left, 0),
        ] {
            if bank == 0 && a != b {
                continue;
            }
            for &mxcsr in if inexact {
                &[0x1f80, 0x9fe7][..]
            } else {
                &[0x1f80, 0x9fe7, 0x0f80][..]
            } {
                let mut p = packet(
                    vec![Operation::Float(FloatInstruction {
                        op: numeric(kind),
                        dst: 0,
                        src,
                    })],
                    &[(bank, right), (0, left)],
                    &[(0, upper | result)],
                    1,
                    1,
                );
                p["mxcsr"] = json!(mxcsr);
                p["expected"]["mxcsr"] = json!(mxcsr | if inexact { 32 } else { 0 });
                if src == Location::Memory(address) {
                    p["accesses"] = json!([{
                        "kind": "vector_load", "args": ["1122334400000000", 4, Segment::Ds as u32, 1],
                        "result": format!("{b:x}")
                    }]);
                }
                cases.push(p);
            }
        }
    }
    run_cases(cases);
}

#[test]
fn scalar_f32_add_subtract_and_multiply_defer_exceptional_operands_results_and_controls() {
    let address = Address {
        base: AddressBase::Register(0),
        ..Address::default()
    };
    let mut cases = vec![];
    for (kind, a, b, mxcsr) in [
        (Kind::Add, 0x3f80_0000, 0x3380_0000, 0x0f80), // Unmasked precision
        (Kind::Add, 0x3f80_0000, 0x0080_0000, 0x0fa0), // Unmasked with sticky precision
        (Kind::Mul, 0x3f80_0001, 0x3f80_0001, 0x0f80),
        (Kind::Add, 0x3f80_0000, 0x3380_0000, 0x5f80), // Round up
        (Kind::Add, 0x3f80_0000, 0xbf80_0000, 0x3f80), // Round down changes zero's sign
        (Kind::Add, 0x3f80_0000, 0x0000_0001, 0x1f80), // Denormal operand
        (Kind::Add, 0x3f80_0000, 0x0000_0001, 0x1fc0), // DAZ
        (Kind::Add, 0x7f80_0001, 0x3f80_0000, 0x1f00), // Signaling NaN, unmasked invalid
        (Kind::Add, 0xffc0_0002, 0x7fc0_0001, 0x1f80), // NaN payloads
        (Kind::Mul, 0x7f80_0000, 0x8000_0000, 0x1f80), // Infinity times zero
        (Kind::Add, 0x7f7f_ffff, 0x7f7f_ffff, 0x1f80), // Overflow
        (Kind::Mul, 0x0080_0000, 0x0080_0000, 0x1f80), // Tiny result rounded to zero
        (Kind::Mul, 0x0080_0000, 0x3f7f_ffff, 0x1f80), // Tiny result rounded to min-normal
        (Kind::Mul, 0x0080_0000, 0x3f00_0000, 0x9f80), // Exact subnormal result with FTZ
        (Kind::Add, 0x0080_0001, 0x8080_0000, 0x1780), // Unmasked underflow on cancellation
        (Kind::Sub, 0x0080_0000, 0x3f80_0000, 0x0fa0), // Unmasked lost LHS, sticky precision
        (Kind::Sub, 0x3f80_0000, 0x3f80_0000, 0x3f80), // Round down changes zero's sign
        (Kind::Sub, 0x3f80_0000, 0x0000_0001, 0x1fc0), // DAZ
        (Kind::Sub, 0x3f80_0000, 0xff80_0001, 0x1f00), // Right sNaN, unmasked invalid
        (Kind::Sub, 0x3f80_0000, 0xffc0_0002, 0x1f80), // Right qNaN preserves its sign
        (Kind::Sub, 0x7f80_0000, 0x7f80_0000, 0x1f80), // Infinity minus itself
        (Kind::Sub, 0x7f7f_ffff, 0xff7f_ffff, 0x1f80), // Overflow
        (Kind::Sub, 0x0080_0001, 0x0080_0000, 0x1780), // Unmasked underflow on cancellation
        (Kind::Sub, 0x0080_0001, 0x0080_0000, 0x9f80), // Exact subnormal result with FTZ
    ] {
        let op = numeric(kind);
        let left = 0x112233445566778899aabbcc00000000u128 | a;
        let result = crate::x86::float::eval(op, left, b, mxcsr);
        let helper_reason = if result.stage.is_some() { 13 } else { 0 };
        let operation = Operation::Float(FloatInstruction {
            op,
            dst: 0,
            src: Location::Memory(address),
        });
        let mut p = packet(
            vec![operation],
            &[(0, left)],
            &[(0, result.value)],
            if helper_reason == 0 { 1 } else { helper_reason },
            u64::from(helper_reason == 0),
        );
        p["mxcsr"] = json!(mxcsr);
        p["expected"]["mxcsr"] = json!(result.mxcsr);
        p["accesses"] = json!([
            {"kind":"vector_load","args":["1122334400000000",4,Segment::Ds as u32,1],"result":format!("{b:x}")},
            access(op, left, b, result.value, helper_reason, result.mxcsr),
        ]);
        cases.push(p);
    }
    run_cases(cases);
}

#[test]
fn numeric_middle_entries_skip_unexecuted_writers_and_capture_completed_predecessors() {
    let a = 0x11112222333344445555666677778888u128;
    let b = 0x9999aaaabbbbccccddddeeeeffff0000u128;
    let result = 0xfedcba98765432100123456789abcdefu128;
    let op = numeric(Kind::Div);
    let operation = |dst, src| {
        Operation::Float(FloatInstruction {
            op,
            dst,
            src: Location::Register(RegisterView::low(src)),
        })
    };
    let operations = vec![
        operation(8, 9),
        Operation::Jump { target: 0x100c },
        operation(15, 8),
        operation(7, 15),
    ];
    let mut cases = vec![];
    for (entry, budget) in [(0, 3), (1, 2), (2, 2), (3, 1)] {
        let mut changes = vec![(7, result)];
        if entry == 0 {
            changes.push((8, result));
        }
        if entry == 2 {
            changes.push((15, result));
        }
        let mut p = packet(
            operations.clone(),
            &[(7, a), (8, a), (9, b), (15, b)],
            &changes,
            1,
            budget,
        );
        p["pc"] = json!(format!("{:x}", 0x1000 + entry * 4));
        p["budget"] = json!(budget.to_string());
        p["expected"]["pc"] = json!("1010");
        p["expected"]["remaining"] = json!("0");
        let mut accesses = vec![];
        if entry == 0 {
            accesses.push(access(op, a, b, result, 0, 0x1fa0));
        }
        if entry == 2 {
            accesses.push(access(op, b, a, result, 0, 0x1fa0));
        }
        accesses.push(access(
            op,
            a,
            if entry == 2 { result } else { b },
            result,
            0,
            0x1fa0,
        ));
        p["accesses"] = json!(accesses);
        p["expected"]["mxcsr"] = json!(0x1fa0);
        cases.push(p);
    }
    run_cases(cases);
}

#[test]
fn numeric_exception_preserves_live_predecessor_flags_and_status_without_destination_publication() {
    let a = 0x11112222333344445555666677778888u128;
    let b = 0x9999aaaabbbbccccddddeeeeffff0000u128;
    let compare = numeric(Kind::Comi);
    let add = numeric(Kind::Add);
    let operations = vec![
        Operation::Float(FloatInstruction {
            op: compare,
            dst: 0,
            src: Location::Register(RegisterView::low(1)),
        }),
        Operation::Float(FloatInstruction {
            op: add,
            dst: 8,
            src: Location::Register(RegisterView::low(9)),
        }),
    ];
    let mut p = packet(operations, &[(0, a), (1, b), (8, a), (9, b)], &[], 13, 1);
    p["expected"]["flags"] = json!("447");
    p["expected"]["mxcsr"] = json!(0x1fa1);
    p["accesses"] = json!([
        access(compare, a, b, 0x45, 0, 0x1fa0),
        access(add, a, b, 0, 13, 0x1fa1)
    ]);
    run_cases(vec![p]);
}

#[test]
fn mxcsr_code_stores_retire_before_budget_stop_dispatch_and_failed_stores_leave_state_unchanged() {
    let address = Address {
        base: AddressBase::Register(0),
        ..Address::default()
    };
    let operations = vec![
        Operation::Mxcsr {
            address,
            store: true,
        },
        Operation::Nop,
    ];
    let mut cases = vec![];
    for (status, budget, stop, reason, retired) in [
        (0, 2, None, 6, 0),
        (1, 2, None, 1, 2),
        (2, 2, None, 0, 1),
        (2, 1, Some(0x1004), 1, 1),
        (2, 2, Some(0x1004), 2, 1),
    ] {
        let mut p = packet(operations.clone(), &[], &[], reason, retired);
        p["budget"] = json!(budget.to_string());
        p["expected"]["remaining"] = json!((budget - retired).to_string());
        if let Some(stop) = stop {
            p["stop"] = json!(format!("{stop:x}"));
        }
        let mut accesses = vec![
            json!({"kind":"store_mxcsr","args":["1122334400000000",Segment::Ds as u32,0x1f80],"result":status}),
        ];
        if status == 0 {
            accesses.push(json!({"kind":"access_reason","args":[],"result":6}));
        }
        p["accesses"] = json!(accesses);
        cases.push(p);
    }
    run_cases(cases);
}

#[test]
fn memory_to_integer_numeric_units_need_scratch_temporaries_without_xmm_banks() {
    let address = Address {
        base: AddressBase::Register(0),
        ..Address::default()
    };
    let mut cases = vec![];
    for kind in [Kind::FloatToInt, Kind::TruncToInt] {
        for format in [Format::F32, Format::F64] {
            for integer64 in [false, true] {
                let mut op = numeric(kind);
                op.format = format;
                op.integer64 = integer64;
                let mut p = packet(
                    vec![Operation::Float(FloatInstruction {
                        op,
                        dst: 0,
                        src: Location::Memory(address),
                    })],
                    &[],
                    &[],
                    1,
                    1,
                );
                p["expected"]["registers"][0] = json!("7fffffff");
                p["accesses"] = json!([
                    {"kind":"vector_load","args":["1122334400000000",if format==Format::F32{4}else{8},Segment::Ds as u32,1],"result":"12345678"},
                    {"kind":"float","args":[op.encode(),"0","0","12345678","0"],"result":{"reason":0,"value":"7fffffff"}}
                ]);
                cases.push(p);
            }
        }
    }
    run_cases(cases);
}
