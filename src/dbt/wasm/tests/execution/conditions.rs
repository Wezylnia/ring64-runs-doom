use super::*;

#[test]
fn deferred_parity_is_visible_to_conditions_lahf_and_exact_stop() {
    let artifact = compiled_fixture(
        0x1000,
        &[0x31, 0xc8, 0x9f, 0xf9, 0x7a, 2, 0xb2, 0x87, 0x90],
        0x1000,
    ); // xor eax,ecx; lahf; stc; jp +2; mov dl,0x87; nop
    let mut cases = Vec::new();
    for (result, status, taken) in [(1, 2, false), (3, 6, true), (0x800000ff, 0x86, true)] {
        let input = [(0, result), (1, 0), (2, 0)];
        let mut stop = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &input,
            0x602,
            0x1000,
            10,
            (&[], 0x600 | status, 0x1002, 2, 9),
        );
        stop["stop"] = json!("1002");
        cases.push(stop);
        let count = if taken { 5 } else { 6 };
        cases.push(execution_case(
            &artifact.bytes,
            &artifact.entries,
            &input,
            0x602,
            0x1000,
            count,
            (
                &[
                    (0, (result & !0xff00) | (status << 8)),
                    (2, if taken { 0 } else { 0x87 }),
                ],
                0x601 | status,
                0x1009,
                1,
                0,
            ),
        ));
    }
    run_cases(cases);
}

#[test]
fn sahf_replaces_deferred_parity_and_preserves_overflow() {
    let artifact = compiled_fixture(0x1000, &[0x01, 0xd1, 0x9e], 0x1000); // add ecx,edx; sahf
    run_cases(vec![execution_case(
        &artifact.bytes,
        &artifact.entries,
        &[(0, 0xd500), (1, 0x7fffffff), (2, 2)],
        0x602,
        0x1000,
        2,
        (&[(1, 0x80000001)], 0xed7, 0x1003, 1, 0),
    )]);
}

