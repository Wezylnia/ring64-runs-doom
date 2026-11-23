use crate::dbt::wasm::tests::*;
use crate::embedding::layout::{BUDGET, DATA, DISPATCH, SEGMENT};
use crate::x86::context::{ExecutionContext, Segment};
use crate::x86::instruction::{Address, AddressBase, Bit, BitIndex, Condition, Location};

fn context(code32: bool, stack32: bool) -> ExecutionContext {
    let mut segments = serde_json::Map::new();
    for name in ["cs", "ss", "ds", "es", "fs", "gs"] {
        segments.insert(
            name.into(),
            json!({
                "base":"0x0", "limit":u32::MAX,
                "default32":if name == "ss" { stack32 } else { code32 },
                "usable":true, "type":if name == "cs" { "readable-code" } else { "read-write" }
            }),
        );
    }
    ExecutionContext::from_json(&json!({"mode":"protected","segments":segments})).unwrap()
}

fn packet(
    context: ExecutionContext,
    operation: Operation,
    input: &[(usize, u64)],
    outcome: Outcome<'_>,
) -> Value {
    let mut u = unit(vec![Instruction {
        pc: 0x1000,
        next_pc: 0x1003,
        operation,
    }]);
    u.context = context;
    execution_case(&emit(&u), &[0x1000], input, 0x8d7, 0x1000, 2, outcome)
}

#[test]
fn non64_register_writes_preserve_inactive_banks_and_high_dword() {
    let mut cases = vec![];
    for code32 in [false, true] {
        let mut u = unit(vec![Instruction {
            pc: 0x1000,
            next_pc: 0x1005,
            operation: Operation::Mov {
                dst: RegisterView::low(0),
                src: DataOperand::Immediate(0x89abcdef),
                width: Width::W32,
            },
        }]);
        u.context = context(code32, true);
        cases.push(execution_case(
            &emit(&u),
            &[0x1000],
            &[(0, 0xfedcba9876543210)],
            2,
            0x1000,
            1,
            (&[(0, 0xfedcba9889abcdef)], 2, 0x1005, BUDGET as u32, 0),
        ));
    }
    run_cases(cases);
}

#[test]
fn cmov_select_writes_the_high_dword_according_to_mode_for_both_predicates() {
    let mut cases = Vec::new();
    for context in [
        ExecutionContext::default(),
        context(true, true),
        context(false, true),
    ] {
        let mut u = unit(vec![Instruction {
            pc: 0x1000,
            next_pc: 0x1003,
            operation: Operation::Cmov {
                dst: RegisterView::low(0),
                src: Location::Register(RegisterView::low(1)),
                width: Width::W32,
                condition: Condition::Equal,
            },
        }]);
        let upper = if context.is_long64() {
            0
        } else {
            0xfedcba9800000000
        };
        u.context = context;
        let bytes = emit(&u);
        assert!(wasmparser::Parser::new(0).parse_all(&bytes).any(|part| {
            let wasmparser::Payload::CodeSectionEntry(body) = part.unwrap() else {
                return false;
            };
            body.get_operators_reader()
                .unwrap()
                .into_iter()
                .any(|op| matches!(op.unwrap(), wasmparser::Operator::Select))
        }));
        for (flags, low) in [(2, 0x76543210), (0x42, 0x89abcdef)] {
            cases.push(execution_case(
                &bytes,
                &[0x1000],
                &[(0, 0xfedcba9876543210), (1, 0x0123456789abcdef)],
                flags,
                0x1000,
                1,
                (&[(0, upper | low)], flags, 0x1003, BUDGET as u32, 0),
            ));
        }
    }
    run_cases(cases);
}

