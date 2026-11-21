use super::*;
use crate::embedding::layout::{BUDGET, DISPATCH, STOP};
use crate::x86::instruction::{Address, AddressBase, MemoryAluOperands};

#[test]
fn dynamic_entry_lookup_uses_full_pcs_and_the_artifacts_original_ordinals() {
    let high = 0xffff800000001000;
    // Source order deliberately differs from unsigned pc order. The lookup
    // returns the emitter's entry ordinal, never the sorted-key rank.
    let instructions = vec![
        Instruction {
            pc: 0x1000,
            next_pc: 0x1002,
            operation: Operation::JumpRegister {
                width: crate::x86::instruction::Width::W64,
                target: 0,
            },
        },
        Instruction {
            pc: high,
            next_pc: high + 10,
            operation: Operation::Mov {
                dst: RegisterView::low(2),
                src: DataOperand::Immediate(0x123456789abcdef),
                width: Width::W64,
            },
        },
        Instruction {
            pc: 0x1010,
            next_pc: 0x1015,
            operation: Operation::Mov {
                dst: RegisterView::low(1),
                src: DataOperand::Immediate(17),
                width: Width::W32,
            },
        },
        Instruction {
            pc: 0x10,
            next_pc: 0x15,
            operation: Operation::Mov {
                dst: RegisterView::low(3),
                src: DataOperand::Immediate(34),
                width: Width::W32,
            },
        },
    ];
    let mut cases = vec![];
    for (target, next, dst, value) in [
        (high, high + 10, 2, 0x123456789abcdef),
        (0x1010, 0x1015, 1, 17),
        (0x10, 0x15, 3, 34),
    ] {
        cases.push(case(
            instructions.clone(),
            &[(0, target)],
            0x8d7,
            0x1000,
            2,
            (&[(dst, value)], 0x8d7, next, BUDGET as u32, 0),
        ));
    }
    for miss in [0, 0x1001, 0x100001010, 0xffff800000001010] {
        cases.push(case(
            instructions.clone(),
            &[(0, miss)],
            0x8d7,
            0x1000,
            2,
            (&[], 0x8d7, miss, DISPATCH as u32, 1),
        ));
    }
    cases.push(case(
        instructions.clone(),
        &[(0, 0x1000)],
        0x8d7,
        0x1000,
        3,
        (&[], 0x8d7, 0x1000, BUDGET as u32, 0),
    ));
    cases.push(case(
        instructions.clone(),
        &[(0, high)],
        0x8d7,
        0x1000,
        1,
        (&[], 0x8d7, high, BUDGET as u32, 0),
    ));
    let mut stop = case(
        instructions,
        &[(0, 0x1010)],
        0x8d7,
        0x1000,
        3,
        (&[], 0x8d7, 0x1010, STOP as u32, 2),
    );
    stop["stop"] = json!("1010");
    cases.push(stop);
    run_cases(cases);
}

#[test]
fn entry_lookup_and_write_intent_helpers_keep_distinct_function_indices() {
    let mut p = case(
        vec![
            Instruction {
                pc: 0x1000,
                next_pc: 0x1004,
                operation: Operation::MemoryAlu {
                    kind: Alu::Add,
                    address: Address {
                        size: crate::x86::instruction::Width::W64,
                        segment: crate::x86::context::Segment::Ds,
                        base: AddressBase::Register(3),
                        index: None,
                        displacement: 0,
                    },
                    operands: MemoryAluOperands::MemoryFirst(DataOperand::Immediate(3)),
                    width: Width::W64,
                },
            },
            Instruction {
                pc: 0x1004,
                next_pc: 0x1006,
                operation: Operation::JumpRegister {
                    width: crate::x86::instruction::Width::W64,
                    target: 0,
                },
            },
            Instruction {
                pc: 0x1010,
                next_pc: 0x1015,
                operation: Operation::Mov {
                    dst: RegisterView::low(8),
                    src: DataOperand::Immediate(99),
                    width: Width::W32,
                },
            },
        ],
        &[(0, 0x1010), (3, 0x8000)],
        0x8d7,
        0x1000,
        3,
        (&[(8, 99)], 2, 0x1015, BUDGET as u32, 0),
    );
    p["accesses"] = json!([
        {"kind":"load_for_write","args":["8000",8],"result":"a"},
        {"kind":"access_failed","args":[],"result":0},
        {"kind":"store","args":["8000",8,"d"],"result":1}
    ]);
    run_cases(vec![p]);
}

