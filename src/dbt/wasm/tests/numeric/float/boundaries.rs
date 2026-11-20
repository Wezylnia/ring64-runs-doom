use super::*;

fn access(op: Numeric, left: u128, right: u128, result: u128, reason: u32, mxcsr: u32) -> Value {
    json!({"kind":"float","args":[op.encode(),format!("{:x}",left as u64),format!("{:x}",left>>64),format!("{:x}",right as u64),format!("{:x}",right>>64)],"result":{"value":format!("{result:x}"),"reason":reason,"mxcsr":mxcsr}})
}

#[test]
fn scalar_f32_add_and_multiply_preserve_upper_lanes_and_precision_status_without_a_helper() {
    let mut cases = vec![];
    for (kind, a, b, result, inexact) in [
        (Kind::Add, 0x3f80_0000, 0x4000_0000, 0x4040_0000, false),
        (Kind::Add, 0x3f80_0000, 0x3380_0000, 0x3f80_0000, true),
        (Kind::Add, 0x3f80_0000, 0x0080_0000, 0x3f80_0000, true),
        (Kind::Add, 0x0080_0000, 0x3f80_0000, 0x3f80_0000, true),
        (Kind::Add, 0x3f80_0000, 0xbf80_0000, 0, false),
        (Kind::Add, 0x8000_0000, 0x8000_0000, 0x8000_0000, false),
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
fn scalar_f32_add_and_multiply_defer_exceptional_operands_results_and_controls() {
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
