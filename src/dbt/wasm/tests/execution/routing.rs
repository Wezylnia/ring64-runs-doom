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
    let specialized = !cfg!(feature = "parallel");
    assert_eq!(
        wasm_loop_count(&artifact.bytes),
        1 + usize::from(specialized)
    );
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
        max_artifact_bytes: artifact.bytes.len() as u32 - u32::from(specialized),
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
            max_artifact_bytes: artifact.bytes.len() as u32 - u32::from(expected_loops == 2),
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
