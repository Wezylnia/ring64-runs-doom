use super::*;

#[test]
fn sse3_raw_moves_copy_literal_lanes_and_request_exact_memory_operands() {
    let raw = 0x7f812345_80000000_ff8123ab_12345678u128;
    let double = 0xff8123ab_12345678_ff8123ab_12345678u128;
    let even = 0x80000000_80000000_12345678_12345678u128;
    let odd = 0x7f812345_7f812345_ff8123ab_ff8123abu128;
    let mut cases = vec![];
    for (prefix, opcode, width, alignment, result) in [
        (0xf2, 0x12, 8, 1, double),
        (0xf3, 0x12, 16, 16, even),
        (0xf3, 0x16, 16, 16, odd),
        (0xf2, 0xf0, 16, 1, raw),
    ] {
        // XMM15 <- XMM8, XMM15 <- XMM15, and XMM15 <- [R8].
        for modrm in [0xf8, 0xff, 0x38] {
            let memory = modrm == 0x38;
            if opcode == 0xf0 && !memory {
                continue;
            }
            let code = [prefix, 0x45, 0x0f, opcode, modrm];
            let artifact = compiled_fixture(0x1000, &code, 0x1000);
            let source = if modrm == 0xff { 15 } else { 8 };
            for failed in [false, true] {
                if failed && !memory {
                    continue;
                }
                let changes = [(15, result)];
                let mut p = with_xmm(
                    execution_case(
                        &artifact.bytes,
                        &artifact.entries,
                        &[(8, 0x6040)],
                        0xcd7,
                        0x1000,
                        1,
                        if failed {
                            (&[], 0xcd7, 0x1000, DATA as u32, 1)
                        } else {
                            (&[], 0xcd7, 0x1005, BUDGET as u32, 0)
                        },
                    ),
                    &[(source, raw)],
                    if failed { &[] } else { &changes },
                );
                p["mxcsr"] = json!(0x0020);
                p["expected"]["mxcsr"] = p["mxcsr"].clone();
                if memory {
                    let mut accesses = vec![json!({
                        "kind":"vector_load", "args":["6040",width,Segment::Ds as u32,alignment],
                        "result":if failed { Value::Null } else { json!(format!("{raw:x}")) }
                    })];
                    if failed {
                        accesses.push(json!({"kind":"access_reason","args":[],"result":DATA}));
                    }
                    p["accesses"] = json!(accesses);
                }
                cases.push(p);
            }
        }
    }
    run_cases(cases);
}

#[test]
fn sse3_raw_duplicate_chain_consumes_the_live_register_at_each_entry() {
    let code = [
        0x66, 0x45, 0x0f, 0x6f, 0xc1, // MOVDQA XMM8, XMM9
        0xf3, 0x45, 0x0f, 0x16, 0xc0, // MOVSHDUP XMM8, XMM8
        0xf2, 0x45, 0x0f, 0x12, 0xd0, // MOVDDUP XMM10, XMM8
    ];
    let artifact = compiled_fixture(0x1000, &code, 0x1000);
    let mut cases = vec![];
    for (entry, budget, xmm8, xmm10) in [
        (
            0x1000,
            3,
            0x88888888_88888888_66666666_66666666,
            0x66666666_66666666_66666666_66666666,
        ),
        (
            0x1005,
            2,
            0x44444444_44444444_22222222_22222222,
            0x22222222_22222222_22222222_22222222,
        ),
    ] {
        cases.push(with_xmm(
            execution_case(
                &artifact.bytes,
                &artifact.entries,
                &[],
                2,
                entry,
                budget,
                (&[], 2, 0x100f, BUDGET as u32, 0),
            ),
            &[
                (8, 0x44444444_33333333_22222222_11111111),
                (9, 0x88888888_77777777_66666666_55555555),
            ],
            &[(8, xmm8), (10, xmm10)],
        ));
    }
    run_cases(cases);
}