#[test]
fn protected_register_loop_preserves_imported_dwords_through_every_exit() {
    let head = 0x1000;
    let mut u = unit(
        [
            Operation::Alu {
                kind: Alu::Add,
                dst: RegisterView::low(0),
                src: DataOperand::Immediate(1),
                width: Width::W32,
            },
            Operation::Mov {
                dst: RegisterView::high_byte(0),
                src: DataOperand::Immediate(0x54),
                width: Width::W8,
            },
            Operation::Alu {
                kind: Alu::Sub,
                dst: RegisterView::low(1),
                src: DataOperand::Immediate(1),
                width: Width::W32,
            },
            Operation::Jcc {
                condition: Condition::NotEqual,
                target: head,
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(index, operation)| Instruction {
            pc: head + index as u64,
            next_pc: head + index as u64 + 1,
            operation,
        })
        .collect(),
    );
    u.context = context(true, true);
    let bytes = emit(&u);
    let entries = [head, head + 1, head + 2, head + 3];
    let mut cases = Vec::new();
    for (rax_high, rcx_high) in [
        (0x1122334400000000, 0x8877665500000000),
        (0, u64::MAX << 32),
    ] {
        let input = [(0, rax_high | 0x89abcdef), (1, rcx_high | 2)];
        for (start, budget, stop, eax, ecx, flags, next, reason, remaining) in [
            (head, 2, None, 0x89ab54f0, 2, 0x96, head + 2, BUDGET, 0),
            (head, 5, None, 0x89ab54f1, 1, 0x82, head + 1, BUDGET, 0),
            (head, 8, None, 0x89ab54f1, 0, 0x46, head + 4, BUDGET, 0),
            (
                head,
                9,
                Some(head + 4),
                0x89ab54f1,
                0,
                0x46,
                head + 4,
                crate::embedding::layout::STOP,
                1,
            ),
            (
                head,
                9,
                Some(head + 1),
                0x89abcdf0,
                2,
                0x96,
                head + 1,
                crate::embedding::layout::STOP,
                8,
            ),
            (head + 1, 1, None, 0x89ab54ef, 2, 2, head + 2, BUDGET, 0),
        ] {
            let writes = [(0, rax_high | eax), (1, rcx_high | ecx)];
            let mut packet = execution_case(
                &bytes,
                &entries,
                &input,
                2,
                start,
                budget,
                (&writes, flags, next, reason as u32, remaining),
            );
            if let Some(stop) = stop {
                packet["stop"] = json!(format!("{stop:x}"));
            }
            cases.push(packet);
        }
    }
    run_cases(cases);
}

#[test]
fn protected_unit_publishes_low_registers_on_faults_and_profile_exit() {
    let first = 0x1000;
    let operations = [
        Operation::Mov {
            dst: RegisterView::low(0),
            src: DataOperand::Immediate(0x89abcdef),
            width: Width::W32,
        },
        Operation::Load {
            dst: RegisterView::low(1),
            address: Address {
                base: AddressBase::Register(3),
                size: Width::W32,
                ..Address::default()
            },
            width: Width::W32,
        },
        Operation::Return {
            cleanup: 8,
            width: Width::W32,
        },
    ];
    let mut u = unit(
        operations
            .into_iter()
            .enumerate()
            .map(|(index, operation)| Instruction {
                pc: first + index as u64,
                next_pc: first + index as u64 + 1,
                operation,
            })
            .collect(),
    );
    u.context = context(true, true);
    let entries = [first, first + 1, first + 2];
    let writes_low_registers = |bytes: &[u8]| {
        wasmparser::Parser::new(0).parse_all(bytes).any(|part| {
            let wasmparser::Payload::CodeSectionEntry(body) = part.unwrap() else {
                return false;
            };
            body.get_operators_reader().unwrap().into_iter().any(|op| {
                matches!(op.unwrap(), wasmparser::Operator::I64Store32 { memarg }
                    if memarg.offset == std::mem::offset_of!(RunState, registers) as u64)
            })
        })
    };
    let bytes = emit(&u);
    assert_eq!(writes_low_registers(&bytes), !cfg!(feature = "parallel"));
    assert!(!writes_low_registers(&emit_observed(&u, true)));
    assert!(!writes_low_registers(&emit_profile(
        &u,
        false,
        true,
        usize::MAX
    )));

    let input = [(3, 0x332211ff00007000), (4, 0x8877665500008000)];
    let new_accumulator = [(0, 0x1122334489abcdef)];
    let mut load_fault = execution_case(
        &bytes,
        &entries,
        &input,
        2,
        first,
        3,
        (&new_accumulator, 2, first + 1, DATA as u32, 2),
    );
    load_fault["accesses"] = json!([
        {"kind":"segmented_load","args":["7000",4,2],"result":"0"},
        {"kind":"segmented_access_failed","args":[],"result":1},
        {"kind":"access_reason","args":[],"result":DATA}]);
    let mut return_fault = execution_case(
        &bytes,
        &entries,
        &input,
        2,
        first,
        3,
        (
            &[(0, 0x1122334489abcdef), (1, 0x1122334401020304)],
            2,
            first + 2,
            SEGMENT as u32,
            1,
        ),
    );
    return_fault["accesses"] = json!([
        {"kind":"segmented_load","args":["7000",4,2],"result":"1020304"},
        {"kind":"segmented_access_failed","args":[],"result":0},
        {"kind":"segmented_load","args":["8000",4,1],"result":"9000"},
        {"kind":"segmented_access_failed","args":[],"result":0},
        {"kind":"target_check","args":["9000"],"result":0}]);
    let profile_mismatch = execution_case(
        &bytes,
        &entries,
        &input,
        0x102,
        first,
        3,
        (&[], 0x102, first, DISPATCH as u32, 3),
    );
    run_cases(vec![load_fault, return_fault, profile_mismatch]);

    // The processor bridge can replace full banks even in protected mode.
    u.instructions[2].operation = Operation::Processor(crate::x86::processor::Instruction::Cpuid);
    assert!(!writes_low_registers(&emit(&u)));
}

#[test]
fn offsets_are_truncated_before_segment_helpers_or_long64_bases() {
    let mut cases = vec![];
    let mut p = packet(
        context(true, true),
        Operation::Load {
            dst: RegisterView::low(0),
            address: Address {
                base: AddressBase::Register(3),
                displacement: 0x30,
                size: Width::W16,
                segment: Segment::Fs,
                ..Address::default()
            },
            width: Width::W32,
        },
        &[(3, 0xaabbccddfffffff0)],
        (
            &[(0, 0x11223344deadbeef)],
            0x8d7,
            0x1003,
            DISPATCH as u32,
            1,
        ),
    );
    p["accesses"] = json!([
        {"kind":"segmented_load","args":["20",4,4],"result":"deadbeef"},
        {"kind":"segmented_access_failed","args":[],"result":0}]);
    cases.push(p);

    let long = ExecutionContext::from_json(
        &json!({"mode":"long64","fsBase":"0x100000000","gsBase":"0x0"}),
    )
    .unwrap();
    let mut p = packet(
        long.clone(),
        Operation::Load {
            dst: RegisterView::low(0),
            address: Address {
                base: AddressBase::Register(3),
                displacement: 0x30,
                size: Width::W32,
                segment: Segment::Fs,
                ..Address::default()
            },
            width: Width::W32,
        },
        &[(3, 0x11223344fffffff0)],
        (&[(0, 0xdeadbeef)], 0x8d7, 0x1003, DISPATCH as u32, 1),
    );
    p["accesses"] = json!([
        {"kind":"load","args":["100000020",4],"result":"deadbeef"},
        {"kind":"access_failed","args":[],"result":0}]);
    cases.push(p);

    let mut p = packet(
        long,
        Operation::Bit {
            kind: Bit::Test,
            dst: Location::Memory(Address {
                base: AddressBase::Register(3),
                size: Width::W32,
                segment: Segment::Fs,
                ..Address::default()
            }),
            index: BitIndex::Register(RegisterView::low(1)),
            width: Width::W32,
        },
        &[(3, 0), (1, 0xffffffff)],
        (&[], 0x43, 0x1003, DISPATCH as u32, 1),
    );
    p["accesses"] = json!([
        {"kind":"load","args":["1fffffffc",4],"result":"80000000"},
        {"kind":"access_failed","args":[],"result":0}]);
    cases.push(p);
    run_cases(cases);
}

#[test]
fn stack_slots_and_pointer_widths_remain_independent_including_pop_sp_aliases() {
    let mut cases = vec![];
    for (stack32, width, old, new, offset) in [
        (
            false,
            Width::W32,
            0xaabbccdd76540002,
            0xaabbccdd7654fffe,
            "fffe",
        ),
        (
            true,
            Width::W16,
            0xaabbccdd00000001,
            0xaabbccddffffffff,
            "ffffffff",
        ),
    ] {
        let mut p = packet(
            context(!stack32, stack32),
            Operation::Push {
                src: Operand::Register(4),
                width,
            },
            &[(4, old)],
            (&[(4, new)], 0x8d7, 0x1003, DISPATCH as u32, 1),
        );
        let value = old & width.mask();
        p["accesses"] = json!([{"kind":"segmented_store","args":[offset,width.bytes(),format!("{value:x}"),1],"result":1}]);
        cases.push(p);
    }
    for (stack32, width, old, loaded, new) in [
        (
            true,
            Width::W16,
            0xaabbccdd0000ffff,
            0x7654u64,
            0xaabbccdd00017654,
        ),
        (
            false,
            Width::W32,
            0xaabbccdd7654fffe,
            0x87654321,
            0xaabbccdd87654321,
        ),
    ] {
        let mut p = packet(
            context(true, stack32),
            Operation::Pop { dst: 4, width },
            &[(4, old)],
            (&[(4, new)], 0x8d7, 0x1003, DISPATCH as u32, 1),
        );
        let offset = old & if stack32 { 0xffffffff } else { 0xffff };
        p["accesses"] = json!([
            {"kind":"segmented_load","args":[format!("{offset:x}"),width.bytes(),1],"result":format!("{loaded:x}")},
            {"kind":"segmented_access_failed","args":[],"result":0}]);
        cases.push(p);
    }
    run_cases(cases);
}

#[test]
fn segmented_failures_and_target_checks_precede_architectural_commit() {
    let mut cases = vec![];
    for reason in [DATA, SEGMENT] {
        let mut p = packet(
            context(true, false),
            Operation::Pop {
                dst: 4,
                width: Width::W32,
            },
            &[(4, 0xaabbccdd12345678)],
            (&[], 0x8d7, 0x1000, reason as u32, 2),
        );
        p["accesses"] = json!([
            {"kind":"segmented_load","args":["5678",4,1],"result":"0"},
            {"kind":"segmented_access_failed","args":[],"result":1},
            {"kind":"access_reason","args":[],"result":reason}]);
        cases.push(p);
    }
    let mut p = packet(
        context(true, true),
        Operation::Call {
            target: Operand::Register(4),
            width: Width::W16,
        },
        &[(4, 0xaabbccddffff2345)],
        (&[], 0x8d7, 0x1000, SEGMENT as u32, 2),
    );
    p["accesses"] = json!([{"kind":"target_check","args":["2345"],"result":0}]);
    cases.push(p);

    let mut p = packet(
        context(true, false),
        Operation::Return {
            cleanup: 0xffff,
            width: Width::W16,
        },
        &[(4, 0xaabbccdd1234ffff)],
        (
            &[(4, 0xaabbccdd12340000)],
            0x8d7,
            0x2345,
            DISPATCH as u32,
            1,
        ),
    );
    p["accesses"] = json!([
        {"kind":"segmented_load","args":["ffff",2,1],"result":"2345"},
        {"kind":"segmented_access_failed","args":[],"result":0},
        {"kind":"target_check","args":["2345"],"result":1}]);
    cases.push(p);
    run_cases(cases);
}

#[test]
fn zero_count_segmented_rmw_uses_combined_status_before_any_commit() {
    let mut p = packet(
        context(true, false),
        Operation::Shift {
            kind: crate::x86::instruction::Shift::Shl,
            dst: Location::Memory(Address {
                base: AddressBase::Register(3),
                size: Width::W16,
                ..Address::default()
            }),
            count: crate::x86::instruction::Count::Immediate(0),
            width: Width::W32,
        },
        &[(3, 0xaabbccddffff7000)],
        (&[], 0x8d7, 0x1000, SEGMENT as u32, 2),
    );
    p["accesses"] = json!([
        {"kind":"segmented_load_for_write","args":["7000",4,2],"result":"0"},
        {"kind":"segmented_access_failed","args":[],"result":1},
        {"kind":"access_reason","args":[],"result":SEGMENT}]);
    run_cases(vec![p]);
}

#[test]
fn long64_faults_retain_selected_segments_without_hot_path_helper_calls() {
    let mut cases = vec![];
    for segment in [Segment::Ss, Segment::Ds, Segment::Fs] {
        let mut p = packet(
            ExecutionContext::default(),
            Operation::Load {
                dst: RegisterView::low(0),
                address: Address {
                    base: AddressBase::Register(5),
                    segment,
                    ..Address::default()
                },
                width: Width::W32,
            },
            &[(5, 0x8000)],
            (&[], 0x8d7, 0x1000, DATA as u32, 2),
        );
        p["accesses"] = json!([
            {"kind":"load","args":["8000",4],"result":"0"},
            {"kind":"access_failed","args":[],"result":1}]);
        p["faultSegment"] = json!(segment as u32);
        cases.push(p);
    }
    run_cases(cases);
}

#[test]
fn non64_code_writing_call_commits_stack_target_and_retirement_before_exit() {
    let mut p = packet(
        context(true, false),
        Operation::Call {
            target: Operand::Register(4),
            width: Width::W32,
        },
        &[(4, 0xaabbccdd00002004)],
        (
            &[(4, 0xaabbccdd00002000)],
            0x8d7,
            0x2004,
            DISPATCH as u32,
            1,
        ),
    );
    p["accesses"] = json!([
        {"kind":"target_check","args":["2004"],"result":1},
        {"kind":"segmented_store","args":["2000",4,"1003",1],"result":2}]);
    run_cases(vec![p]);
}