#[test]
fn one_shared_lookup_keeps_many_return_entries_within_a_bounded_artifact() {
    let code: Vec<_> = (0..63)
        .flat_map(|_| [0xe8, 1, 0, 0, 0, 0xc3])
        .chain([0xc3])
        .collect();
    let image = crate::memory::Image::new(0x1000, code, 0x8000, vec![0; 1024]).unwrap();
    let artifact = crate::dbt::compile(
        &image,
        0x1000,
        crate::dbt::Limits {
            max_instructions: 128,
            max_source_bytes: 2048,
            max_artifact_bytes: 32768,
        },
    )
    .unwrap();
    assert_eq!(artifact.entries.len(), 127);
    wasmparser::Validator::new()
        .validate_all(&artifact.bytes)
        .unwrap();
}

const FLOAT_LOOP: [u8; 17] = [
    0x75, 4, // jnz into the loop's second instruction
    0xf3, 0x0f, 0x58, 0xc1, // addss xmm0, xmm1
    0xf3, 0x0f, 0x59, 0xc2, // mulss xmm0, xmm2
    0x83, 0xe9, 1, // sub ecx, 1
    0x75, 0xf3, // jnz to addss
    0xff, 0xe2, // jmp rdx
];

fn wasm_loop_count(bytes: &[u8]) -> usize {
    wasmparser::Parser::new(0)
        .parse_all(bytes)
        .filter_map(|part| match part.unwrap() {
            wasmparser::Payload::CodeSectionEntry(body) => Some(body),
            _ => None,
        })
        .map(|body| {
            body.get_operators_reader()
                .unwrap()
                .into_iter()
                .filter(|op| matches!(op, Ok(wasmparser::Operator::Loop { .. })))
                .count()
        })
        .sum()
}

fn executable_artifact_size(bytes: &[u8]) -> u32 {
    use wasm_encoder::{RawSection, Section};
    use wasmparser::{Parser, Payload};

    let hints = Parser::new(0)
        .parse_all(bytes)
        .find_map(|item| match item.unwrap() {
            Payload::CustomSection(section) if section.name() == "metadata.code.branch_hint" => {
                Some(section.range())
            }
            _ => None,
        });
    let mut encoded = Vec::new();
    if let Some(range) = hints {
        RawSection {
            id: 0,
            data: &bytes[range.start as usize..range.end as usize],
        }
        .append_to(&mut encoded);
    }
    (bytes.len() - encoded.len()) as u32
}

