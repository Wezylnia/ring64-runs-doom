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

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_paged_register_loop_uses_the_full_linear_cs_span() {
    use crate::x86::context::{Mode, Segment};

    let code = [0x83, 0xc0, 1, 0x83, 0xe9, 1, 0x75, 0xf8];
    let same_offset_page = compiled_fixture(0x1000, &code, 0x1000).instructions;
    let split_offset_page = compiled_fixture(0x1ffc, &code, 0x1ffc).instructions;
    for (mode, code32, base, limit, split_offset, usable, guarded) in [
        (Mode::Protected, true, 0x123, 0x1007, false, true, true),
        (Mode::Protected, true, 4, u32::MAX, true, true, true),
        (
            Mode::Compatibility,
            true,
            0x123,
            u32::MAX,
            false,
            true,
            true,
        ),
        (Mode::Protected, true, 0xffc, u32::MAX, false, true, false),
        (Mode::Protected, true, 0x123, 0x1006, false, true, false),
        (
            Mode::Protected,
            true,
            0xffff_f000,
            u32::MAX,
            false,
            true,
            false,
        ),
        (
            Mode::Protected,
            true,
            0xffff_effc,
            u32::MAX,
            false,
            true,
            false,
        ),
        (Mode::Protected, true, 0, u32::MAX, false, false, false),
        (Mode::Protected, false, 0, u32::MAX, false, true, false),
        (Mode::Compatibility, false, 0, u32::MAX, false, true, false),
        (Mode::Real, false, 0, 0xffff, false, true, false),
        (Mode::Virtual8086, false, 0, 0xffff, false, true, false),
    ] {
        let mut unit = unit(if split_offset {
            split_offset_page.clone()
        } else {
            same_offset_page.clone()
        });
        unit.context.set_mode_and_cpl(mode, 0);
        let segment = Segment::Cs;
        let mut cs = unit.context.segment(segment);
        (cs.base, cs.limit, cs.default32, cs.usable) = (base, limit, code32, usable);
        let mut attributes = unit.context.attributes(segment);
        attributes.long = false;
        unit.context.set_segment(segment, 8, cs, attributes);

        let bytes = emit_profile(&unit, true, true, usize::MAX);
        assert_eq!(
            wasm_loop_count(&bytes),
            if guarded { 3 } else { 2 },
            "{mode:?} code32={code32} CS base={base:#x} limit={limit:#x}"
        );
        wasmparser::Validator::new().validate_all(&bytes).unwrap();
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn paged_dword_read_loop_keeps_original_dispatch_under_a_tight_artifact_limit() {
    let code = [
        0x8b, 0x07, 0x8b, 0x4f, 4, 0x8b, 0x57, 8, 0x8b, 0x1e, 0x83, 0xed, 1, 0x75, 0xf1,
    ];
    let same_page = unit(compiled_fixture(0x1000, &code, 0x1000).instructions);
    let split_page = unit(compiled_fixture(0x1ff8, &code, 0x1ff8).instructions);
    let guarded = emit_profile(&same_page, true, true, usize::MAX);
    let limited = emit_profile(&same_page, true, true, guarded.len() - 1);
    let split_page = emit_profile(&split_page, true, true, usize::MAX);
    for (bytes, loops) in [(guarded, 3), (limited, 1), (split_page, 1)] {
        assert_eq!(wasm_loop_count(&bytes), loops);
        wasmparser::Validator::new().validate_all(&bytes).unwrap();
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn paged_rotated_compare_requires_its_whole_code_span_and_a_ds_operand() {
    use crate::x86::{context::Segment, instruction::Operation};

    // The rotated prefix is visited by JAE after CMP [RCX+R14*4],ESI.
    let code = [
        0x44, 0x89, 0xf7, 0x41, 0x39, 0xfb, 0x73, 0xc8, 0x89, 0xfb, 0x44, 0x29, 0xdb, 0xd1, 0xeb,
        0x46, 0x8d, 0x34, 0x1b, 0x42, 0x39, 0x34, 0xb1, 0x73, 0xe7, 0x41, 0x01, 0xdb, 0x41, 0xff,
        0xc3, 0x41, 0x39, 0xfb, 0x72, 0xe4,
    ];
    let same_page = unit(compiled_fixture(0x1000, &code, 0x1008).instructions);
    let cross_page = unit(compiled_fixture(0x1ffc, &code, 0x2004).instructions);
    let mut fs = unit(same_page.instructions.clone());
    let Some(compare) = fs
        .instructions
        .iter_mut()
        .find_map(|i| match &mut i.operation {
            Operation::MemoryAlu { address, .. } => Some(address),
            _ => None,
        })
    else {
        panic!("fixture must decode a memory compare");
    };
    compare.segment = Segment::Fs;

    let guarded = emit_profile(&same_page, true, true, usize::MAX);
    let capped = emit_profile(&same_page, true, true, guarded.len() - 1);
    let cross_page = emit_profile(&cross_page, true, true, usize::MAX);
    let fs = emit_profile(&fs, true, true, usize::MAX);
    for (bytes, loops) in [(guarded, 3), (capped, 1), (cross_page, 1), (fs, 1)] {
        assert_eq!(wasm_loop_count(&bytes), loops);
        wasmparser::Validator::new().validate_all(&bytes).unwrap();
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn paged_32_bit_rotated_compare_checks_its_linear_code_span_and_segment() {
    use crate::memory::Image;
    use crate::x86::context::{ExecutionContext, Mode, Segment};

    let code = [
        0x89, 0xd3, 0x39, 0xdd, 0x73, 0xba, 0x89, 0xde, 0x29, 0xee, 0xd1, 0xee, 0x8d, 0x14, 0x2e,
        0x39, 0x3c, 0x91, 0x73, 0xec, 0x01, 0xf5, 0x45, 0x39, 0xdd, 0x72, 0xeb,
    ];
    let mut context = ExecutionContext::default();
    context.set_mode_and_cpl(Mode::Protected, 0);
    let mut cs = context.segment(Segment::Cs);
    cs.default32 = true;
    cs.base = 0x30;
    let mut cs_attributes = context.attributes(Segment::Cs);
    cs_attributes.long = false;
    context.set_segment(Segment::Cs, 8, cs, cs_attributes);
    let fixture = |base: u64| {
        let image = Image::new(base + 0x30, code.to_vec(), 0x8000, vec![0; 8]).unwrap();
        let artifact = crate::dbt::compile_with_context(
            &image,
            &context,
            base + 6,
            crate::dbt::Limits {
                max_instructions: 64,
                max_source_bytes: 256,
                max_artifact_bytes: 65536,
            },
        )
        .unwrap();
        Unit {
            context: context.clone(),
            ..unit(artifact.instructions)
        }
    };
    let same_page = fixture(0x1000);
    // The guest offsets fit one page. Their CS-based linear addresses do not.
    let split_page = fixture(0x1fcc);
    let mut fs = fixture(0x1000);
    let compare = fs
        .instructions
        .iter_mut()
        .find_map(|instruction| match &mut instruction.operation {
            Operation::MemoryAlu { address, .. } => Some(address),
            _ => None,
        })
        .unwrap();
    compare.segment = Segment::Fs;
    let mut limit = fixture(0x1000);
    let mut cs = limit.context.segment(Segment::Cs);
    cs.limit = 0x101f;
    limit.context.set_segment(Segment::Cs, 8, cs, cs_attributes);
    let outside = limit
        .instructions
        .iter_mut()
        .find_map(|instruction| match &mut instruction.operation {
            Operation::Jcc { target, .. } if *target < 0x1000 => Some(target),
            _ => None,
        })
        .unwrap();
    *outside = 0x1020;
    let full = emit_profile(&same_page, true, true, usize::MAX);
    let capped = emit_profile(&same_page, true, true, full.len() - 1);
    let split = emit_profile(&split_page, true, true, usize::MAX);
    let fs = emit_profile(&fs, true, true, usize::MAX);
    let limit = emit_profile(&limit, true, true, usize::MAX);
    for (bytes, loops) in [(full, 3), (capped, 1), (split, 1), (fs, 1), (limit, 1)] {
        assert_eq!(wasm_loop_count(&bytes), loops);
        wasmparser::Validator::new().validate_all(&bytes).unwrap();
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn paged_32_bit_reads_require_flat_ds_and_dword_addresses() {
    use crate::memory::Image;
    use crate::x86::context::{ExecutionContext, Mode, Segment, SegmentType};

    let mut context = ExecutionContext::default();
    context.set_mode_and_cpl(Mode::Protected, 0);
    let mut cs = context.segment(Segment::Cs);
    cs.default32 = true;
    let mut cs_attributes = context.attributes(Segment::Cs);
    cs_attributes.long = false;
    context.set_segment(Segment::Cs, 8, cs, cs_attributes);
    for (mode, base, limit, usable, kind, addr32, guarded) in [
        (
            Mode::Protected,
            0,
            u32::MAX,
            true,
            SegmentType::ReadWrite,
            true,
            true,
        ),
        (
            Mode::Compatibility,
            0,
            u32::MAX,
            true,
            SegmentType::ReadWrite,
            true,
            true,
        ),
        (
            Mode::Protected,
            1,
            u32::MAX,
            true,
            SegmentType::ReadWrite,
            true,
            false,
        ),
        (
            Mode::Protected,
            0,
            0xffff,
            true,
            SegmentType::ReadWrite,
            true,
            false,
        ),
        (
            Mode::Protected,
            0,
            u32::MAX,
            false,
            SegmentType::ReadWrite,
            true,
            false,
        ),
        (
            Mode::Protected,
            0,
            u32::MAX,
            true,
            SegmentType::ExpandDownReadOnly,
            true,
            false,
        ),
        (
            Mode::Protected,
            0,
            u32::MAX,
            true,
            SegmentType::ReadWrite,
            false,
            false,
        ),
    ] {
        context.set_mode_and_cpl(mode, 0);
        let mut ds = context.segment(Segment::Ds);
        (ds.base, ds.limit, ds.usable, ds.kind) = (base, limit, usable, kind);
        context.set_segment(Segment::Ds, 24, ds, context.attributes(Segment::Ds));
        let mut code = Vec::new();
        for _ in 0..4 {
            if !addr32 {
                code.push(0x67);
            }
            code.extend_from_slice(&[0x8b, 0x07]); // mov eax, [edi] / [bx]
        }
        code.extend_from_slice(&[0x83, 0xe9, 1]);
        code.extend_from_slice(&[0x75, 0u8.wrapping_sub(code.len() as u8 + 2)]);
        let image = Image::new(0x1000, code, 0x8000, vec![0; 8]).unwrap();
        let artifact = crate::dbt::compile_with_context(
            &image,
            &context,
            0x1000,
            crate::dbt::Limits {
                max_instructions: 64,
                max_source_bytes: 256,
                max_artifact_bytes: 65536,
            },
        )
        .unwrap();
        let unit = Unit {
            context: context.clone(),
            ..unit(artifact.instructions)
        };
        let bytes = emit_profile(&unit, true, true, usize::MAX);
        assert_eq!(
            wasm_loop_count(&bytes),
            if guarded { 3 } else { 1 },
            "{mode:?}: DS={ds:?} addr32={addr32}"
        );
        wasmparser::Validator::new().validate_all(&bytes).unwrap();
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn paged_natural_write_graph_imports_only_the_admitted_ds_byte_store() {
    use crate::x86::{context::Segment, instruction::Operation};

    let code = [
        0x0f, 0xb6, 0x07, // movzx eax,[rdi]
        0x39, 0xf0, // cmp eax,esi
        0x72, 0x06, // jb store
        0x0f, 0xb6, 0xc0, // movzx eax,al
        0x0f, 0x4d, 0xc6, // cmovge eax,esi
        0x88, 0x07, // store: mov [rdi],al
        0x48, 0xff, 0xc7, // inc rdi
        0xff, 0xc9, // dec ecx
        0x75, 0xea, // jne head
    ];
    let ds = unit(compiled_fixture(0x1000, &code, 0x1000).instructions);
    let split_page = unit(compiled_fixture(0x1ffc, &code, 0x1ffc).instructions);
    let mut fs = unit(ds.instructions.clone());
    for i in &mut fs.instructions {
        if let Operation::Store { address, .. } = &mut i.operation {
            address.segment = Segment::Fs;
        }
    }
    let guarded = emit_profile(&ds, true, true, usize::MAX);
    // Canonical checked entries serve both direct entry and guarded replay.
    assert_eq!(wasm_loop_count(&guarded), 2);
    let capped = emit_profile(&ds, true, true, guarded.len() - 1);
    let unpaged = emit_profile(&ds, true, false, usize::MAX);
    let split_page = emit_profile(&split_page, true, true, usize::MAX);
    let fs = emit_profile(&fs, true, true, usize::MAX);
    for (bytes, guarded) in [
        (guarded, true),
        (capped, false),
        (unpaged, false),
        (split_page, false),
        (fs, false),
    ] {
        wasmparser::Validator::new().validate_all(&bytes).unwrap();
        let writes = wasmparser::Parser::new(0)
            .parse_all(&bytes)
            .filter_map(|payload| match payload.unwrap() {
                wasmparser::Payload::ImportSection(reader) => Some(reader),
                _ => None,
            })
            .flat_map(|reader| reader.into_imports())
            .filter(|import| import.as_ref().unwrap().name == "paged_loop_write_byte")
            .count();
        assert_eq!(writes, usize::from(guarded));
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn paged_scalar_f32_loop_uses_a_distinct_guard_and_has_checked_fallback() {
    let code = [
        0x8b, 0x17, // mov edx, [rdi]
        0xf3, 0x0f, 0x10, 0x07, // movss xmm0, [rdi]
        0xf3, 0x0f, 0x59, 0x06, // mulss xmm0, [rsi]
        0xf3, 0x0f, 0x58, 0xc1, // addss xmm0, xmm1
        0x83, 0xc0, 1, 0x83, 0xe9, 1, 0x75, 0xec,
    ];
    let local = unit(compiled_fixture(0x1000, &code, 0x1000).instructions);
    let split = unit(compiled_fixture(0x1ff8, &code, 0x1ff8).instructions);
    let guarded = emit_profile(&local, true, true, usize::MAX);
    let limited = emit_profile(&local, true, true, guarded.len() - 1);
    let crossing = emit_profile(&split, true, true, usize::MAX);
    for (bytes, fast) in [(guarded, true), (limited, false), (crossing, false)] {
        wasmparser::Validator::new().validate_all(&bytes).unwrap();
        let imports: Vec<_> = wasmparser::Parser::new(0)
            .parse_all(&bytes)
            .find_map(|payload| match payload.unwrap() {
                wasmparser::Payload::ImportSection(reader) => Some(reader),
                _ => None,
            })
            .unwrap()
            .into_imports()
            .map(|item| item.unwrap().name)
            .collect();
        assert!(imports.contains(&"debug_begin"));
        assert!(imports.contains(&"vector_load"));
        assert_eq!(imports.contains(&"paged_sse_loop_guard"), fast);
        assert_eq!(imports.contains(&"paged_loop_read_dword"), fast);
        assert!(!imports.contains(&"paged_loop_guard"));
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn paged_stack_spills_require_flat_segments_and_32_bit_effective_addresses() {
    use crate::x86::{
        context::{Mode, Segment, SegmentType},
        instruction::{Operation, Width},
    };

    let code = [
        0x8b, 0x44, 0x24, 4, // mov eax,[esp+4]
        0x3b, 0x44, 0x24, 8, // cmp eax,[esp+8]
        0x76, 1,    // jbe spill
        0x40, // inc eax
        0x89, 0x44, 0x24, 4, // spill: mov [esp+4],eax
        0x0f, 0xb6, 0x17, // movzx edx,[edi]
        0x01, 0xc2, // add edx,eax
        0x88, 0x16, // mov [esi],dl
        0x46, 0x47, 0x49, // inc esi; inc edi; dec ecx
        0x75, 0xe5, // jne head
    ];
    let mut context = ExecutionContext::default();
    context.set_mode_and_cpl(Mode::Protected, 0);
    let mut cs = context.segment(Segment::Cs);
    cs.default32 = true;
    let mut cs_attributes = context.attributes(Segment::Cs);
    cs_attributes.long = false;
    context.set_segment(Segment::Cs, 8, cs, cs_attributes);
    let image = crate::memory::Image::new(0x1000, code.to_vec(), 0x8000, vec![0; 8]).unwrap();
    let artifact = crate::dbt::compile_with_context(
        &image,
        &context,
        0x1000,
        crate::dbt::Limits {
            max_instructions: 64,
            max_source_bytes: 256,
            max_artifact_bytes: 65536,
        },
    )
    .unwrap();
    let mut body = Unit {
        context,
        ..unit(artifact.instructions)
    };
    let guarded = emit_profile(&body, true, true, usize::MAX);
    let capped = emit_profile(&body, true, true, guarded.len() - 1);
    let unpaged = emit_profile(&body, true, false, usize::MAX);
    let old_ss = body.context.segment(Segment::Ss);
    let ss_attributes = body.context.attributes(Segment::Ss);
    let mut cases = vec![(guarded, true), (capped, false), (unpaged, false)];
    for ss in [
        crate::x86::context::LoadedSegment {
            base: 0x1000,
            ..old_ss
        },
        crate::x86::context::LoadedSegment {
            limit: 0xffff,
            ..old_ss
        },
        crate::x86::context::LoadedSegment {
            kind: SegmentType::ExpandDownReadWrite,
            ..old_ss
        },
    ] {
        body.context
            .set_segment(Segment::Ss, 0x10, ss, ss_attributes);
        cases.push((emit_profile(&body, true, true, usize::MAX), false));
    }
    body.context
        .set_segment(Segment::Ss, 0x10, old_ss, ss_attributes);
    let old_ds = body.context.segment(Segment::Ds);
    let ds_attributes = body.context.attributes(Segment::Ds);
    let mut ro_ds = old_ds;
    ro_ds.kind = SegmentType::ReadOnly;
    body.context
        .set_segment(Segment::Ds, 0x18, ro_ds, ds_attributes);
    cases.push((emit_profile(&body, true, true, usize::MAX), false));
    body.context
        .set_segment(Segment::Ds, 0x18, old_ds, ds_attributes);
    if let Operation::Load { address, .. } = &mut body.instructions[0].operation {
        address.size = Width::W16;
    } else {
        unreachable!("the first instruction loads the stack slot");
    }
    cases.push((emit_profile(&body, true, true, usize::MAX), false));
    for (bytes, accepted) in cases {
        wasmparser::Validator::new().validate_all(&bytes).unwrap();
        let imports: Vec<_> = wasmparser::Parser::new(0)
            .parse_all(&bytes)
            .filter_map(|payload| match payload.unwrap() {
                wasmparser::Payload::ImportSection(reader) => Some(reader),
                _ => None,
            })
            .flat_map(|reader| reader.into_imports().map(|import| import.unwrap().name))
            .collect();
        for name in [
            "paged_write_loop_guard32",
            "paged_loop_read_dword",
            "paged_loop_read_byte",
            "paged_loop_write_byte",
            "paged_loop_write_dword",
        ] {
            assert_eq!(imports.contains(&name), accepted, "{name}");
        }
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
