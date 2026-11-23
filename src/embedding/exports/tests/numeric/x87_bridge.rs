use super::*;
use crate::embedding::layout::X87_EXCEPTION;
use crate::memory::{MappingInput, MemoryLimits, Permissions};
use crate::x86::x87::{Format, Kind, Op, State as X87State};

fn operation(kind: Kind, format: Format, encoding: u16) -> Op {
    Op {
        kind,
        format,
        encoding,
        ..Op::default()
    }
}
fn invoke(operation: Op, offset: u64, pc: u64, flags: u64, rax: u64) -> i32 {
    x87::core_x87(
        operation.encode(),
        offset,
        Segment::Fs as u32,
        pc,
        flags,
        rax,
    )
}
fn install(state: X87State) {
    BRIDGE.with_borrow_mut(|bridge| unsafe {
        (*bridge.core.as_mut().unwrap().state.get()).x87 = state
    });
}
fn snapshot() -> X87State {
    BRIDGE.with_borrow(|bridge| unsafe { (*bridge.core.as_ref().unwrap().state.get()).x87 })
}

#[cfg(not(feature = "parallel"))]
#[test]
fn x87_register_store_pop_matches_the_full_evaluator() {
    initialize(0x1000, &[0xf4]);
    for top in 0..8 {
        for index in 0..8 {
            let op = Op {
                index,
                pop: 1,
                ..operation(Kind::Store, Format::Register, 0x5d8 + u16::from(index))
            };
            for (raw, tag, control_word) in [
                (0x4000_c000_0000_0000_0000, 2, 0x37f),
                (0x8000_0000_0000_0000_0000, 0, 0x37f),
                (0x7fff_8000_0000_0000_0001, 0, 0x37f),
                (0x4000_c000_0000_0000_0000, 3, 0x37f),
                (0x4000_c000_0000_0000_0000, 3, 0x37e),
            ] {
                let mut prior = X87State {
                    control_word,
                    status_word: (top << 11) | 0x4700 | 0x20,
                    tag_word: 0,
                    opcode: 0x123,
                    instruction_pointer: 0xdead,
                    data_pointer: 0x4000,
                    ..X87State::default()
                };
                prior.set_raw(usize::from(top), raw);
                prior.set_tag(usize::from(top), tag);
                let mut expected = prior;
                crate::x86::x87::eval(&mut expected, op, 0, 0x202);
                expected.instruction_pointer = 0x1000;
                if expected.pending() != 0 {
                    expected.opcode = op.encoding;
                }
                install(prior);
                assert_eq!(invoke(op, 0, 0x1000, 0x202, 0), 0);
                assert_eq!(snapshot(), expected);
            }
        }
    }

    let pending = X87State {
        control_word: 0x37e,
        status_word: 0x8081,
        ..X87State::default()
    };
    install(pending);
    let op = Op {
        pop: 1,
        ..operation(Kind::Store, Format::Register, 0x5d8)
    };
    assert_eq!(invoke(op, 0, 0x1000, 0x202, 0), X87_EXCEPTION);
    assert_eq!(snapshot(), pending);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn x87_register_pairs_keep_numeric_and_environment_effects_or_refuse_before_either() {
    use crate::x86::x87::REGISTER_PAIR_SEGMENT;

    initialize(0x1000, &[0xf4]);
    let load = operation(Kind::Load, Format::Register, 0x1c0);
    let multiply = Op {
        index: 1,
        ..operation(Kind::Mul, Format::Register, 0x0c9)
    };
    let pair = || {
        x87::core_x87(
            load.encode(),
            u64::from(multiply.encode()),
            REGISTER_PAIR_SEGMENT,
            0x1000,
            0,
            0x1002,
        )
    };
    let mut ordinary = X87State {
        tag_word: 0xfffc,
        opcode: 0x123,
        data_pointer: 0x4000,
        ..X87State::default()
    };
    ordinary.set_raw(0, 0x3fff_c000_0000_0000_0000);
    let mut overflow = X87State {
        control_word: 0x377,
        ..ordinary
    };
    overflow.set_raw(0, 0x7ffe_ffff_ffff_ffff_ffff);
    for prior in [ordinary, overflow] {
        install(prior);
        assert_eq!(invoke(load, 0, 0x1000, 0x8d7, 0), 0);
        assert_eq!(invoke(multiply, 0, 0x1002, 0x8d7, 0), 0);
        let expected = snapshot();
        assert_eq!(expected.instruction_pointer, 0x1002);
        assert_eq!(expected.data_pointer, 0x4000);
        if prior.control_word == 0x377 {
            assert_ne!(expected.pending(), 0);
            assert_eq!(expected.opcode, 0x0c9);
        }
        install(prior);
        assert_eq!(pair(), 0);
        assert_eq!(snapshot(), expected);
    }

    let mut occupied = ordinary;
    occupied.set_tag(7, 2);
    let mut empty = ordinary;
    empty.set_tag(0, 3);
    let pending = X87State {
        control_word: 0x37e,
        status_word: 0x8081,
        ..ordinary
    };
    assert_eq!(core_load(0x7000, 8), 0);
    let diagnostic = metadata(DATA);
    for prior in [occupied, empty, pending] {
        install(prior);
        assert_eq!(pair(), -1);
        assert_eq!(snapshot(), prior);
        assert_eq!(metadata(DATA), diagnostic);
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn x87_memory_pair_matches_separate_register_load_and_arithmetic() {
    use crate::x86::x87::MEMORY_PAIR_SEGMENT;

    initialize(0x1000, &[0xf4]);
    let load = operation(Kind::Load, Format::Register, 0x1c0);
    let pair = |second: Op, address| {
        x87::core_x87(
            load.encode(),
            address,
            MEMORY_PAIR_SEGMENT | Segment::Fs as u32,
            0,
            u64::from(second.encode()),
            0x1002,
        )
    };
    for (format, encoded, width, inputs) in [
        (Format::F32, 0x038, 4, [0x4040_0000, 1, 0x7f80_0010]),
        (
            Format::F64,
            0x438,
            8,
            [0x4008_0000_0000_0000u64, 1, 0x7ff0_0000_0000_0010],
        ),
    ] {
        let divide = operation(Kind::DivReverse, format, encoded);
        for input in inputs {
            assert_eq!(core_store(0x4001, width, input), 1);
            for top in 0u16..8 {
                for cw in [0x037f, 0x037e, 0x007d, 0x0e5f] {
                    let mut prior = X87State {
                        control_word: cw,
                        status_word: (top << 11) | 0x4700,
                        opcode: 0x123,
                        instruction_pointer: 0x7000,
                        data_pointer: 0x8000,
                        ..X87State::default()
                    };
                    prior.set_raw(usize::from(top), 0x4000_c000_0000_0000_0000);
                    prior.set_tag(usize::from(top), 0);
                    install(prior);
                    assert_eq!(invoke(load, 0, 0x1000, 0x202, 0), 0);
                    assert_eq!(invoke(divide, 0x4001, 0x1002, 0x202, 0), 0);
                    let expected = snapshot();
                    assert_eq!(expected.instruction_pointer, 0x1002);
                    assert_eq!(expected.data_pointer, 0x4001);
                    install(prior);
                    assert_eq!(core_load(0x7000, 8), 0);
                    let diagnostic = metadata(DATA);
                    assert_eq!(pair(divide, 0x400f), -1);
                    assert_eq!(snapshot(), prior);
                    assert_eq!(metadata(DATA), diagnostic);
                    assert_eq!(pair(divide, 0x4001), 0);
                    assert_eq!(snapshot(), expected);
                }
            }
        }
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn x87_memory_first_pairs_match_separate_operations_or_leave_the_prefix_intact() {
    use crate::x86::x87::MEMORY_FIRST_PAIR_SEGMENT;

    initialize(0x1000, &[0xf4]);
    let pair = |first: Op, second: Op, output| {
        x87::core_x87(
            first.encode(),
            0x4000,
            MEMORY_FIRST_PAIR_SEGMENT | Segment::Fs as u32 | ((Segment::Fs as u32) << 3),
            0x1003,
            output,
            u64::from(second.encode()),
        )
    };
    let values = [
        (Format::F32, 4, [0x4040_0000, 1, 0x7f80_0010]),
        (
            Format::F64,
            8,
            [0x4008_0000_0000_0000, 1, 0x7ff0_0000_0000_0010],
        ),
    ];
    let mut successes = 0;
    let mut refusals = 0;
    for (format, width, inputs) in values {
        for (first, second) in [
            (
                operation(Kind::Load, format, 0x500),
                operation(Kind::Load, Format::Register, 0x1c0),
            ),
            (
                operation(Kind::DivReverse, format, 0x438),
                Op {
                    pop: 1,
                    ..operation(Kind::Store, format, 0x518)
                },
            ),
        ] {
            for input in inputs {
                assert_eq!(core_store(0x4000, width, input), 1);
                for control_word in [0x37f, 0x37e, 0x37d, 0x077f, 0x0e5f] {
                    for top in 0u16..8 {
                        let mut prior = X87State {
                            control_word,
                            status_word: (top << 11) | 0x4700,
                            opcode: 0x123,
                            instruction_pointer: 0x7000,
                            data_pointer: 0x8000,
                            ..X87State::default()
                        };
                        prior.set_raw(usize::from(top), 0x4000_c000_0000_0000_0000);
                        prior.set_tag(usize::from(top), 0);
                        install(prior);
                        assert_eq!(core_store(0x4008, 8, u64::MAX), 1);
                        assert_eq!(invoke(first, 0x4000, 0x1000, 0, 0), 0);
                        let separate_status = invoke(second, 0x4008, 0x1003, 0, 0);
                        let separate = (snapshot(), core_load(0x4008, 8));

                        install(prior);
                        assert_eq!(core_store(0x4008, 8, u64::MAX), 1);
                        if second.is_store() {
                            assert_eq!(pair(first, second, 0x400f), -1);
                            assert_eq!(snapshot(), prior);
                            assert_eq!(core_load(0x4008, 8), u64::MAX);
                        }
                        match pair(first, second, 0x4008) {
                            0 => {
                                successes += 1;
                                assert_eq!(separate_status, 0);
                                assert_eq!((snapshot(), core_load(0x4008, 8)), separate);
                            }
                            -1 => {
                                refusals += 1;
                                assert_eq!(snapshot(), prior);
                                assert_eq!(core_load(0x4008, 8), u64::MAX);
                            }
                            status => panic!("unexpected status {status}"),
                        }
                    }
                }
            }
        }
    }
    assert!(successes > 0 && refusals > 0);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn x87_register_pop_pair_matches_two_checked_writes_or_refuses_intact() {
    use crate::x86::x87::REGISTER_PAIR_SEGMENT;

    initialize(0x1000, &[0xf4]);
    let pop = |index| Op {
        index,
        pop: 1,
        ..operation(Kind::Store, Format::Register, 0x5d8 + u16::from(index))
    };
    let flags = 0xcd7;
    let pair = |first: Op, second: Op| {
        x87::core_x87(
            first.encode(),
            u64::from(second.encode()),
            REGISTER_PAIR_SEGMENT,
            0x1000,
            0,
            0x1002,
        )
    };
    let projection = || {
        BRIDGE.with_borrow(|bridge| unsafe {
            let run = &*bridge.core.as_ref().unwrap().state.get();
            (run.x87, run.flags)
        })
    };
    BRIDGE.with_borrow_mut(|bridge| unsafe {
        (*bridge.core.as_mut().unwrap().state.get()).flags = flags;
    });
    for top in 0u16..8 {
        for first in 0..8 {
            for second in 0..8 {
                for (source, next, control_word, status) in [
                    (2, 0, 0x37f, 0x4720),
                    (2, 3, 0x37f, 0x4720),
                    (3, 0, 0x37f, 0x4720),
                    (2, 0, 0x37e, 0x8081),
                ] {
                    let mut prior = X87State {
                        control_word,
                        status_word: (top << 11) | status,
                        opcode: 0x123,
                        instruction_pointer: 0xdead,
                        data_pointer: 0x4000,
                        ..X87State::default()
                    };
                    let top = usize::from(top);
                    let next_top = (top + 1) & 7;
                    prior.set_raw(top, 0x7fff_8000_0000_0000_0001);
                    prior.set_tag(top, source);
                    prior.set_raw(next_top, 0x8000_0000_0000_0000_0000);
                    prior.set_tag(next_top, next);
                    let first_op = pop(first);
                    let second_op = pop(second);
                    if prior.pending() == 0 && source != 3 && (next != 3 || first == 1) {
                        install(prior);
                        for (op, pc) in [(first_op, 0x1000), (second_op, 0x1002)] {
                            assert_eq!(
                                x87::core_x87_observed(
                                    op.encode(),
                                    0,
                                    Segment::Cs as u32,
                                    pc,
                                    0,
                                    0
                                ),
                                0,
                            );
                        }
                        let expected = projection();
                        assert_eq!(expected.0.instruction_pointer, 0x1002);
                        install(prior);
                        assert_eq!(pair(first_op, second_op), 0);
                        assert_eq!(projection(), expected);
                    } else {
                        install(prior);
                        assert_eq!(core_load(0x7000, 8), 0);
                        let diagnostic = metadata(DATA);
                        assert_eq!(pair(first_op, second_op), -1);
                        assert_eq!(projection(), (prior, flags));
                        assert_eq!(metadata(DATA), diagnostic);
                    }
                }
            }
        }
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn x87_exchange_compare_pair_preserves_live_integer_flags_and_numeric_exceptions() {
    use crate::x86::x87::REGISTER_PAIR_SEGMENT;

    initialize(0x1000, &[0xf4]);
    let exchange = Op {
        index: 1,
        ..operation(Kind::Exchange, Format::Register, 0x1c9)
    };
    let compare = Op {
        index: 1,
        pop: 1,
        ..operation(Kind::CompareUnorderedFlags, Format::Register, 0x7e9)
    };
    let flags = 0xcd7;
    let projection = || {
        BRIDGE.with_borrow(|bridge| unsafe {
            let run = &*bridge.core.as_ref().unwrap().state.get();
            (run.x87, run.flags)
        })
    };
    let pair = || {
        x87::core_x87(
            exchange.encode(),
            u64::from(compare.encode()),
            REGISTER_PAIR_SEGMENT,
            0x1000,
            flags,
            0x1002,
        )
    };
    let mut ordinary = X87State {
        tag_word: 0xfff0,
        data_pointer: 0x5000,
        opcode: 0x123,
        ..X87State::default()
    };
    ordinary.set_raw(0, 0x3fff_8000_0000_0000_0000);
    ordinary.set_raw(1, 0x4000_8000_0000_0000_0000);
    let mut invalid = X87State {
        control_word: 0x37e,
        ..ordinary
    };
    invalid.set_raw(1, 0x7fff_8000_0000_0000_0001);
    invalid.set_tag(1, 2);
    for prior in [ordinary, invalid] {
        install(prior);
        assert_eq!(invoke(exchange, 0, 0x1000, 0, 0), 0);
        assert_eq!(invoke(compare, 0, 0x1002, flags, 0), 0);
        let expected = projection();
        assert_eq!(expected.0.instruction_pointer, 0x1002);
        if prior == invalid {
            assert_eq!(expected.0.pending(), 1);
            assert_eq!(expected.0.opcode, compare.encoding);
        }
        install(prior);
        assert_eq!(pair(), 0);
        assert_eq!(projection(), expected);
    }

    ordinary.set_tag(1, 3);
    install(ordinary);
    let before = projection();
    assert_eq!(pair(), -1);
    assert_eq!(projection(), before);
}

#[test]
fn x87_malformed_private_operations_reject_before_state_or_diagnostic_changes() {
    initialize(0x1000, &[0xf4]);
    let before = snapshot();
    assert_eq!(core_load(0x7000, 8), 0);
    let fault = metadata(DATA);
    for invalid in [63, 15 << 6, 1 << 31] {
        assert_eq!(
            x87::core_x87(invalid, 0, Segment::Ds as u32, 0x1000, 0x46, 0),
            -1
        );
        assert_eq!(snapshot(), before);
        assert_eq!(metadata(DATA), fault);
    }
    for invalid in [
        operation(Kind::Store, Format::None, 0),
        operation(Kind::LoadControl, Format::Register, 0),
        Op {
            index: 7,
            ..operation(Kind::Constant, Format::None, 0)
        },
        operation(Kind::Store, Format::F80, 0),
    ] {
        assert_eq!(
            x87::core_x87(invalid.encode(), 0, Segment::Ds as u32, 0x1000, 0x46, 0),
            -1
        );
        assert_eq!(snapshot(), before);
        assert_eq!(metadata(DATA), fault);
    }
    assert_eq!(
        x87::core_x87(
            operation(Kind::Wait, Format::None, 0).encode(),
            0,
            6,
            0x1000,
            0x46,
            0
        ),
        -1
    );
    assert_eq!(snapshot(), before);
    assert_eq!(metadata(DATA), fault);
}

#[test]
fn x87_pending_detector_precedes_operand_access_and_diagnostics_do_not_erase_pending() {
    initialize(0x1000, &[0xf4]);
    let mut pending = X87State {
        control_word: 0x37e,
        status_word: 0x8081,
        instruction_pointer: 0x1234,
        data_pointer: 0x5678,
        opcode: 0x52b,
        ..X87State::default()
    };
    for index in 0..8 {
        pending.set_raw(index, (0xff80u128 << 64) | index as u128);
    }
    install(pending);
    let load = operation(Kind::Load, Format::F80, 0x52b);
    for load in [
        load,
        operation(Kind::Load, Format::Bcd, 0x723),
        Op {
            pop: 1,
            ..operation(Kind::Store, Format::Bcd, 0x733)
        },
        operation(Kind::Remainder, Format::None, 0x1f8),
        operation(Kind::RemainderNearest, Format::None, 0x1f5),
        operation(Kind::Scale, Format::None, 0x1fd),
        operation(Kind::Extract, Format::None, 0x1f4),
        operation(Kind::SinCos, Format::None, 0x1fb),
    ] {
        assert_eq!(
            invoke(load, 0x800000000000, 0x2000, 0x8d7, 0x12345678),
            X87_EXCEPTION
        );
        assert_eq!(snapshot(), pending);
        assert!(BRIDGE.with_borrow(|bridge| bridge.core.as_ref().unwrap().fault.is_none()));
        core_clear_fault();
    }
    // A no-wait status read still publishes AX from the live, unpublished RAX.
    assert_eq!(
        invoke(
            operation(Kind::StoreStatus, Format::Ax, 0x7e0),
            0,
            0x2001,
            0x46,
            0x123456789abcdef0
        ),
        0
    );
    assert_eq!(snapshot(), pending);
    BRIDGE.with_borrow(|bridge| unsafe {
        assert_eq!(
            (*bridge.core.as_ref().unwrap().state.get()).registers[0],
            0x123456789abc8081
        )
    });
    assert_eq!(
        invoke(operation(Kind::Wait, Format::None, 0), 0, 0x2003, 0x46, 0),
        X87_EXCEPTION
    );
    assert_eq!(snapshot(), pending);
}

#[test]
fn x87_failed_full_span_accesses_precede_stack_status_and_preserve_every_byte() {
    for failing in 0..10 {
        for store in [false, true] {
            initialize(0x1000, &[0xf4]);
            let mut before = X87State {
                instruction_pointer: 0x1111,
                data_pointer: 0x2222,
                opcode: 0x333,
                ..X87State::default()
            };
            if !store {
                before.tag_word = 0;
            }
            for index in 0..8 {
                before.set_raw(
                    index,
                    (0xffffu128 << 64) | 0x8000000000000000 | index as u128,
                );
            }
            install(before);
            let mut op = operation(
                if store { Kind::Store } else { Kind::Load },
                Format::F80,
                if store { 0x73b } else { 0x52b },
            );
            op.pop = u8::from(store);
            let address = 0x4010 - failing;
            assert_eq!(invoke(op, address, 0x1000, 0x8d7, 0), DATA);
            assert_eq!(snapshot(), before, "store={store}, failing={failing}");
            BRIDGE.with_borrow(|bridge| {
                let core = bridge.core.as_ref().unwrap();
                let fault = core.fault.as_ref().unwrap();
                assert_eq!(fault.address, address);
                assert_eq!(fault.width, 10);
                assert_eq!(fault.failing, 0x4010);
                assert_eq!(
                    fault.access,
                    if store { Access::Write } else { Access::Read }
                );
                assert_eq!(&core.image.read_vector(0x4000, 16).unwrap(), &[0xa5; 16]);
            });
        }
    }
}

#[test]
fn x87_exact_ten_byte_loads_keep_raw_encodings_and_update_only_defined_environment_fields() {
    let raw = (0x7fffu128 << 64) | 0x8000000000000011;
    for mode in [
        "long64",
        "protected",
        "compatibility",
        "real",
        "virtual8086",
    ] {
        initialize(0x1000, &[0xf4]);
        let context = if mode == "long64" {
            serde_json::json!({"mode":mode,"fsBase":"0x20000","gsBase":"0x0"})
        } else {
            let mut segments = serde_json::Map::new();
            for name in ["cs", "ss", "ds", "es", "fs", "gs"] {
                segments.insert(name.into(), serde_json::json!({"base":if name=="cs"{"0x10000"}else if name=="fs"{"0x20000"}else{"0x0"},"limit":if mode=="virtual8086"{0xffffu32}else{0xffffffffu32},"default32":false,"usable":true,"type":if name=="cs"&&mode!="virtual8086"{"readable-code"}else{"read-write"}}));
            }
            serde_json::json!({"mode":mode,"segments":segments})
        };
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            core.context = ExecutionContext::from_json(&context).unwrap();
            core.image = Image::new(0x1000, vec![0xf4], 0x24000, vec![0xa5; 16]).unwrap();
            core.image
                .write_bytes(0x24006, &raw.to_le_bytes()[..10])
                .unwrap();
        });
        install(X87State {
            instruction_pointer: u64::MAX,
            data_pointer: u64::MAX,
            opcode: 0x123,
            ..X87State::default()
        });
        assert_eq!(
            invoke(
                operation(Kind::Load, Format::F80, 0x52b),
                0x4006,
                0x100001000,
                0x8d7,
                0
            ),
            0,
            "{mode}"
        );
        let after = snapshot();
        assert_eq!(after.raw(7), raw);
        assert_eq!(after.status_word, 0x3800);
        assert_eq!(after.tag_word, 0xbfff);
        assert_eq!(after.opcode, 0x123);
        assert_eq!(
            after.instruction_pointer,
            if mode == "long64" {
                0x100001000
            } else if matches!(mode, "real" | "virtual8086") {
                0x11000
            } else {
                0x1000
            }
        );
        assert_eq!(
            after.data_pointer,
            if matches!(mode, "real" | "virtual8086") {
                0x24006
            } else {
                0x4006
            }
        );
        assert_eq!(
            invoke(
                operation(Kind::Nop, Format::None, 0x1d0),
                0,
                0x100001002,
                0x8d7,
                0
            ),
            0
        );
        let nop = snapshot();
        assert_eq!(nop.instruction_pointer, after.instruction_pointer + 2);
        assert_eq!(nop.data_pointer, after.data_pointer);
        assert_eq!(nop.opcode, after.opcode);
        assert_eq!(nop.registers, after.registers);
    }
}

fn executable_alias(raw: u128) -> u64 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let bytes = raw.to_le_bytes();
        core.image = Image::mapped(
            vec![
                ("head".into(), bytes[..2].to_vec()),
                ("tail".into(), bytes[2..10].to_vec()),
            ],
            [("head", 0, 2), ("tail", 2, 8)]
                .into_iter()
                .flat_map(|(backing, start, length)| {
                    [0x1000, 0x4000].map(|base| MappingInput {
                        base: base + start,
                        backing: backing.into(),
                        offset: 0,
                        length,
                        permissions: Permissions {
                            read: false,
                            write: base == 0x4000,
                            execute: base == 0x1000,
                        },
                    })
                })
                .collect(),
            MemoryLimits {
                max_backing_bytes: 10,
                max_backings: 2,
                max_mappings: 4,
            },
        )
        .unwrap();
        let owner = core.image.register_source([(0x1000, 10)]).unwrap();
        assert!(core.image.activate(owner));
        owner
    })
}

#[test]
fn x87_split_code_store_invalidates_only_after_store_and_pop_commit() {
    initialize(0x1000, &[0xf4]);
    let raw = (0x3fffu128 << 64) | 0x8000000000000000;
    let owner = executable_alias(raw);
    let mut before = X87State {
        tag_word: 0xfffc,
        opcode: 0x123,
        ..X87State::default()
    };
    before.set_raw(0, raw);
    install(before);
    let mut store = operation(Kind::Store, Format::F80, 0x73b);
    store.pop = 1;
    assert_eq!(invoke(store, 0x4000, 0x1000, 0x8d7, 0), 2);
    let after = snapshot();
    assert_eq!(after.raw(0), raw);
    assert_eq!(after.tag_word, 0xffff);
    assert_eq!(after.status_word, 0x800);
    assert_eq!(after.instruction_pointer, 0x1000);
    assert_eq!(after.data_pointer, 0x4000);
    assert_eq!(after.opcode, 0x123);
    BRIDGE.with_borrow(|bridge| {
        let image = &bridge.core.as_ref().unwrap().image;
        assert!(!image.owner_valid(owner));
        assert!(image.active_changed());
        for (index, expected) in raw.to_le_bytes()[..10].iter().copied().enumerate() {
            assert_eq!(image.fetch(0x1000 + index as u64).unwrap(), expected);
        }
    });

    let owner = executable_alias(raw);
    let mut before = X87State {
        control_word: 0x377,
        tag_word: 0xfffc,
        ..X87State::default()
    };
    before.set_raw(0, (0x407fu128 << 64) | 0x8000000000000000);
    install(before);
    store.format = Format::F32;
    store.encoding = 0x11b;
    assert_eq!(invoke(store, 0x4000, 0x1000, 0x8d7, 0), 0);
    let after = snapshot();
    assert_eq!(after.raw(0), before.raw(0));
    assert_eq!(after.tag_word, before.tag_word);
    assert_eq!(after.status_word, 0x8088);
    assert_eq!(after.instruction_pointer, 0x1000);
    assert_eq!(after.data_pointer, 0x4000);
    assert_eq!(after.opcode, 0x11b);
    BRIDGE.with_borrow(|bridge| {
        let image = &bridge.core.as_ref().unwrap().image;
        assert!(image.owner_valid(owner));
        assert!(!image.active_changed());
        for (index, expected) in raw.to_le_bytes()[..10].iter().copied().enumerate() {
            assert_eq!(image.fetch(0x1000 + index as u64).unwrap(), expected);
        }
    });
}

#[test]
fn x87_operand_accessed_alias_reports_source_change_without_a_payload_store() {
    for store in [false, true] {
        initialize(0x400000, &[0x90]);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            let mut tables = vec![0; 0x4000];
            for (address, value) in [
                (0x1000, 0x2027u64),
                (0x2000, 0x3027),
                (0x3010, 0x4027),
                (0x4000, 0x9027),
            ] {
                tables[address - 0x1000..address - 0x1000 + 8]
                    .copy_from_slice(&value.to_le_bytes());
            }
            let map = |base, backing: &str, offset, length, execute| MappingInput {
                base,
                backing: backing.into(),
                offset,
                length,
                permissions: Permissions {
                    read: true,
                    write: true,
                    execute,
                },
            };
            core.image = Image::physical_mapped(
                vec![
                    ("tables".into(), tables),
                    (
                        "code".into(),
                        vec![
                            0xdf,
                            if store { 0x33 } else { 0x23 },
                            0xb8,
                            7,
                            0xa0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                        ],
                    ),
                    ("data".into(), vec![0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
                ],
                vec![
                    map(0x1000, "tables", 0, 0x3800, false),
                    map(0x4800, "code", 3, 8, false),
                    map(0x4808, "tables", 0x3808, 0x7f8, false),
                    map(0x9000, "code", 0, 11, true),
                    map(0xa000, "data", 0, 10, false),
                ],
                MemoryLimits {
                    max_backings: 3,
                    max_mappings: 5,
                    max_backing_bytes: 0x5000,
                },
            )
            .unwrap();
            core.paging = Some(crate::x86::paging::State {
                cr3: 0x1000,
                ..crate::x86::paging::State::default()
            });
            let owner = core.image.register_source([(0x9000, 11)]).unwrap();
            assert!(core.image.activate(owner));
            if store {
                let state = unsafe { &mut (*core.state.get()).x87 };
                state.control_word = 0x37e;
                state.set_raw(0, 0x7fff8000000000000000);
                state.set_tag(0, 2);
            }
        });
        let op = Op {
            kind: if store { Kind::Store } else { Kind::Load },
            format: Format::Bcd,
            pop: u8::from(store),
            encoding: if store { 0x733 } else { 0x723 },
            ..Op::default()
        };
        assert_eq!(
            x87::core_paged_x87(op.encode(), 0x500000, Segment::Ds as u32, 0x400000, 2, 0),
            2
        );
        BRIDGE.with_borrow(|bridge| {
            let core = bridge.core.as_ref().unwrap();
            assert_eq!(core.image.physical_read(0x9003, 4).unwrap(), 0xa027);
            assert_eq!(core.image.physical_read(0xa000, 1).unwrap(), 0x42);
            let state = unsafe { &(*core.state.get()).x87 };
            assert_eq!(state.instruction_pointer, 0x400000);
            assert_eq!(state.top(), if store { 0 } else { 7 });
            if store {
                assert_eq!(state.status_word, 0x8081);
            } else {
                assert_eq!(state.raw(7), 0x4004a800000000000000);
            }
        });
    }
}
