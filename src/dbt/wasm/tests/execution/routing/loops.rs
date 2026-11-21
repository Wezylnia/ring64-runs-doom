use super::*;

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

#[cfg(not(feature = "parallel"))]
#[test]
fn paged_register_loop_retains_dispatch_under_a_tight_artifact_limit() {
    let code = [0x83, 0xc0, 1, 0x83, 0xe9, 1, 0x75, 0xf8];
    let same_page = unit(compiled_fixture(0x1000, &code, 0x1000).instructions);
    let cross_page = unit(compiled_fixture(0x1ffc, &code, 0x1ffc).instructions);
    let guarded = emit_profile(&same_page, true, true, usize::MAX);
    let checked = emit_profile(&same_page, true, true, guarded.len() - 1);
    let dispatch = emit_profile(&same_page, true, true, checked.len() - 1);
    let cross_page = emit_profile(&cross_page, true, true, usize::MAX);
    assert!(checked.len() < guarded.len());
    assert!(dispatch.len() < checked.len());
    for (bytes, loops) in [(guarded, 3), (checked, 2), (dispatch, 1), (cross_page, 2)] {
        assert_eq!(wasm_loop_count(&bytes), loops);
        wasmparser::Validator::new().validate_all(&bytes).unwrap();
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
