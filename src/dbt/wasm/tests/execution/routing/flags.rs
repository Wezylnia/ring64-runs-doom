use super::*;

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_graph_skips_nonzero_shift_status_before_register_writers() {
    use crate::dbt::wasm::execution::loops::loop_cfg::{
        CompareAction, LoopOrder, compare_predicates,
    };
    use crate::x86::instruction::Count;

    let code = [
        0xc1, 0xe8, 16, // shr eax, 16
        0x83, 0xe0, 127, // and eax, 127
        0xc1, 0xea, 9, // shr edx, 9
        0x74, 2, // je after_load
        0x8b, 0x0f, // mov ecx, [rdi]
        0x83, 0xee, 1, // after_load: sub esi, 1
        0x75, 0xee, // jne head
        0x90,
    ];
    let artifact = compiled_fixture(0x1000, &code, 0x1000);
    let mut instructions = artifact.instructions;
    let order = LoopOrder {
        earlier: 0,
        head: 0,
        tail: 6,
    };
    let context = crate::x86::context::ExecutionContext::default();
    assert_eq!(
        compare_predicates(&instructions, order, &context, true, None)[..3],
        [
            CompareAction::SkipStatus,
            CompareAction::SkipStatus,
            CompareAction::Ordinary
        ]
    );
    let Operation::Shift { ref mut count, .. } = instructions[2].operation else {
        unreachable!();
    };
    *count = Count::Immediate(32);
    assert_eq!(
        compare_predicates(&instructions, order, &context, true, None)[1],
        CompareAction::Ordinary
    );
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_forward_graph_skips_only_unobservable_arithmetic_status() {
    use crate::dbt::wasm::execution::loops::loop_cfg::{CompareAction, compare_predicates, select};
    use crate::embedding::layout::DATA;
    use crate::x86::instruction::Width;

    let code = [
        0x83, 0xe8, 1, // sub eax, 1
        0xf7, 0xd8, // neg eax
        0x89, 0xc2, // mov edx, eax
        0x01, 0xda, // add edx, ebx
        0x74, 2, // je after_load
        0x8b, 0x0f, // mov ecx, [rdi]
        0x83, 0xee, 1, // after_load: sub esi, 1
        0x75, 0xee, // jne head
        0x90,
    ];
    let artifact = compiled_fixture(0x1000, &code, 0x1000);
    let (order, _) = select(&artifact.instructions).unwrap();
    let context = crate::x86::context::ExecutionContext::default();
    let plan = compare_predicates(&artifact.instructions, order, &context, true, None);
    assert_eq!(
        plan[..4],
        [
            CompareAction::SkipStatus,
            CompareAction::SkipStatus,
            CompareAction::Ordinary,
            CompareAction::Ordinary,
        ]
    );
    assert_eq!(
        compare_predicates(&artifact.instructions, order, &context, false, None)[..2],
        [CompareAction::Ordinary; 2]
    );
    let mut with_carry = artifact.instructions.clone();
    with_carry[1].operation = Operation::Alu {
        kind: Alu::Adc,
        dst: RegisterView::low(0),
        src: DataOperand::Immediate(0),
        width: Width::W32,
    };
    assert_eq!(
        compare_predicates(&with_carry, order, &context, true, None)[0],
        CompareAction::Ordinary
    );
    let input = [(0, 2), (3, 0), (6, 2), (7, 0x8000)];
    let mut fault = execution_case(
        &artifact.bytes,
        &artifact.entries,
        &input,
        2,
        0x1000,
        40,
        (
            &[(0, 0xffffffff), (2, 0xffffffff)],
            0x86,
            0x100b,
            DATA as u32,
            35,
        ),
    );
    fault["accesses"] = json!([
        {"kind":"load","args":["8000",4],"result":"0"},
        {"kind":"access_failed","args":[],"result":DATA}
    ]);
    let partial = execution_case(
        &artifact.bytes,
        &artifact.entries,
        &input,
        0x8d7,
        0x1000,
        1,
        (&[(0, 1)], 2, 0x1003, BUDGET as u32, 0),
    );
    let mut stopped = execution_case(
        &artifact.bytes,
        &artifact.entries,
        &input,
        2,
        0x1000,
        40,
        (&[(0, 0xffffffff)], 0x97, 0x1005, STOP as u32, 38),
    );
    stopped["stop"] = json!("1005");
    run_cases(vec![fault, partial, stopped]);
}

#[test]
fn memory_compare_predicate_keeps_flags_at_faults_and_checked_entries() {
    use crate::dbt::wasm::execution::loops::loop_cfg::{
        CompareAction, ConditionSource, compare_predicates, select,
    };
    use crate::embedding::layout::DATA;

    let decrement = |bank| Operation::Alu {
        kind: Alu::Sub,
        dst: RegisterView::low(bank),
        src: DataOperand::Immediate(1),
        width: Width::W32,
    };
    let mut cases = Vec::new();
    for width in [Width::W32, Width::W64] {
        let operations = [
            Operation::MemoryAlu {
                kind: Alu::Cmp,
                address: Address {
                    base: AddressBase::Register(3),
                    ..Address::default()
                },
                operands: MemoryAluOperands::MemoryFirst(DataOperand::Immediate(2)),
                width,
            },
            Operation::Jcc {
                condition: Condition::Below,
                target: 0x1010,
            },
            Operation::Mov {
                dst: RegisterView::low(0),
                src: DataOperand::Immediate(7),
                width: Width::W32,
            },
            decrement(1),
            decrement(5),
            Operation::Jcc {
                condition: Condition::NotEqual,
                target: 0x1000,
            },
            Operation::Nop,
        ];
        let instructions: Vec<_> = operations
            .into_iter()
            .enumerate()
            .map(|(i, operation)| Instruction {
                pc: 0x1000 + i as u64 * 4,
                next_pc: 0x1004 + i as u64 * 4,
                operation,
            })
            .collect();
        let (order, max_lap) = select(&instructions).unwrap();
        assert_eq!(max_lap, 6);
        let context = crate::x86::context::ExecutionContext::default();
        assert_eq!(
            compare_predicates(&instructions, order, &context, true, None)[1],
            CompareAction::Condition(ConditionSource::Operands(width))
        );
        let mut faulting = instructions.clone();
        faulting[4].operation = Operation::Load {
            dst: RegisterView::low(1),
            address: Address::default(),
            width: Width::W32,
        };
        assert_eq!(
            compare_predicates(&faulting, order, &context, true, None)[1],
            CompareAction::Ordinary
        );
        let mut joined = instructions.clone();
        joined[5].operation = Operation::Jcc {
            condition: Condition::NotEqual,
            target: 0x0ffc,
        };
        joined.insert(
            0,
            Instruction {
                pc: 0x0ffc,
                next_pc: 0x1000,
                operation: Operation::Jcc {
                    condition: Condition::Parity,
                    target: 0x1004,
                },
            },
        );
        let (joined_order, _) = select(&joined).unwrap();
        assert_eq!(
            compare_predicates(&joined, joined_order, &context, true, None)[2],
            CompareAction::Ordinary
        );
        let unit = unit(instructions);
        let bytes = emit(&unit);
        assert_eq!(
            wasm_loop_count(&bytes),
            1 + usize::from(!cfg!(feature = "parallel"))
        );
        let entries: Vec<_> = unit.instructions.iter().map(|i| i.pc).collect();

        let wide_not_taken = width == Width::W64;
        let expected_taken = [(5, 0)];
        let expected_not_taken = [(0, 7), (1, 1), (5, 0)];
        let (wide_result, wide_remaining) = if wide_not_taken {
            (expected_not_taken.as_slice(), 1)
        } else {
            (expected_taken.as_slice(), 3)
        };
        for (entry, budget, stop, read, expected, flags, rip, reason, remaining) in [
            (
                0x1000,
                7,
                Some(0x1018),
                Some("1"),
                expected_taken.as_slice(),
                0x46,
                0x1018,
                STOP,
                3,
            ),
            (
                0x1000,
                7,
                Some(0x1018),
                Some("ffffffff00000001"),
                wide_result,
                0x46,
                0x1018,
                STOP,
                wide_remaining,
            ),
            (0x1000, 1, None, Some("1"), &[], 0x97, 0x1004, BUDGET, 0),
            (
                0x1000,
                7,
                Some(0x1004),
                Some("1"),
                &[],
                0x97,
                0x1004,
                STOP,
                6,
            ),
            (
                0x1004,
                7,
                Some(0x1018),
                None,
                expected_taken.as_slice(),
                0x46,
                0x1018,
                STOP,
                4,
            ),
            (0x1000, 7, None, Some("0"), &[], 0x803, 0x1000, DATA, 7),
        ] {
            let mut packet = execution_case(
                &bytes,
                &entries,
                &[(0, 0), (1, 2), (3, 0x8000), (5, 1)],
                0x803,
                entry,
                budget,
                (expected, flags, rip, reason as u32, remaining),
            );
            if let Some(stop) = stop {
                packet["stop"] = json!(format!("{stop:x}"));
            }
            if let Some(read) = read {
                packet["accesses"] = json!([
                    {"kind":"load","args":["8000",width.bytes()],"result":read},
                    {"kind":"access_failed","args":[],"result":if reason == DATA { DATA } else { 0 }}
                ]);
            }
            cases.push(packet);
        }
    }
    run_cases(cases);
}

#[test]
fn compare_predicate_restores_flags_only_on_its_fallthrough_load_fault() {
    use crate::dbt::wasm::execution::loops::loop_cfg::{
        CompareAction, ConditionSource, ReadFaultFlags, compare_predicates, select,
    };
    use crate::embedding::layout::DATA;

    let operations = [
        Operation::Alu {
            kind: Alu::Cmp,
            dst: RegisterView::low(0),
            src: DataOperand::Immediate(1),
            width: Width::W32,
        },
        Operation::Jcc {
            condition: Condition::Below,
            target: 0x1010,
        },
        Operation::Load {
            dst: RegisterView::low(0),
            address: Address {
                base: AddressBase::Register(3),
                ..Address::default()
            },
            width: Width::W32,
        },
        Operation::Mov {
            dst: RegisterView::low(1),
            src: DataOperand::Register(RegisterView::low(0)),
            width: Width::W32,
        },
        Operation::Alu {
            kind: Alu::Sub,
            dst: RegisterView::low(5),
            src: DataOperand::Immediate(1),
            width: Width::W32,
        },
        Operation::Jcc {
            condition: Condition::NotEqual,
            target: 0x1000,
        },
        Operation::Nop,
    ];
    let instructions: Vec<_> = operations
        .into_iter()
        .enumerate()
        .map(|(i, operation)| Instruction {
            pc: 0x1000 + i as u64 * 4,
            next_pc: 0x1004 + i as u64 * 4,
            operation,
        })
        .collect();
    let (order, _) = select(&instructions).unwrap();
    let context = crate::x86::context::ExecutionContext::default();
    let plan = compare_predicates(&instructions, order, &context, true, None);
    assert_eq!(
        plan[..3],
        [
            CompareAction::SkipStatus,
            CompareAction::Condition(ConditionSource::Operands(Width::W32)),
            CompareAction::RestoreOnLoadFault(ReadFaultFlags::Arithmetic(Alu::Cmp, Width::W32)),
        ]
    );

    let mut joined = instructions.clone();
    joined[5].operation = Operation::Jcc {
        condition: Condition::NotEqual,
        target: 0x0ffc,
    };
    joined.insert(
        0,
        Instruction {
            pc: 0x0ffc,
            next_pc: 0x1000,
            operation: Operation::Jcc {
                condition: Condition::Parity,
                target: 0x1008,
            },
        },
    );
    let (joined_order, _) = select(&joined).unwrap();
    assert_eq!(
        compare_predicates(&joined, joined_order, &context, true, None)[2],
        CompareAction::Ordinary
    );

    let bytes = emit(&unit(instructions.to_vec()));
    let entries: Vec<_> = instructions.iter().map(|i| i.pc).collect();
    let mut packets = Vec::new();
    for (entry, eax, budget, stop, read, expected, flags, reason, remaining) in [
        (
            0x1000,
            0x80000000,
            7,
            0x1018,
            Some(DATA),
            &[][..],
            0x816,
            DATA,
            5,
        ),
        (
            0x1000,
            0x80000000,
            7,
            0x1018,
            Some(0),
            &[(0, 0x1234), (1, 0x1234), (5, 0)][..],
            0x46,
            STOP,
            1,
        ),
        (0x1000, 0, 7, 0x1018, None, &[(5, 0)][..], 0x46, STOP, 3),
        (0x1000, 0x80000000, 7, 0x1008, None, &[][..], 0x816, STOP, 5),
        (
            0x1000,
            0x80000000,
            2,
            0x1018,
            None,
            &[][..],
            0x816,
            BUDGET,
            0,
        ),
        (
            0x1008,
            0x80000000,
            7,
            0x1018,
            Some(DATA),
            &[][..],
            0x803,
            DATA,
            7,
        ),
    ] {
        let rip = if reason == STOP { stop } else { 0x1008 };
        let mut packet = execution_case(
            &bytes,
            &entries,
            &[(0, eax), (3, 0x8000), (5, 1)],
            0x803,
            entry,
            budget,
            (expected, flags, rip, reason as u32, remaining),
        );
        packet["stop"] = json!(format!("{stop:x}"));
        if let Some(status) = read {
            packet["accesses"] = json!([
                {"kind":"load","args":["8000",4],"result":"1234"},
                {"kind":"access_failed","args":[],"result":status}
            ]);
        }
        packets.push(packet);
    }
    run_cases(packets);
}

#[test]
fn compare_predicate_restores_status_on_a_taken_loop_exit() {
    use crate::dbt::wasm::execution::loops::loop_cfg::{
        CompareAction, ConditionSource, compare_predicates, select,
    };
    use crate::embedding::layout::DATA;

    let artifact = compiled_fixture(
        0x1000,
        &[
            0x74, 0x07, // je 0x1009
            0x3b, 0x07, // cmp eax,[rdi]
            0x73, 0x09, // jae 0x100f
            0x0f, 0xb6, 0xd0, // movzx edx,al
            0x83, 0xe9, 0x01, // sub ecx,1
            0x75, 0xf2, // jne 0x1000
            0x90, 0x90,
        ],
        0x1000,
    );
    let context = crate::x86::context::ExecutionContext::default();
    let predicates = |instructions: &[Instruction]| {
        let (order, _) = select(instructions).unwrap();
        compare_predicates(instructions, order, &context, true, None)
    };
    assert_eq!(
        predicates(&artifact.instructions)[1..3],
        [
            CompareAction::SkipStatus,
            CompareAction::Condition(ConditionSource::OperandsWithTakenFlags(Width::W32)),
        ]
    );
    let mut flags_live = artifact.instructions;
    flags_live[4].operation = Operation::Nop;
    assert_eq!(predicates(&flags_live)[1], CompareAction::Ordinary);

    let continued = [(1, 0), (2, 0x80)];
    let mut cases = Vec::new();
    for (eax, entry, budget, stop, read, expected) in [
        (
            0x8000_0000,
            0x1000,
            8,
            0x100f,
            Some(("1", 0)),
            (&[][..], 0x816, 0x100f, STOP, 5),
        ),
        (
            0x80,
            0x1000,
            8,
            0x100e,
            Some(("ff", 0)),
            (&continued[..], 0x46, 0x100e, STOP, 2),
        ),
        (
            0x80,
            0x1004,
            8,
            0x100e,
            None,
            (&continued[..], 0x46, 0x100e, STOP, 4),
        ),
        (
            0x8000_0000,
            0x1000,
            2,
            0x100f,
            Some(("1", 0)),
            (&[][..], 0x816, 0x1004, BUDGET, 0),
        ),
        (
            0x8000_0000,
            0x1000,
            8,
            0x100f,
            Some(("1", DATA)),
            (&[][..], 0x803, 0x1002, DATA, 7),
        ),
    ] {
        let (registers, flags, rip, reason, remaining) = expected;
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &[(0, eax), (1, 1), (7, 0x8000)],
            0x803,
            entry,
            budget,
            (registers, flags, rip, reason as u32, remaining),
        );
        packet["stop"] = json!(format!("{stop:x}"));
        if let Some((value, status)) = read {
            packet["accesses"] = json!([
                {"kind":"load","args":["8000",4],"result":value},
                {"kind":"access_failed","args":[],"result":status}
            ]);
        }
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn conditional_move_preserves_sub_flags_across_an_intervening_scalar_load() {
    use crate::dbt::wasm::execution::loops::loop_cfg::{
        CompareAction, ConditionSource, ReadFaultFlags, compare_predicates, select,
    };
    use crate::embedding::layout::DATA;

    let artifact = compiled_fixture(
        0x1000,
        &[
            0x74, 0x08, // je 0x100a
            0x29, 0xf8, // sub eax,edi
            0x0f, 0xb6, 0x3b, // movzx edi,[rbx]
            0x0f, 0x4d, 0xf0, // cmovge esi,eax
            0x83, 0xe9, 0x01, // sub ecx,1
            0x75, 0xf1, // jne 0x1000
            0x90,
        ],
        0x1000,
    );
    let context = crate::x86::context::ExecutionContext::default();
    let predicates = |instructions: &[Instruction]| {
        let (order, _) = select(instructions).unwrap();
        compare_predicates(instructions, order, &context, true, None)
    };
    assert_eq!(
        predicates(&artifact.instructions)[1..4],
        [
            CompareAction::SkipStatus,
            CompareAction::RestoreOnLoadFault(ReadFaultFlags::Arithmetic(Alu::Sub, Width::W32)),
            CompareAction::Condition(ConditionSource::Operands(Width::W32)),
        ],
    );
    let mut joined = artifact.instructions;
    for target in [0x1004, 0x1007] {
        joined[0].operation = Operation::Jcc {
            condition: Condition::Equal,
            target,
        };
        assert_eq!(predicates(&joined)[1], CompareAction::Ordinary);
    }

    let complete = [(0, 0), (1, 0), (6, 0), (7, 0xff)];
    let sub = [(0, 0x7fff_ffff)];
    let loaded = [(0, 0x7fff_ffff), (7, 0xff)];
    let alternate_cmov = [(1, 0), (6, 0x8000_0000)];
    let mut cases = Vec::new();
    for (entry, eax, budget, stop, read, expected) in [
        (
            0x1000,
            1,
            8,
            0x100f,
            Some(0),
            (&complete[..], 0x46, 0x100f, STOP, 2),
        ),
        (
            0x1000,
            0x8000_0000,
            8,
            0x100f,
            Some(DATA),
            (&sub[..], 0x816, 0x1004, DATA, 6),
        ),
        (
            0x1000,
            0x8000_0000,
            2,
            0x100f,
            None,
            (&sub[..], 0x816, 0x1004, BUDGET, 0),
        ),
        (
            0x1000,
            0x8000_0000,
            8,
            0x1007,
            Some(0),
            (&loaded[..], 0x816, 0x1007, STOP, 5),
        ),
        (
            0x1004,
            0x8000_0000,
            8,
            0x100f,
            Some(DATA),
            (&[][..], 2, 0x1004, DATA, 8),
        ),
        (
            0x1007,
            0x8000_0000,
            8,
            0x100f,
            None,
            (&alternate_cmov[..], 0x46, 0x100f, STOP, 5),
        ),
    ] {
        let (registers, flags, rip, reason, remaining) = expected;
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &[(0, eax), (1, 1), (3, 0x8000), (6, 0xee), (7, 1)],
            2,
            entry,
            budget,
            (registers, flags, rip, reason as u32, remaining),
        );
        packet["stop"] = json!(format!("{stop:x}"));
        if let Some(status) = read {
            packet["accesses"] = json!([
                {"kind":"load","args":["8000",1],"result":"ff"},
                {"kind":"access_failed","args":[],"result":status}
            ]);
        }
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn guarded_backedge_uses_compare_operands_and_keeps_all_flags() {
    use crate::dbt::wasm::execution::loops::loop_cfg::{
        CompareAction, ConditionSource, compare_predicates, select,
    };

    let artifact = compiled_fixture(
        0x1000,
        &[
            0x74, 0x03, // je compare
            0x83, 0xc0, 1, // add eax,1
            0x39, 0xd8, // compare: cmp eax,ebx
            0x72, 0xf7, // jb head
            0x90,
        ],
        0x1000,
    );
    let context = crate::x86::context::ExecutionContext::default();
    let (order, _) = select(&artifact.instructions).unwrap();
    assert_eq!(
        compare_predicates(&artifact.instructions, order, &context, true, None)[2..4],
        [
            CompareAction::Ordinary,
            CompareAction::Condition(ConditionSource::Operands(Width::W32)),
        ],
    );
    let mut joined = artifact.instructions;
    joined[0].operation = Operation::Jcc {
        condition: Condition::Equal,
        target: 0x1007,
    };
    assert_eq!(
        compare_predicates(&joined, order, &context, true, None)[3],
        CompareAction::Ordinary
    );

    let mut cases = Vec::new();
    for (entry, eax, budget, stop, result, flags, rip, reason, remaining) in [
        (
            0x1000,
            0x7fff_fffe,
            4,
            0x1009,
            0x7fff_ffff,
            0x887,
            0x1000,
            BUDGET,
            0,
        ),
        (
            0x1000,
            0xffff_fffe,
            5,
            0x1009,
            0xffff_ffff,
            0x46,
            0x1009,
            STOP,
            1,
        ),
        (
            0x1000,
            0x7fff_fffe,
            5,
            0x1007,
            0x7fff_ffff,
            0x887,
            0x1007,
            STOP,
            2,
        ),
        (
            0x1007,
            0x7fff_ffff,
            5,
            0x1009,
            0x7fff_ffff,
            2,
            0x1009,
            STOP,
            4,
        ),
    ] {
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &[(0, eax), (3, 0xffff_ffff)],
            2,
            entry,
            budget,
            (&[(0, result)], flags, rip, reason as u32, remaining),
        );
        packet["stop"] = json!(format!("{stop:x}"));
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn guarded_shift_restores_flags_on_a_read_fault_before_compare_branch() {
    use crate::dbt::wasm::execution::loops::loop_cfg::{
        CompareAction, ConditionSource, ReadFaultFlags, compare_predicates, read_prefix_len, select,
    };
    use crate::embedding::layout::DATA;
    use crate::x86::instruction::{Count, Location, Shift};

    let head = 0x1009;
    let read_pc = 0x100e;
    let stop = 0x1017;
    let artifact = compiled_fixture(
        0x1000,
        &[
            0xeb, 0x07, // jmp head
            0x89, 0xc1, // below: mov ecx,eax
            0x83, 0xee, 1, // sub esi,1
            0x74, 0x0e, // je stop
            0xd1, 0xe8, // head: shr eax,1
            0x8d, 0x50, 1, // lea edx,[rax+1]
            0x39, 0x1f, // cmp [rdi],ebx
            0x72, 0xf0, // jb below
            0x83, 0xee, 1, // sub esi,1
            0x75, 0xf2, // jne head
            0x90,
        ],
        head,
    );
    let context = crate::x86::context::ExecutionContext::default();
    let (order, _) = select(&artifact.instructions).unwrap();
    assert_eq!(read_prefix_len(&artifact.instructions, order), 2);
    let predicates = |instructions: &[Instruction]| {
        compare_predicates(instructions, order, &context, true, None)
    };
    let plan = predicates(&artifact.instructions);
    assert_eq!(plan[0], CompareAction::SkipStatus);
    assert_eq!(
        plan[2],
        CompareAction::SkipStatusAndRestoreOnLoadFault(ReadFaultFlags::Shift(
            Shift::Shr,
            Width::W32,
        ))
    );
    assert_eq!(
        plan[3],
        CompareAction::Condition(ConditionSource::Operands(Width::W32))
    );

    let mut rejected = artifact.instructions;
    rejected[order.head as usize].operation = Operation::Shift {
        kind: Shift::Shr,
        dst: Location::Register(RegisterView::low(0)),
        width: Width::W32,
        count: Count::Immediate(32),
    };
    assert_eq!(predicates(&rejected)[0], CompareAction::Ordinary);
    let head_index = order.head as usize;
    rejected[head_index].operation = Operation::Shift {
        kind: Shift::Shr,
        dst: Location::Register(RegisterView::low(0)),
        width: Width::W32,
        count: Count::Immediate(1),
    };
    for target in [head + 2, read_pc] {
        rejected[order.at(3) as usize].operation = Operation::Jcc {
            condition: Condition::Below,
            target,
        };
        assert_eq!(predicates(&rejected)[0], CompareAction::Ordinary);
        assert_eq!(read_prefix_len(&rejected, order), 0);
    }

    let shifted = [(0, 0x4000_0000), (2, 0x4000_0001)];
    let below = [shifted[0], (1, 0x4000_0000), shifted[1], (6, 0)];
    let equal = [shifted[0], shifted[1], (6, 0)];
    let mut cases = Vec::new();
    for (entry, budget, stop_at, read, expected) in [
        (
            head,
            20,
            stop,
            Some(("1", 0)),
            (&below[..], 0x46, stop, STOP, 13),
        ),
        (
            head,
            20,
            stop,
            Some(("2", 0)),
            (&equal[..], 0x46, stop, STOP, 14),
        ),
        (
            head,
            20,
            stop,
            Some(("1", DATA)),
            (&shifted[..], 0x807, read_pc, DATA, 18),
        ),
        (
            head,
            2,
            stop,
            None,
            (&shifted[..], 0x807, read_pc, BUDGET, 0),
        ),
        (
            head,
            20,
            read_pc,
            None,
            (&shifted[..], 0x807, read_pc, STOP, 18),
        ),
        (
            read_pc,
            20,
            stop,
            Some(("1", DATA)),
            (&[][..], 0x803, read_pc, DATA, 20),
        ),
    ] {
        let (registers, flags, rip, reason, remaining) = expected;
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &[(0, 0x8000_0001), (3, 2), (6, 1), (7, 0x8000)],
            0x803,
            entry,
            budget,
            (registers, flags, rip, reason as u32, remaining),
        );
        packet["stop"] = json!(format!("{stop_at:x}"));
        if let Some((value, status)) = read {
            packet["accesses"] = json!([
                {"kind":"load","args":["8000",4],"result":value},
                {"kind":"access_failed","args":[],"result":status}
            ]);
        }
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn guarded_read_loop_publishes_preceding_add_flags_on_load_fault_and_budget() {
    use crate::embedding::layout::DATA;

    let artifact = compiled_fixture(
        0x1000,
        &[
            0x0f, 0xb6, 0x06, // movzx eax,[rsi]
            0x83, 0xc0, 0x01, // add eax,1; can defer flags until next load fails
            0x0f, 0xb6, 0x56, 0x01, // movzx edx,[rsi+1]
            0x83, 0xc2, 0x01, // add edx,1; overwrites flags
            0x0f, 0xb6, 0x5e, 0x02, // movzx ebx,[rsi+2]
            0x0f, 0xb6, 0x7e, 0x03, // movzx edi,[rsi+3]
            0x48, 0x83, 0xc6, 0x04, // add rsi,4
            0x48, 0x39, 0xce, // cmp rsi,rcx
            0x75, 0xe2, // jne head
            0x90,
        ],
        0x1000,
    );
    assert_eq!(
        wasm_loop_count(&artifact.bytes),
        1 + usize::from(!cfg!(feature = "parallel")),
    );
    let mut cases = Vec::new();
    for (budget, stop, reason, load_fails) in [
        (9, None, DATA, true),
        (2, None, BUDGET, false),
        (9, Some(0x1006), STOP, false),
    ] {
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &[(6, 0x8000), (1, 0x8004)],
            2,
            0x1000,
            budget,
            (&[(0, 0x100)], 0x16, 0x1006, reason as u32, budget - 2),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        let mut accesses = vec![
            json!({"kind":"load","args":["8000",1],"result":"ff"}),
            json!({"kind":"access_failed","args":[],"result":0}),
        ];
        if load_fails {
            accesses.extend([
                json!({"kind":"load","args":["8001",1],"result":"0"}),
                json!({"kind":"access_failed","args":[],"result":DATA}),
            ]);
        }
        packet["accesses"] = json!(accesses);
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn guarded_conditional_move_uses_comparison_and_result_sign_after_overflow() {
    use crate::dbt::wasm::execution::loops::loop_cfg::{
        CompareAction, ConditionSource, compare_predicates, select,
    };
    use crate::embedding::layout::DATA;
    use crate::x86::instruction::{Location, Unary};

    let operations = [
        Operation::Jcc {
            condition: Condition::Equal,
            target: 0x1018,
        },
        Operation::Alu {
            kind: Alu::Sub,
            dst: RegisterView::low(0),
            src: DataOperand::Immediate(1),
            width: Width::W32,
        },
        Operation::Cmov {
            dst: RegisterView::low(3),
            src: Location::Register(RegisterView::low(0)),
            width: Width::W32,
            condition: Condition::Less,
        },
        Operation::Alu {
            kind: Alu::Or,
            dst: RegisterView::low(2),
            src: DataOperand::Immediate(0),
            width: Width::W32,
        },
        Operation::Unary {
            kind: Unary::Neg,
            dst: Location::Register(RegisterView::low(2)),
            width: Width::W32,
        },
        Operation::Cmov {
            dst: RegisterView::low(6),
            src: Location::Register(RegisterView::low(2)),
            width: Width::W32,
            condition: Condition::Sign,
        },
        Operation::Alu {
            kind: Alu::Sub,
            dst: RegisterView::low(1),
            src: DataOperand::Immediate(1),
            width: Width::W32,
        },
        Operation::Jcc {
            condition: Condition::NotEqual,
            target: 0x1000,
        },
        Operation::Nop,
    ];
    let mut unit = unit(
        operations
            .into_iter()
            .enumerate()
            .map(|(i, operation)| Instruction {
                pc: 0x1000 + i as u64 * 4,
                next_pc: 0x1004 + i as u64 * 4,
                operation,
            })
            .collect(),
    );
    let bytes = emit(&unit);
    assert_eq!(
        wasm_loop_count(&bytes),
        1 + usize::from(!cfg!(feature = "parallel"))
    );
    let entries: Vec<_> = unit.instructions.iter().map(|i| i.pc).collect();
    let predicates = |unit: &Unit| {
        let (order, _) = select(&unit.instructions).unwrap();
        compare_predicates(&unit.instructions, order, &unit.context, true, None)
    };
    let sources = predicates(&unit);
    assert_eq!(sources[1], CompareAction::SkipStatus);
    assert_eq!(
        sources[2],
        CompareAction::Condition(ConditionSource::Operands(Width::W32))
    );
    assert_eq!(sources[4], CompareAction::SkipStatus);
    assert_eq!(
        sources[5],
        CompareAction::Condition(ConditionSource::ResultSign(Width::W32))
    );
    let mut cases = Vec::new();
    for (budget, next, flags, remaining, stop, expected) in [
        (
            8,
            0x1000,
            2,
            0,
            false,
            vec![
                (0, 0x7fffffff),
                (1, 1),
                (2, 0x80000000),
                (3, 0x7fffffff),
                (6, 0x80000000),
            ],
        ),
        (8, 0x1008, 0x816, 6, true, vec![(0, 0x7fffffff)]),
        (
            8,
            0x1014,
            0x887,
            3,
            true,
            vec![(0, 0x7fffffff), (2, 0x80000000), (3, 0x7fffffff)],
        ),
    ] {
        let mut packet = execution_case(
            &bytes,
            &entries,
            &[(0, 0x80000000), (1, 2), (2, 0x80000000)],
            2,
            0x1000,
            budget,
            (
                &expected,
                flags,
                next,
                if stop { STOP } else { BUDGET } as u32,
                remaining,
            ),
        );
        if stop {
            packet["stop"] = json!(format!("{next:x}"));
        }
        cases.push(packet);
    }

    let original_move = unit.instructions[2].operation;
    unit.instructions[2].operation = Operation::Cmov {
        dst: RegisterView::low(3),
        src: Location::Memory(Address {
            base: AddressBase::Register(7),
            ..Address::default()
        }),
        width: Width::W32,
        condition: Condition::Less,
    };
    assert_eq!(predicates(&unit)[1..=2], [CompareAction::Ordinary; 2]);
    let mut fault = execution_case(
        &emit(&unit),
        &entries,
        &[(0, 0x10), (1, 1), (7, 0x8000)],
        2,
        0x1000,
        8,
        (&[(0, 0xf)], 0x16, 0x1008, DATA as u32, 6),
    );
    // The signed condition is false, but the memory source still faults.
    fault["accesses"] = json!([
        {"kind":"load","args":["8000",4],"result":"0"},
        {"kind":"access_failed","args":[],"result":DATA}
    ]);
    cases.push(fault);
    unit.instructions[2].operation = original_move;

    let original_branch = unit.instructions[0].operation;
    unit.instructions[0].operation = Operation::Jcc {
        condition: Condition::Equal,
        target: 0x1008,
    };
    assert_eq!(predicates(&unit)[1..=2], [CompareAction::Ordinary; 2]);
    let mut joined = execution_case(
        &emit(&unit),
        &entries,
        &[(0, 0x1234), (1, 1), (2, 0x80000000)],
        0xc2,
        0x1000,
        8,
        (
            &[(1, 0), (2, 0x80000000), (3, 0x1234), (6, 0x80000000)],
            0x46,
            0x1020,
            STOP as u32,
            1,
        ),
    );
    joined["stop"] = json!("1020");
    cases.push(joined);
    unit.instructions[0].operation = original_branch;

    unit.instructions[3].operation = Operation::Alu {
        kind: Alu::Adc,
        dst: RegisterView::low(2),
        src: DataOperand::Immediate(0),
        width: Width::W32,
    };
    assert_eq!(predicates(&unit)[1..=2], [CompareAction::Ordinary; 2]);
    let mut carry = execution_case(
        &emit(&unit),
        &entries,
        &[(0, 0), (1, 1), (2, 0x7fffffff)],
        2,
        0x1000,
        9,
        (
            &[
                (0, 0xffffffff),
                (1, 0),
                (2, 0x80000000),
                (3, 0xffffffff),
                (6, 0x80000000),
            ],
            0x46,
            0x1020,
            STOP as u32,
            1,
        ),
    );
    carry["stop"] = json!("1020");
    cases.push(carry);
    run_cases(cases);
}
