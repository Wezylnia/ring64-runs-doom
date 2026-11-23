mod flags;
mod loops;

use crate::dbt::wasm::tests::*;
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