#[test]
#[cfg(not(feature = "parallel"))]
fn branch_hints_point_to_code_and_fit_the_artifact_limit() {
    use wasmparser::{KnownCustom, Operator, Parser, Payload};

    let code = [0x90, 0x90];
    let artifact = compiled_fixture(0x1000, &code, 0x1000);
    let mut sections = Parser::new(0).parse_all(&artifact.bytes);
    let hint_function = sections
        .find_map(|item| match item.unwrap() {
            Payload::CustomSection(section) => match section.as_known() {
                KnownCustom::BranchHints(hints) => hints.into_iter().next().map(Result::unwrap),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    let run = sections
        .find_map(|item| match item.unwrap() {
            Payload::CodeSectionEntry(body) => Some(body),
            _ => None,
        })
        .unwrap();
    let mut ops = run.get_operators_reader().unwrap();
    let mut branches = Vec::new();
    while !ops.eof() {
        let (op, offset) = ops.read_with_offset().unwrap();
        if matches!(op, Operator::If { .. } | Operator::BrIf { .. }) {
            branches.push(offset - run.range().start);
        }
    }
    assert!(hint_function.hints.count() >= 2);
    for hint in hint_function.hints {
        let hint = hint.unwrap();
        assert!(!hint.taken && branches.contains(&u64::from(hint.func_offset)));
    }

    let image = crate::memory::Image::new(0x1000, code.to_vec(), 0x8000, vec![0; 8]).unwrap();
    let capped = crate::dbt::compile(
        &image,
        0x1000,
        crate::dbt::Limits {
            max_instructions: 64,
            max_source_bytes: 256,
            max_artifact_bytes: artifact.bytes.len() as u32 - 1,
        },
    )
    .unwrap();
    let capped_run = Parser::new(0)
        .parse_all(&capped.bytes)
        .find_map(|item| match item.unwrap() {
            Payload::CustomSection(section) => {
                assert_ne!(section.name(), "metadata.code.branch_hint");
                None
            }
            Payload::CodeSectionEntry(body) => Some(body),
            _ => None,
        })
        .unwrap();
    assert_eq!(artifact.entries, capped.entries);
    assert_eq!(run.as_bytes(), capped_run.as_bytes());
}

#[test]
fn register_loop_reuses_the_checked_entries_for_partial_laps() {
    let code = [
        0xd1, 0xe0, // shl eax, 1
        0x31, 0xc3, // xor ebx, eax
        0x83, 0xe9, 1, // sub ecx, 1
        0x75, 0xf7, // jne head
        0x90,
    ];
    let artifact = compiled_fixture(0x1000, &code, 0x1000);
    assert_eq!(wasm_loop_count(&artifact.bytes), 2);
    let input = [(0, 0x40000001), (1, 2), (3, 0x40)];
    let mut cases = Vec::new();
    for (pc, budget, stop, writes, flags, next, reason, remaining) in [
        (
            0x1000,
            3,
            None,
            [0x80000002, 1, 0x80000042],
            2,
            0x1007,
            BUDGET,
            0,
        ),
        (
            0x1000,
            4,
            None,
            [0x80000002, 1, 0x80000042],
            2,
            0x1000,
            BUDGET,
            0,
        ),
        (
            0x1000,
            5,
            None,
            [4, 1, 0x80000042],
            0x803,
            0x1002,
            BUDGET,
            0,
        ),
        (0x1000, 8, None, [4, 0, 0x80000046], 0x46, 0x1009, BUDGET, 0),
        (
            0x1000,
            9,
            Some(0x1009),
            [4, 0, 0x80000046],
            0x46,
            0x1009,
            STOP,
            1,
        ),
        (
            0x1000,
            9,
            Some(0x1002),
            [0x80000002, 2, 0x40],
            0x882,
            0x1002,
            STOP,
            8,
        ),
        (
            0x1002,
            2,
            None,
            [0x40000001, 1, 0x40000041],
            2,
            0x1007,
            BUDGET,
            0,
        ),
    ] {
        let writes = [(0, writes[0]), (1, writes[1]), (3, writes[2])];
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &input,
            2,
            pc,
            budget,
            (&writes, flags, next, reason as u32, remaining),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        cases.push(packet);
    }
    run_cases(cases);

    let image = crate::memory::Image::new(0x1000, code.to_vec(), 0x8000, vec![0; 16]).unwrap();
    let limits = crate::dbt::Limits {
        max_instructions: 16,
        max_source_bytes: 64,
        max_artifact_bytes: executable_artifact_size(&artifact.bytes) - 1,
    };
    let fallback = crate::dbt::compile(&image, 0x1000, limits).unwrap();
    assert_eq!(wasm_loop_count(&fallback.bytes), 1);
    assert_eq!(fallback.entries, artifact.entries);
    let observed = crate::dbt::compile_with_context_observed(
        &image,
        &ExecutionContext::default(),
        0x1000,
        crate::dbt::Limits {
            max_artifact_bytes: 65536,
            ..limits
        },
        true,
    )
    .unwrap();
    assert_eq!(wasm_loop_count(&observed.bytes), 1);

    for excluded in [
        [0xd1, 0x20], // shl dword [rax], 1
        [0x01, 0x03], // add [rbx], eax
        [0x89, 0x03], // mov [rbx], eax
        [0x8b, 0x03], // mov eax, [rbx]
        [0xf7, 0x18], // neg dword [rax]
        [0xf7, 0xe0], // mul eax
        [0xf7, 0xf0], // div eax
    ] {
        let mut other = code;
        other[..2].copy_from_slice(&excluded);
        let artifact = compiled_fixture(0x1000, &other, 0x1000);
        assert_eq!(wasm_loop_count(&artifact.bytes), 1, "{excluded:02x?}");
    }
}

#[test]
fn register_loop_keeps_carry_live_through_zero_count_and_partial_writers() {
    let code = [
        0xd1, 0xe0, // shl eax, 1 (sets CF)
        0x89, 0xd1, // mov ecx, edx
        0xd3, 0xe0, // shl eax, cl (masked zero)
        0xd1, 0xd3, // rcl ebx, 1 (reads and sets CF)
        0x31, 0xc0, // xor eax, eax (clears CF)
        0x83, 0xd7, 0, // adc edi, 0 (reads CF)
        0x83, 0xee, 1, // sub esi, 1
        0x75, 0xee, // jne head
    ];
    let artifact = compiled_fixture(0x1000, &code, 0x1000);
    let input = [
        (0, 0x80000000),
        (1, 9),
        (2, 32),
        (3, 0x80000000),
        (6, 1),
        (7, 7),
    ];
    run_cases(vec![
        execution_case(
            &artifact.bytes,
            &artifact.entries,
            &input,
            2,
            0x1000,
            3,
            (&[(0, 0), (1, 32)], 0x847, 0x1006, BUDGET as u32, 0),
        ),
        execution_case(
            &artifact.bytes,
            &artifact.entries,
            &input,
            2,
            0x1000,
            8,
            (
                &[(0, 0), (1, 32), (3, 1), (6, 0)],
                0x46,
                0x1012,
                BUDGET as u32,
                0,
            ),
        ),
    ]);
}

#[test]
fn float_backedge_uses_a_structured_loop_with_size_fallback() {
    use crate::dbt::{Limits, compile};

    let artifact = compiled_fixture(0x1000, &FLOAT_LOOP, 0x1000);
    let expected_loops = 1 + usize::from(!cfg!(feature = "parallel"));
    assert_eq!(wasm_loop_count(&artifact.bytes), expected_loops);
    assert_eq!(
        artifact.entries,
        [0x1000, 0x1002, 0x1006, 0x100a, 0x100d, 0x100f]
    );

    let image =
        crate::memory::Image::new(0x1000, FLOAT_LOOP.to_vec(), 0x8000, vec![0; 16]).unwrap();
    let fallback = compile(
        &image,
        0x1000,
        Limits {
            max_instructions: 16,
            max_source_bytes: 64,
            max_artifact_bytes: executable_artifact_size(&artifact.bytes)
                - u32::from(expected_loops == 2),
        },
    )
    .unwrap();
    assert_eq!(wasm_loop_count(&fallback.bytes), 1);
    assert_eq!(fallback.entries, artifact.entries);
    let observed = crate::dbt::compile_with_context_observed(
        &image,
        &ExecutionContext::default(),
        0x1000,
        Limits {
            max_instructions: 16,
            max_source_bytes: 64,
            max_artifact_bytes: 65536,
        },
        true,
    )
    .unwrap();
    assert_eq!(wasm_loop_count(&observed.bytes), 1);
}

fn float_loop_state(packet: &mut Value, result: f32) {
    let upper = 0x112233445566778899aabbcc00000000u128;
    let data: [u128; 16] = std::array::from_fn(|bank| {
        upper
            | u128::from(if bank == 1 {
                1.0f32.to_bits()
            } else {
                2.0f32.to_bits()
            })
    });
    let mut expected = data;
    expected[0] = upper | u128::from(result.to_bits());
    packet["xmm"] = json!(data.map(|v| format!("{v:x}")));
    packet["expected"]["xmm"] = json!(expected.map(|v| format!("{v:x}")));
    packet["mxcsr"] = json!(0x1f80);
    packet["expected"]["mxcsr"] = json!(0x1f80);
}

#[test]
fn float_backedge_preserves_retirement_stops_and_alternate_entry() {
    let artifact = compiled_fixture(0x1000, &FLOAT_LOOP, 0x1000);
    let mut cases = Vec::new();
    for (entry, flags, budget, stop, ecx, next, result, final_flags, reason, remaining) in [
        (0x1002, 2, 1, None, 2, 0x1006, 3.0, 2, BUDGET, 0),
        (0x1002, 2, 2, None, 2, 0x100a, 6.0, 2, BUDGET, 0),
        (0x1002, 2, 3, None, 1, 0x100d, 6.0, 2, BUDGET, 0),
        (0x1002, 2, 4, None, 1, 0x1002, 6.0, 2, BUDGET, 0),
        (0x1002, 2, 8, Some(0x100d), 1, 0x100d, 6.0, 2, STOP, 5),
        (0x1002, 2, 8, None, 0, 0x100f, 14.0, 0x46, BUDGET, 0),
        (0x1006, 2, 2, None, 1, 0x100d, 4.0, 2, BUDGET, 0),
        (0x1000, 2, 3, None, 1, 0x100d, 4.0, 2, BUDGET, 0),
        (0x1000, 0x42, 2, None, 2, 0x1006, 3.0, 0x42, BUDGET, 0),
        (0x100f, 2, 3, None, 1, 0x100d, 4.0, 2, BUDGET, 0),
    ] {
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &[(1, 2), (2, 0x1006)],
            flags,
            entry,
            budget,
            (&[(1, ecx)], final_flags, next, reason as u32, remaining),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        float_loop_state(&mut packet, result);
        cases.push(packet);
    }

    let mut numeric_fault = execution_case(
        &artifact.bytes,
        &artifact.entries,
        &[(1, 2)],
        2,
        0x1002,
        8,
        (
            &[],
            2,
            0x1006,
            crate::embedding::layout::SIMD_EXCEPTION as u32,
            7,
        ),
    );
    float_loop_state(&mut numeric_fault, 3.0);
    numeric_fault["xmm"][2] = json!("7f800001");
    numeric_fault["expected"]["xmm"][2] = json!("7f800001");
    numeric_fault["mxcsr"] = json!(0x1f00);
    numeric_fault["expected"]["mxcsr"] = json!(0x1f01);
    let multiply = crate::x86::float::Op {
        kind: crate::x86::float::Kind::Mul,
        format: crate::x86::float::Format::F32,
        scalar: true,
        integer64: false,
        predicate: 0,
    };
    numeric_fault["accesses"] = json!([{
        "kind": "float",
        "args": [multiply.encode(), "99aabbcc40400000", "1122334455667788", "7f800001", "0"],
        "result": {"reason": crate::embedding::layout::SIMD_EXCEPTION, "value": "0", "mxcsr": 0x1f01}
    }]);
    cases.push(numeric_fault);

    let mut memory = FLOAT_LOOP;
    memory[9] = 0x02; // mulss xmm0, [rdx]
    let artifact = compiled_fixture(0x1000, &memory, 0x1000);
    let mut denied = execution_case(
        &artifact.bytes,
        &artifact.entries,
        &[(1, 2), (2, 0x8000)],
        2,
        0x1002,
        8,
        (&[], 2, 0x1006, crate::embedding::layout::DATA as u32, 7),
    );
    float_loop_state(&mut denied, 3.0);
    denied["accesses"] = json!([
        {"kind":"vector_load","args":["8000",4,Segment::Ds as u32,1],"result":null},
        {"kind":"access_reason","args":[],"result":crate::embedding::layout::DATA}
    ]);
    cases.push(denied);
    run_cases(cases);
}

#[test]
fn read_only_integer_backedge_hoists_gates_without_changing_load_faults() {
    use crate::embedding::layout::DATA;

    let artifact = compiled_fixture(
        0x1000,
        &[
            0x0f, 0xb6, 0x06, // movzx eax,[rsi]
            0x0f, 0xb6, 0x56, 0x01, // movzx edx,[rsi+1]
            0x0f, 0xb6, 0x5e, 0x02, // movzx ebx,[rsi+2]
            0x0f, 0xb6, 0x7e, 0x03, // movzx edi,[rsi+3]
            0x48, 0x83, 0xc6, 0x04, // add rsi,4
            0x48, 0x39, 0xce, // cmp rsi,rcx
            0x75, 0xe8, // jne head
            0x90,
        ],
        0x1000,
    );
    assert_eq!(
        wasm_loop_count(&artifact.bytes),
        1 + usize::from(!cfg!(feature = "parallel")),
    );
    let mut cases = Vec::new();
    for (budget, retired, next, flags, reason, expected, stop) in [
        (
            7,
            7,
            0x1000,
            0x97,
            BUDGET,
            vec![(0, 1), (2, 2), (3, 3), (7, 4), (6, 0x8004)],
            None,
        ),
        (
            14,
            14,
            0x1018,
            0x46,
            BUDGET,
            vec![(0, 5), (2, 6), (3, 7), (7, 8), (6, 0x8008)],
            None,
        ),
        (2, 2, 0x1007, 2, BUDGET, vec![(0, 1), (2, 2)], None),
        (8, 1, 0x1003, 2, STOP, vec![(0, 1)], Some(0x1003)),
        (
            14,
            8,
            0x1003,
            0x97,
            DATA,
            vec![(0, 5), (2, 2), (3, 3), (7, 4), (6, 0x8004)],
            None,
        ),
    ] {
        let mut packet = execution_case(
            &artifact.bytes,
            &artifact.entries,
            &[(6, 0x8000), (1, 0x8008)],
            2,
            0x1000,
            budget,
            (&expected, flags, next, reason as u32, budget - retired),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        let loads = match retired {
            7 => 4,
            14 => 8,
            8 => 6,
            n => n,
        };
        packet["accesses"] = json!((0..loads)
            .flat_map(|i| [
                json!({"kind":"load","args":[format!("{:x}",0x8000 + i),1],"result":format!("{:x}",i+1)}),
                json!({"kind":"access_failed","args":[],"result": if reason == DATA && i == 5 { DATA } else { 0 }}),
            ])
            .collect::<Vec<_>>());
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn natural_loop_preserves_forward_branches_external_exits_and_store_order() {
    use crate::embedding::layout::DATA;
    let operations = [
        Operation::Jump { target: 0x1008 },
        Operation::Nop,
        Operation::Jcc {
            condition: Condition::Below,
            target: 0x1014,
        },
        Operation::Alu {
            kind: Alu::Add,
            dst: RegisterView::low(0),
            src: DataOperand::Immediate(7),
            width: Width::W32,
        },
        Operation::Jcc {
            condition: Condition::NotEqual,
            target: 0x1014,
        },
        Operation::Store {
            src: DataOperand::Immediate(0xab),
            width: Width::W8,
            address: Address {
                base: AddressBase::Register(3),
                ..Address::default()
            },
        },
        Operation::Alu {
            kind: Alu::Sub,
            dst: RegisterView::low(1),
            src: DataOperand::Immediate(1),
            width: Width::W32,
        },
        Operation::Jcc {
            condition: Condition::Equal,
            target: 0x1004,
        },
        Operation::Jump { target: 0x1008 },
        Operation::Nop,
    ];
    let instructions = operations
        .into_iter()
        .enumerate()
        .map(|(i, operation)| Instruction {
            pc: 0x1000 + i as u64 * 4,
            next_pc: 0x1004 + i as u64 * 4,
            operation,
        })
        .collect();
    let unit = unit(instructions);
    let bytes = emit(&unit);
    assert_eq!(
        wasm_loop_count(&bytes),
        1 + usize::from(!cfg!(feature = "parallel"))
    );
    let limit =
        executable_artifact_size(&bytes) as usize - usize::from(!cfg!(feature = "parallel"));
    let fallback = emit_profile(&unit, false, false, limit);
    assert!(fallback.len() <= limit);
    assert_eq!(wasm_loop_count(&fallback), 1);
    wasmparser::Validator::new()
        .validate_all(&fallback)
        .unwrap();
    let entries: Vec<_> = unit.instructions.iter().map(|i| i.pc).collect();
    let mut cases = Vec::new();
    for (entry, budget, flags, counter, expected, rip, reason, remaining, after, stop, store) in [
        (
            0x1008,
            7,
            2,
            2,
            vec![(0, 7), (1, 1)],
            0x1008,
            BUDGET,
            0,
            2,
            None,
            Some(1),
        ),
        (
            0x1008,
            7,
            3,
            2,
            vec![(0, 7), (1, 1)],
            0x1010,
            BUDGET,
            0,
            2,
            None,
            Some(1),
        ),
        (
            0x1008,
            7,
            3,
            1,
            vec![(1, 0)],
            0x1004,
            STOP,
            3,
            0x46,
            Some(0x1004),
            Some(1),
        ),
        (
            0x1008,
            7,
            2,
            2,
            vec![(0, 7)],
            0x1018,
            DISPATCH,
            3,
            2,
            None,
            Some(2),
        ),
        (
            0x1008,
            7,
            2,
            2,
            vec![(0, 7)],
            0x1014,
            DATA,
            4,
            2,
            None,
            Some(0),
        ),
        (
            0x1008,
            7,
            2,
            2,
            vec![(0, 7)],
            0x1014,
            STOP,
            4,
            2,
            Some(0x1014),
            None,
        ),
        (0x1010, 1, 2, 2, vec![], 0x1014, BUDGET, 0, 2, None, None),
    ] {
        let mut packet = execution_case(
            &bytes,
            &entries,
            &[(0, 0), (1, counter), (3, 0x8000)],
            flags,
            entry,
            budget,
            (&expected, after, rip, reason as u32, remaining),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        if let Some(status) = store {
            packet["accesses"] = json!([
                {"kind":"store","args":["8000",1,"ab"],"result":status}
            ]);
        }
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn rotated_loop_preserves_both_backedges_and_original_entry_boundaries() {
    use crate::embedding::layout::DATA;
    use crate::x86::instruction::{Count, Location, Shift};

    let decrement = Operation::Alu {
        kind: Alu::Sub,
        dst: RegisterView::low(1),
        src: DataOperand::Immediate(1),
        width: Width::W32,
    };
    let operations = [
        Operation::Jump { target: 0x1010 },
        Operation::Nop,
        decrement,
        Operation::Jcc {
            condition: Condition::Equal,
            target: 0x1004,
        },
        Operation::Load {
            dst: RegisterView::low(0),
            address: Address {
                base: AddressBase::Register(3),
                ..Address::default()
            },
            width: Width::W32,
        },
        Operation::Shift {
            kind: Shift::Shr,
            dst: Location::Register(RegisterView::low(0)),
            count: Count::Immediate(1),
            width: Width::W32,
        },
        Operation::Jcc {
            condition: Condition::Equal,
            target: 0x1008,
        },
        decrement,
        Operation::Jcc {
            condition: Condition::NotEqual,
            target: 0x1010,
        },
        Operation::Nop,
    ];
    let instructions = operations
        .into_iter()
        .enumerate()
        .map(|(i, operation)| Instruction {
            pc: 0x1000 + i as u64 * 4,
            next_pc: 0x1004 + i as u64 * 4,
            operation,
        })
        .collect();
    let unit = unit(instructions);
    let bytes = emit(&unit);
    // Standalone rotates the load loop; workers specialize the register backedge.
    assert_eq!(wasm_loop_count(&bytes), 2);
    let entries: Vec<_> = unit.instructions.iter().map(|i| i.pc).collect();
    let mut cases = Vec::new();
    for (entry, budget, counter, load, eax, ecx, flags, rip, reason, remaining, stop) in [
        (0x1010, 5, 2, Some("1"), 0, 1, 2, 0x1010, BUDGET, 0, None),
        (0x1010, 5, 2, Some("2"), 1, 1, 2, 0x1010, BUDGET, 0, None),
        (
            0x1010,
            6,
            1,
            Some("1"),
            0,
            0,
            0x46,
            0x1004,
            STOP,
            1,
            Some(0x1004),
        ),
        (
            0x1010,
            6,
            1,
            Some("2"),
            1,
            0,
            0x46,
            0x1024,
            STOP,
            1,
            Some(0x1024),
        ),
        (0x1010, 3, 2, Some("1"), 0, 2, 0x47, 0x1008, BUDGET, 0, None),
        (
            0x1010,
            5,
            2,
            Some("1"),
            0,
            2,
            0x47,
            0x1008,
            STOP,
            2,
            Some(0x1008),
        ),
        (0x1008, 2, 2, None, 7, 1, 2, 0x1010, BUDGET, 0, None),
        (0x1010, 5, 2, Some("0"), 7, 2, 2, 0x1010, DATA, 5, None),
    ] {
        let mut packet = execution_case(
            &bytes,
            &entries,
            &[(0, 7), (1, counter), (3, 0x8000)],
            2,
            entry,
            budget,
            (&[(0, eax), (1, ecx)], flags, rip, reason as u32, remaining),
        );
        if let Some(stop) = stop {
            packet["stop"] = json!(format!("{stop:x}"));
        }
        if let Some(load) = load {
            packet["accesses"] = json!([
                {"kind":"load","args":["8000",4],"result":load},
                {"kind":"access_failed","args":[],"result":if reason == DATA { DATA } else { 0 }}
            ]);
        }
        cases.push(packet);
    }
    run_cases(cases);
}

#[test]
fn memory_compare_predicate_keeps_flags_at_faults_and_checked_entries() {
    use super::super::loop_cfg::{CompareAction, ConditionSource, compare_predicates, select};
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
            compare_predicates(&instructions, order, &context)[1],
            CompareAction::Condition(ConditionSource::Operands(width))
        );
        let mut faulting = instructions.clone();
        faulting[4].operation = Operation::Load {
            dst: RegisterView::low(1),
            address: Address::default(),
            width: Width::W32,
        };
        assert_eq!(
            compare_predicates(&faulting, order, &context)[1],
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
            compare_predicates(&joined, joined_order, &context)[2],
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
    use super::super::loop_cfg::{
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
    let plan = compare_predicates(&instructions, order, &context);
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
        compare_predicates(&joined, joined_order, &context)[2],
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
    use super::super::loop_cfg::{CompareAction, ConditionSource, compare_predicates, select};
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
        compare_predicates(instructions, order, &context)
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
    use super::super::loop_cfg::{
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
        compare_predicates(instructions, order, &context)
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
    use super::super::loop_cfg::{CompareAction, ConditionSource, compare_predicates, select};

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
        compare_predicates(&artifact.instructions, order, &context)[2..4],
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
        compare_predicates(&joined, order, &context)[3],
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
    use super::super::loop_cfg::{
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
    let predicates =
        |instructions: &[Instruction]| compare_predicates(instructions, order, &context);
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
    use super::super::loop_cfg::{CompareAction, ConditionSource, compare_predicates, select};
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
        compare_predicates(&unit.instructions, order, &unit.context)
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