#[test]
fn scalar_float_compare_replaces_parity_only_on_success() {
    use crate::embedding::layout::SIMD_EXCEPTION;
    use crate::x86::float::{Format, Kind, Op as Numeric};
    let artifact = compiled_fixture(0x1000, &[0x31, 0xc8, 0x0f, 0x2e, 0xc1], 0x1000);
    let compare = Numeric {
        kind: Kind::Ucomi,
        format: Format::F32,
        scalar: true,
        integer64: false,
        predicate: 0,
    };
    let mut cases = Vec::new();
    for (status, next, result, remaining, flags) in [
        (0, 0x1005, 1, 0, 0x647),
        (SIMD_EXCEPTION, 0x1002, SIMD_EXCEPTION, 1, 0x602),
    ] {
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &[(0, 1), (1, 0)],
            0x602,
            0x1000,
            2,
            (&[], flags, next, result as u32, remaining),
        );
        let mut xmm = ["0"; 16];
        xmm[0] = "7f800001";
        packet["xmm"] = json!(xmm);
        packet["expected"]["xmm"] = packet["xmm"].clone();
        packet["accesses"] = json!([
            {"kind":"float", "args":[compare.encode(),"7f800001","0","0","0"],
             "result":{"reason":status,"value":"45"}}
        ]);
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn ordered_f32_flag_comparisons_preserve_xmm_and_mxcsr_without_a_helper() {
    let mut cases = Vec::new();
    for opcode in [0x2e, 0x2f] {
        let artifact = compiled_fixture(0x1000, &[0x0f, opcode, 0xc1], 0x1000);
        for (left, right, status) in [
            (0u32, 0x80000000u32, 0x40),
            (0x00800000, 0x7f7fffff, 1),
            (0x80800000, 0xff7fffff, 0),
            (0xff800000, 0x7f800000, 1),
            (0x7f800000, 0x7f800000, 0x40),
            (0x3f800000, 0x3f800001, 1),
        ] {
            let mut packet = execution_case(
                &artifact.bytes,
                &artifact.entries,
                &[],
                0xed7,
                0x1000,
                1,
                (&[], 0x602 | status, 0x1003, 1, 0),
            );
            let mut xmm = [0; 16];
            xmm[0] = 0x7f800001_80000001_ffffffff_00000000u128 | u128::from(left);
            xmm[1] = 0x7fc00001_ff800001_807fffff_00000000u128 | u128::from(right);
            packet["xmm"] = json!(xmm.map(|value| format!("{value:x}")));
            packet["expected"]["xmm"] = packet["xmm"].clone();
            packet["mxcsr"] = json!(0xa03f);
            packet["expected"]["mxcsr"] = packet["mxcsr"].clone();
            cases.push(packet);
        }
    }
    run_cases(cases);
}

#[test]
fn fault_and_debug_boundaries_publish_the_previous_retired_parity() {
    use crate::embedding::layout::{DATA, DEBUG_EXCEPTION};
    let artifact = compiled_fixture(0x1000, &[0x31, 0xc8, 0x8b, 0x03], 0x1000);
    let mut fault = execution_case(
        &artifact.bytes,
        &artifact.entries,
        &[(0, 3), (1, 0), (3, 0x4000)],
        2,
        0x1000,
        2,
        (&[], 6, 0x1002, DATA as u32, 1),
    );
    fault["accesses"] = json!([
        {"kind":"load", "args":["4000",4], "result":"0"},
        {"kind":"access_failed", "args":[], "result":1}
    ]);

    let alu = unit(vec![Instruction {
        pc: 0x1000,
        next_pc: 0x1002,
        operation: Operation::Alu {
            kind: Alu::Xor,
            dst: RegisterView::low(0),
            src: DataOperand::Register(RegisterView::low(1)),
            width: Width::W32,
        },
    }]);
    let mut debug = execution_case(
        &emit_observed(&alu, true),
        &[0x1000],
        &[(0, 1), (1, 0)],
        0x106,
        0x1000,
        2,
        (&[], 0x102, 0x1002, DEBUG_EXCEPTION as u32, 1),
    );
    debug["accesses"] = json!([
        {"kind":"debug_step", "args":["1000","1002",1,"102"], "result":DEBUG_EXCEPTION}
    ]);
    run_cases(vec![fault, debug]);
}

#[test]
fn every_condition_matches_independent_truth_masks_and_preserves_all_flags() {
    use Condition::*;
    // Bit n gives the condition for CF/PF/ZF/SF/OF encoded in n's five bits.
    let truth_masks = [
        (Overflow, 0xffff0000u32),
        (NotOverflow, 0x0000ffff),
        (Below, 0xaaaaaaaa),
        (AboveOrEqual, 0x55555555),
        (Equal, 0xf0f0f0f0),
        (NotEqual, 0x0f0f0f0f),
        (BelowOrEqual, 0xfafafafa),
        (Above, 0x05050505),
        (Sign, 0xff00ff00),
        (NotSign, 0x00ff00ff),
        (Parity, 0xcccccccc),
        (NotParity, 0x33333333),
        (Less, 0x00ffff00),
        (GreaterOrEqual, 0xff0000ff),
        (LessOrEqual, 0xf0fffff0),
        (Greater, 0x0f00000f),
    ];
    let mut cases = Vec::new();
    for (condition, truth_mask) in truth_masks {
        for next_pc in [0x1002, 0x1006] {
            for context in 0..32u64 {
                let flags = 2
                    | (context & 1)
                    | (((context >> 1) & 1) << 2)
                    | (((context >> 2) & 1) << 6)
                    | (((context >> 3) & 1) << 7)
                    | (((context >> 4) & 1) << 11);
                let taken = truth_mask & (1 << context) != 0;
                for af in [0, 0x10] {
                    cases.push(case(
                        vec![Instruction {
                            pc: 0x1000,
                            next_pc,
                            operation: Operation::Jcc {
                                condition,
                                target: 0x1010,
                            },
                        }],
                        &[],
                        flags | af,
                        0x1000,
                        1,
                        (&[], flags | af, if taken { 0x1010 } else { next_pc }, 1, 0),
                    ));
                }
            }
        }
    }
    run_cases(cases);
}

#[test]
fn mixed_conditions_and_middle_entries_use_the_current_flags() {
    let mut cases = Vec::new();
    let mixed = compiled_fixture(0x1000, &[0x7c, 2, 0x72, 2], 0x1000);
    assert_eq!(mixed.entries, [0x1000, 0x1002]);
    for (pc, flags, budget, next_pc) in [
        (0x1000, 3, 2, 0x1006),
        (0x1002, 0x12, 1, 0x1004),
        (0x1002, 0x13, 1, 0x1006),
    ] {
        cases.push(execution_case(
            &mixed.bytes,
            &mixed.entries,
            &[],
            flags,
            pc,
            budget,
            (&[], flags, next_pc, 1, 0),
        ));
    }
    let forward = compiled_fixture(
        0x1000,
        &[
            0x7a, 10, 0xb8, 17, 0, 0, 0, 0xb9, 34, 0, 0, 0, 0xba, 51, 0, 0, 0,
        ],
        0x1000,
    );
    assert_eq!(forward.entries, [0x1000, 0x1002, 0x1007, 0x100c]);
    for (flags, budget, stop, expected, next_pc, reason, remaining) in [
        (6, 2, None, vec![(2, 51)], 0x1011, 1, 0),
        (2, 4, None, vec![(0, 17), (1, 34), (2, 51)], 0x1011, 1, 0),
        (0x16, 1, None, vec![], 0x100c, 1, 0),
        (0x16, 3, Some(0x100c), vec![], 0x100c, 2, 2),
        (0x16, 1, Some(0x100c), vec![], 0x100c, 1, 0),
    ] {
        let mut packet = execution_case(
            &forward.bytes,
            &forward.entries,
            &[],
            flags,
            0x1000,
            budget,
            (&expected, flags, next_pc, reason, remaining),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        cases.push(packet);
    }
    let join = compiled_fixture(
        0x1000,
        &[
            0x74, 4, 0x31, 0xc0, 0xeb, 3, 0x83, 0xc8, 1, 0x74, 5, 0xba, 0x22, 0, 0, 0, 0xb9, 0x33,
            0, 0, 0,
        ],
        0x1000,
    );
    assert_eq!(
        join.entries,
        [0x1000, 0x1002, 0x1004, 0x1006, 0x1009, 0x100b, 0x1010]
    );
    for (flags, expected, result_flags) in [
        (2, vec![(0, 0), (1, 0x33)], 0x46),
        (0x42, vec![(0, 1), (1, 0x33), (2, 0x22)], 2),
    ] {
        let mut packet = execution_case(
            &join.bytes,
            &join.entries,
            &[],
            flags,
            0x1000,
            5,
            (&expected, result_flags, 0x1015, 1, 0),
        );
        packet["flagMask"] = json!("8c7");
        cases.push(packet);
    }
    for (flags, budget, expected) in [
        (0x52, 2, vec![(1, 0x33)]),
        (0x12, 3, vec![(1, 0x33), (2, 0x22)]),
    ] {
        cases.push(execution_case(
            &join.bytes,
            &join.entries,
            &[],
            flags,
            0x1009,
            budget,
            (&expected, flags, 0x1015, 1, 0),
        ));
    }
    run_cases(cases);
}

#[test]
fn conditional_target_frontiers_keep_budget_stop_and_target_validation_order() {
    let mut cases = Vec::new();
    let pc = 0x7ffffffffff0;
    let noncanonical = compiled_fixture(pc, &[0x78, 0x0e], pc);
    for (flags, budget, stop, next_pc, reason, remaining) in [
        (2, 1, None, pc + 2, 1, 0),
        (0x82, 1, None, pc, 5, 1),
        (0x92, 1, None, pc, 5, 1),
        (0x92, 0, Some(pc), pc, 1, 0),
        (0x92, 1, Some(pc), pc, 2, 1),
    ] {
        let mut packet = execution_case(
            &noncanonical.bytes,
            &noncanonical.entries,
            &[],
            flags,
            pc,
            budget,
            (&[], flags, next_pc, reason, remaining),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        cases.push(packet);
    }
    let pc = 0x7ffffffffffe;
    let fallthrough = compiled_fixture(pc, &[0x78, 0xfe], pc);
    for (flags, next_pc) in [(2, 0x800000000000), (0x92, pc)] {
        cases.push(execution_case(
            &fallthrough.bytes,
            &fallthrough.entries,
            &[],
            flags,
            pc,
            1,
            (&[], flags, next_pc, 1, 0),
        ));
    }
    let external = compiled_fixture(0x1000, &[0x7f, 0x0e], 0x1000);
    for (budget, stop, reason, remaining) in [
        (2, None, 0, 1),
        (2, Some(0x1010), 2, 1),
        (1, Some(0x1010), 1, 0),
    ] {
        let mut packet = execution_case(
            &external.bytes,
            &external.entries,
            &[],
            0x12,
            0x1000,
            budget,
            (&[], 0x12, 0x1010, reason, remaining),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn arithmetic_flags_are_visible_when_the_compare_is_not_admitted() {
    use crate::embedding::layout::{BUDGET, STOP};
    let entry = 0x1000;
    let compare = entry + 3;
    let end = compare + 3;
    let artifact = compiled_fixture(entry, &[0x83, 0xc0, 1, 0x83, 0xf8, 1], entry);
    let mut cases = Vec::new();
    for (budget, stop, flags, rip, reason, remaining) in [
        (1, None, 0x657, compare, BUDGET, 0),
        (1, Some(compare), 0x657, compare, BUDGET, 0),
        (2, Some(compare), 0x657, compare, STOP, 1),
        (2, None, 0x697, end, BUDGET, 0),
        (2, Some(end), 0x697, end, BUDGET, 0),
    ] {
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &[(0, 0xffff_ffff)],
            0x602,
            entry,
            budget,
            (&[(0, 0)], flags, rip, reason as u32, remaining),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        cases.push(packet);
    }
    cases.push(execution_case(
        &artifact.bytes,
        &artifact.entries,
        &[(0, 0)],
        0x602,
        compare,
        1,
        (&[], 0x697, end, BUDGET as u32, 0),
    ));
    run_cases(cases);
}
