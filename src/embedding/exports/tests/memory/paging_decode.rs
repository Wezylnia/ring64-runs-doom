use super::*;
use crate::x86::instruction::{DataOperand, Operation, RegisterView, Width};
use crate::x86::paging::walk::{Failure, PhysicalPhase, physical_counts};

const PC: u64 = 0x1000;
const RF: u64 = 0x10002;

fn fixture(family: &str, warm: bool, executable: bool) -> Vec<u8> {
    let mut bytes = vec![0xa5; 0xa000];
    let (entries, bank) = match family {
        "ia32e" | "long64" | "compat32" => (
            vec![
                (0x1000, 0x2007u64, 8),
                (0x2000, 0x3007, 8),
                (0x3000, 0x4007, 8),
                (0x4008, 0x9007, 8),
            ],
            serde_json::json!({"cr0":"0x80000033","cr2":"0xdead","cr3":"0x1000","cr4":"0x628","efer":"0x500"}),
        ),
        "legacy32" => (
            vec![(0x1000, 0x2007, 4), (0x2004, 0x9007, 4)],
            serde_json::json!({"cr0":"0x80000033","cr2":"0xdead","cr3":"0x1000","cr4":"0x608","efer":"0x0"}),
        ),
        "pae" => (
            vec![(0x2000, 0x3007, 8), (0x3008, 0x9007, 8)],
            // Root RAM remains 0xa5: only this captured quartet is authoritative.
            serde_json::json!({"cr0":"0x80000033","cr2":"0xdead","cr3":"0x1000","cr4":"0x628","efer":"0x0","pdpte":["0x2001","0x0","0x0","0x0"]}),
        ),
        "pg0" => (
            vec![],
            serde_json::json!({"cr0":"0x33","cr2":"0xdead","cr3":"0xdead000","cr4":"0x628","efer":"0x100"}),
        ),
        _ => panic!("unknown literal paging family"),
    };
    let code = if family == "pg0" { 0x1000 } else { 0x9000 };
    bytes[code..code + 3].copy_from_slice(&[0x66, 0x89, 0xc0]);
    for &(address, word, width) in &entries {
        let word = word | if warm { 0x20 } else { 0 };
        bytes[address..address + width].copy_from_slice(&word.to_le_bytes()[..width]);
    }
    let mut context = ExecutionContext::default().to_json();
    if family != "long64" {
        context["mode"] = serde_json::json!(if family == "compat32" {
            "compatibility"
        } else {
            "protected"
        });
        context["segments"]["cs"]["long"] = serde_json::json!(false);
        context["segments"]["cs"]["default32"] = serde_json::json!(true);
    }
    let config = serde_json::json!({
        "memoryModel":if matches!(family, "ia32e" | "long64" | "compat32") { "ia32e-paged" } else { "physical" },
        "context":context,"paging":bank,
        "memoryLimits":{"maxBackingBytes":40960,"maxBackings":1,"maxMappings":1},
        "backings":[{"id":"ram","length":40960}],
        "mappings":[{"base":"0x0","backing":"ram","offset":0,"length":40960,
            "permissions":{"read":true,"write":true,"execute":executable}}]
    });
    let compatibility =
        (family == "compat32").then(|| serde_json::to_vec(&config["context"]).unwrap());
    let length = mapped_input(config, &bytes);
    assert_eq!(mapped::core_init_mapped(PC, length), 0, "{family}");
    if let Some(input) = compatibility {
        // The IA32e initializer starts in Long64; the SDK then imports context.
        BRIDGE.with_borrow_mut(|bridge| bridge.input = input);
        assert_eq!(super::super::context::core_set_context(), 0, "{family}");
    }
    BRIDGE.with_borrow_mut(|bridge| unsafe {
        let state = &mut *bridge.core.as_mut().unwrap().state.get();
        state.registers[0] = 0x1122334455667788;
        state.flags = RF;
    });
    for &(address, word, width) in &entries {
        bytes[address..address + width].copy_from_slice(&(word | 0x20).to_le_bytes()[..width]);
    }
    bytes
}

fn assert_mov(bank: u8, instruction: crate::x86::instruction::Instruction) {
    assert_eq!(instruction.pc, PC);
    assert_eq!(instruction.next_pc, PC + 3);
    assert_eq!(
        instruction.operation,
        Operation::Mov {
            dst: RegisterView::low(bank),
            src: DataOperand::Register(RegisterView::low(bank)),
            width: Width::W16,
        }
    );
}

fn decode_and_check(expected: &[u8]) {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let bank = core.paging;
        let context = core.context.clone();
        let registers = unsafe { (*core.state.get()).registers };
        assert_mov(0, core.paged_decode(PC).unwrap());
        assert_eq!(
            core.image
                .physical_read_bytes(0, expected.len() as u32)
                .unwrap(),
            expected
        );
        assert_eq!(core.paging, bank);
        assert_eq!(core.context, context);
        unsafe {
            assert_eq!((*core.state.get()).registers, registers);
            assert_eq!((*core.state.get()).rip, PC);
            assert_eq!((*core.state.get()).flags, RF);
        }
    });
}

#[test]
fn warm_paged_decode_reads_one_hierarchy_per_unchanged_leaf_span() {
    for (family, counts) in [
        ("ia32e", [4, 32, 0, 0, 0, 0]),
        ("legacy32", [2, 8, 0, 0, 0, 0]),
        ("pae", [2, 16, 0, 0, 0, 0]),
        ("pg0", [0; 6]),
    ] {
        let expected = fixture(family, true, true);
        physical_counts::take();
        decode_and_check(&expected);
        assert_eq!(physical_counts::take(), counts, "{family}");
        decode_and_check(&expected);
        assert_eq!(physical_counts::take(), [0; 6], "retained {family}");
    }
}

#[test]
fn cold_paged_decode_keeps_accessed_writes_and_their_revalidation() {
    for (family, counts) in [
        // First byte: hierarchy + A current-word OR + triangular rereads + final peek.
        // Later bytes reuse that admitted retained translation.
        ("ia32e", [22, 176, 4, 32, 4, 32]),
        ("legacy32", [9, 36, 2, 8, 2, 8]),
        ("pae", [9, 72, 2, 16, 2, 16]),
    ] {
        let expected = fixture(family, false, true);
        physical_counts::take();
        decode_and_check(&expected);
        assert_eq!(physical_counts::take(), counts, "{family}");
    }
}

#[cfg(not(feature = "parallel"))]
fn activate_single_instruction() -> u64 {
    assert_eq!(translation::core_compile(PC, 1, 16, 65536), 0);
    let owner = BRIDGE.with_borrow(|bridge| {
        let metadata: serde_json::Value = serde_json::from_slice(&bridge.info).unwrap();
        assert_eq!(metadata["instructionCount"], 1);
        u64::from_str_radix(
            metadata["owner"].as_str().unwrap().trim_start_matches("0x"),
            16,
        )
        .unwrap()
    });
    assert_eq!(translation::core_publish(owner), 1);
    assert_eq!(translation::core_activate(owner), 1);
    owner
}

#[cfg(not(feature = "parallel"))]
#[test]
fn reached_long64_instruction_requires_a_live_tlb_hit() {
    let expected = fixture("long64", true, true);
    let owner = activate_single_instruction();
    let check_miss = || {
        BRIDGE.with_borrow(|bridge| {
            assert_eq!(
                bridge.core.as_ref().unwrap().warm_paged_instruction(PC),
                None
            );
        });
        physical_counts::take();
        assert_eq!(paging::core_paged_loop_guard(PC, 3, 2, 0), 0);
        assert_eq!(physical_counts::take(), [0; 6]);
        assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
        assert_eq!(physical_counts::take(), [4, 32, 0, 0, 0, 0]);
        assert_eq!(paging::core_paged_loop_guard(PC, 3, 2, 0), 1);
        for (bytes, flags, shadow) in [(3, RF, 0), (3, 0x102, 0), (3, 2, 1), (4097, 2, 0)] {
            assert_eq!(paging::core_paged_loop_guard(PC, bytes, flags, shadow), 2);
        }
        assert_eq!(physical_counts::take(), [0; 6]);
    };
    check_miss();
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let exact = core.paged_decode(PC).unwrap();
        assert_mov(0, exact);
        assert_eq!(core.warm_paged_instruction(PC), Some(exact));
        let state = core.paging.unwrap();
        assert!(
            core.translation_owners
                .guarded_code_start(owner, PC, &state)
                .is_some()
        );
        assert_eq!(
            core.translation_owners
                .guarded_code_start(owner, PC + 1, &state),
            None
        );
        assert_eq!(
            core.translation_owners
                .guarded_code_start(owner + 1, PC, &state),
            None
        );
        assert_eq!(
            core.image
                .physical_read_bytes(0, expected.len() as u32)
                .unwrap(),
            expected
        );
        assert_eq!(core.paging.unwrap().cr2, 0xdead);
        assert_eq!(unsafe { (*core.state.get()).flags }, RF);
        core.translation_cache.clear();
    });
    check_miss();
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.translation_cache.clear();
        assert_mov(0, core.paged_decode(PC).unwrap());
        core.reconcile_translation_changes();
        assert_eq!(core.warm_paged_instruction(PC), None);
    });
    // The current fetch can be re-admitted without reading its bytes.
    physical_counts::take();
    assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
    assert_eq!(physical_counts::take(), [0; 6]);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_dword_walk_requires_the_active_source_and_keeps_flags_on_refusal() {
    fixture("long64", true, true);
    BRIDGE.with_borrow_mut(|bridge| {
        let image = &mut bridge.core.as_mut().unwrap().image;
        image.physical_write(0x4010, 8, 0x8027).unwrap();
        image.physical_write(0x4018, 8, 0x8027).unwrap();
        image.physical_write(0x8000, 4, 0xaabbccdd).unwrap();
    });
    activate_single_instruction();
    assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
    assert_eq!(paging::core_paged_loop_guard(PC, 3, 2, 0), 1);
    assert_eq!(paging::core_paged_loop_walk_dword(0x2000), 0xaabbccdd);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        assert_eq!(core.paging.unwrap().cr2, 0xdead);
        assert_eq!(unsafe { (*core.state.get()).flags }, RF);
        core.image.physical_write(0x9000, 1, 0x90).unwrap();
        assert!(core.image.active_changed());
    });
    physical_counts::take();
    assert_eq!(paging::core_paged_loop_walk_dword(0x3000), u64::MAX);
    assert_eq!(physical_counts::take(), [0; 6]);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert_eq!(core.paging.unwrap().cr2, 0xdead);
        assert_eq!(unsafe { (*core.state.get()).flags }, RF);
    });
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_upper_walk_replays_a_cold_leaf_and_reloads_words_on_new_entry() {
    fixture("long64", true, true);
    BRIDGE.with_borrow_mut(|bridge| {
        let image = &mut bridge.core.as_mut().unwrap().image;
        for (address, word) in [
            (0x3008, 0x5027),
            (0x5000, 0x7027),
            (0x5008, 0x8007),
            (0x5010, 0x8027),
            (0x6010, 0x7027),
            (0x6018, 0),
        ] {
            image.physical_write(address, 8, word).unwrap();
        }
        image.physical_write(0x7000, 4, 0x11223344).unwrap();
        image.physical_write(0x8000, 4, 0xaabbccdd).unwrap();
    });
    let owner = activate_single_instruction();
    assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
    assert_eq!(paging::core_paged_walk_guard(PC, 3, 2, 0), 1);
    physical_counts::take();
    assert_eq!(paging::core_paged_loop_walk_dword(0x200000), 0x11223344);
    assert_eq!(physical_counts::take(), [4, 32, 0, 0, 0, 0]);
    assert_eq!(paging::core_paged_loop_walk_dword(0x201000), u64::MAX);
    assert_eq!(physical_counts::take(), [1, 8, 0, 0, 0, 0]);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert!(core.guarded_walk.borrow().is_none());
        assert_eq!(core.image.physical_read(0x5008, 8).unwrap(), 0x8007);
    });
    assert_eq!(
        super::super::scalar::core_paged_load(0x201000, 4),
        0xaabbccdd
    );
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        assert_eq!(core.image.physical_read(0x5008, 8).unwrap(), 0x8027);
        // A write in checked execution can change a data-table prefix that
        // does not belong to the still-active executable source.
        core.image.physical_write(0x3008, 8, 0x6027).unwrap();
        assert!(core.image.source_is_active(owner));
    });
    assert_eq!(paging::core_paged_walk_guard(PC, 3, 2, 0), 1);
    physical_counts::take();
    assert_eq!(paging::core_paged_loop_walk_dword(0x202000), 0x11223344);
    assert_eq!(physical_counts::take(), [4, 32, 0, 0, 0, 0]);
    assert_eq!(paging::core_paged_loop_walk_dword(0x203000), u64::MAX);
    assert_eq!(physical_counts::take(), [1, 8, 0, 0, 0, 0]);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert!(core.guarded_walk.borrow().is_none());
        assert_eq!(core.paging.unwrap().cr2, 0xdead);
    });
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_upper_walk_reloads_after_host_mutation_and_guest_root_change() {
    fixture("long64", true, true);
    BRIDGE.with_borrow_mut(|bridge| {
        let image = &mut bridge.core.as_mut().unwrap().image;
        for (address, word) in [
            (0x3008, 0x5027),
            (0x5000, 0x7027),
            (0x5008, 0x7027),
            (0x6000, 0x8027),
            (0x6008, 0x8027),
            (0x7008, 0x2027),
            (0x7000, 0x2027),
        ] {
            image.physical_write(address, 8, word).unwrap();
        }
        image.physical_write(0x7010, 4, 0x11223344).unwrap();
        image.physical_write(0x8010, 4, 0xaabbccdd).unwrap();
    });
    activate_single_instruction();
    assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
    assert_eq!(paging::core_paged_walk_guard(PC, 3, 2, 0), 1);
    assert_eq!(paging::core_paged_loop_walk_dword(0x200010), 0x11223344);

    BRIDGE.with_borrow_mut(|bridge| bridge.input = 0x6027u64.to_le_bytes().to_vec());
    assert_eq!(mapped::core_write_memory(0x3008), 0);
    assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
    assert_eq!(paging::core_paged_walk_guard(PC, 3, 2, 0), 1);
    physical_counts::take();
    assert_eq!(paging::core_paged_loop_walk_dword(0x201010), 0xaabbccdd);
    assert_eq!(physical_counts::take(), [4, 32, 0, 0, 0, 0]);

    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.paging.as_mut().unwrap().cr3 = 0x7000;
        core.translation_cache.clear();
        core.guest_root_changed();
    });
    assert_eq!(paging::core_paged_loop_walk_dword(0x200010), u64::MAX);
    activate_single_instruction();
    assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
    assert_eq!(paging::core_paged_walk_guard(PC, 3, 2, 0), 1);
    physical_counts::take();
    assert_eq!(paging::core_paged_loop_walk_dword(0x200010), 0xaabbccdd);
    assert_eq!(physical_counts::take(), [4, 32, 0, 0, 0, 0]);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn paged_loop_revalidation_rejects_the_same_instruction_on_a_new_physical_page() {
    fixture("long64", true, true);
    BRIDGE.with_borrow_mut(|bridge| {
        bridge
            .core
            .as_mut()
            .unwrap()
            .image
            .physical_write_bytes(0x8000, &[0x66, 0x89, 0xc0])
            .unwrap();
    });
    let owner = activate_single_instruction();
    assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
    assert_eq!(paging::core_paged_loop_guard(PC, 3, 2, 0), 1);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        use crate::x86::paging::walk::{AccessClass, Request};
        let state = core.paging.as_ref().unwrap();
        let request = Request {
            linear: PC,
            access: Access::Execute,
            class: AccessClass::operand(core.context.cpl(), false),
        };
        let mut retained = core
            .translation_cache
            .observe(state, request, &crate::x86::protection_keys::State::ZERO)
            .unwrap()
            .unwrap();
        // Replacing only the retained view isolates the binding check from
        // the code owner's physical page-table write subscription.
        retained.physical_base = 0x8000;
        retained.physical = 0x8000;
        retained.cache_id = None;
        core.translation_cache.clear();
        core.translation_cache.fill(state, request, retained);
        assert!(!core.image.active_changed());
    });
    assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
    assert_eq!(paging::core_paged_loop_guard(PC, 3, 2, 0), 0);
    assert_eq!(translation::core_validity(owner), 1);
    assert_eq!(translation::core_activate(owner), 0);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn reached_long64_instruction_keeps_byte_and_execute_source_invalidation() {
    use crate::memory::{MappingInput, Permissions};

    for revoke_execute in [false, true] {
        fixture("long64", true, true);
        let owner = activate_single_instruction();
        assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            assert!(core.warm_paged_instruction(PC).is_some());
            if revoke_execute {
                core.image
                    .set_mappings(vec![MappingInput {
                        base: 0,
                        backing: "ram".into(),
                        offset: 0,
                        length: 0xa000,
                        permissions: Permissions {
                            read: true,
                            write: true,
                            execute: false,
                        },
                    }])
                    .unwrap();
            } else {
                core.image.physical_write(0x9002, 1, 0xc9).unwrap();
            }
            assert!(core.image.active_changed());
            assert_eq!(core.warm_paged_instruction(PC), None);
        });
        assert_eq!(paging::core_paged_loop_guard(PC, 3, 2, 0), 0);
        assert_eq!(translation::core_validity(owner), 0);
        if revoke_execute {
            assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 25);
            BRIDGE.with_borrow(|bridge| {
                let core = bridge.core.as_ref().unwrap();
                assert!(matches!(core.paging_failure, Some(Failure::Physical(fault))
                    if fault.phase == PhysicalPhase::FinalAccess
                        && fault.linear == PC
                        && fault.physical.address == 0x9000
                        && fault.physical.cause == Cause::Permission));
                assert_eq!(core.paging.unwrap().cr2, 0xdead);
            });
        } else {
            assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 26);
            BRIDGE.with_borrow_mut(|bridge| {
                assert_mov(1, bridge.core.as_mut().unwrap().paged_decode(PC).unwrap());
            });
        }
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn reached_paged_32_bit_instruction_uses_the_translated_cs_span() {
    for (family, next_pte, width, reads) in [
        ("legacy32", 0x2008, 4, 2),
        ("pae", 0x3010, 8, 2),
        ("compat32", 0x4010, 8, 4),
    ] {
        fixture(family, true, true);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            core.context.set_segment_base(Segment::Cs, 0x1000);
            core.image.physical_write(next_pte, width, 0x9027).unwrap();
        });
        let owner = activate_single_instruction();
        BRIDGE.with_borrow(|bridge| {
            assert_eq!(
                bridge.core.as_ref().unwrap().warm_paged_instruction(PC),
                None
            );
        });
        physical_counts::take();
        assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0, "{family}");
        assert_eq!(
            physical_counts::take(),
            [reads, reads * u64::from(width), 0, 0, 0, 0],
            "{family}"
        );
        assert_eq!(paging::core_paged_loop_guard(PC, 3, 2, 0), 1, "{family}");
        BRIDGE.with_borrow(|bridge| {
            let context = &bridge.core.as_ref().unwrap().context;
            assert!(context.flat_paged_read_span32(0xffff_fff8, 8), "{family}");
            assert!(!context.flat_paged_read_span32(0xffff_fffc, 8), "{family}");
            assert!(
                !context.flat_paged_read_span32(0x1_0000_0000, 1),
                "{family}"
            );
            assert!(!context.flat_paged_read_span32(u64::MAX, 2), "{family}");
        });
        assert_eq!(
            paging::core_paged_read_loop_guard32(PC, 3, 2, 0),
            1,
            "{family}"
        );
        BRIDGE.with_borrow_mut(|bridge| {
            let context = &mut bridge.core.as_mut().unwrap().context;
            context.set_segment_base(Segment::Ds, 1);
            assert!(!context.flat_paged_read_span32(0x1000, 8), "{family}");
        });
        assert_eq!(
            paging::core_paged_read_loop_guard32(PC, 3, 2, 0),
            2,
            "{family}"
        );
        assert_eq!(paging::core_paged_loop_guard(PC, 3, 2, 0), 1, "{family}");
        assert_eq!(physical_counts::take(), [0; 6], "{family}");
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            let exact = core.paged_decode(PC).unwrap();
            assert_mov(0, exact);
            assert_eq!(core.warm_paged_instruction(PC), Some(exact), "{family}");
            core.image.physical_write(0x9002, 1, 0xc9).unwrap();
            assert_eq!(core.warm_paged_instruction(PC), None);
        });
        assert_eq!(translation::core_validity(owner), 0);
        assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 26);
        BRIDGE.with_borrow_mut(|bridge| {
            assert_mov(1, bridge.core.as_mut().unwrap().paged_decode(PC).unwrap());
        });
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn reached_paged_32_bit_instruction_falls_back_for_page_splits_and_cs_wrap() {
    for (base, locations, pte, physical) in [
        (0xffe, [0x9ffe, 0x9fff, 0x8000], 0x2008, 0x8027),
        (0xffff_f0f0, [0x90f0, 0x90f1, 0x90f2], 0x2000, 0x9027),
    ] {
        fixture("legacy32", true, true);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            core.context.set_segment_base(Segment::Cs, base);
            core.image.physical_write(pte, 4, physical).unwrap();
            for (address, byte) in locations.into_iter().zip([0x66, 0x89, 0xc0]) {
                core.image.physical_write(address, 1, byte).unwrap();
            }
        });
        activate_single_instruction();
        assert_eq!(paging::core_paged_debug_begin(PC, RF, 0), 0);
        assert_eq!(paging::core_paged_loop_guard(PC, 3, 2, 0), 2);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            assert_mov(0, core.paged_decode(PC).unwrap());
            assert_eq!(core.warm_paged_instruction(PC), None, "CS base {base:#x}");
            assert_eq!(core.paging.unwrap().cr2, 0xdead);
        });
    }
}

#[test]
fn paged_decode_retains_a_guest_pte_remap_until_invlpg() {
    fixture("ia32e", true, true);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        assert_mov(0, core.paged_decode(PC).unwrap());
        for (offset, byte) in [0x66, 0x89, 0xc9].into_iter().enumerate() {
            core.image
                .physical_write(0x8000 + offset as u64, 1, byte)
                .unwrap();
        }
        core.image.physical_write(0x4008, 8, 0x8027).unwrap();
        assert_mov(0, core.paged_decode(PC).unwrap());
    });
    assert_eq!(
        paging::core_paging_control(4, Segment::Ds as u32, PC, PC, RF, 0),
        0
    );
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        assert_mov(1, core.paged_decode(PC).unwrap());
        assert_eq!(core.paging.unwrap().cr2, 0xdead);
        assert_eq!(unsafe { (*core.state.get()).flags }, RF);
    });
}

#[test]
fn pg0_decode_keeps_actual_physical_execute_permission_checks() {
    fixture("pg0", true, false);
    physical_counts::take();
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        assert!(
            matches!(core.paged_decode(PC), Err(DecodeError::Paging(Failure::Physical(fault)))
            if fault.phase == PhysicalPhase::FinalAccess
                && fault.physical.address == PC
                && fault.physical.access == Access::Execute
                && fault.physical.cause == Cause::Permission)
        );
        assert_eq!(core.paging.unwrap().cr2, 0xdead);
        assert_eq!(unsafe { (*core.state.get()).flags }, RF);
    });
    assert_eq!(physical_counts::take(), [0; 6]);
}

#[test]
fn lmsw_memory_checks_privilege_and_the_full_word_before_publishing_cr0() {
    use crate::x86::context::Mode;
    use crate::x86::protection::ProtectionPhase;

    fixture("pae", true, true);
    let (before, context) = BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.image.physical_write(0x3010, 8, 0).unwrap();
        core.image.physical_write(0x9fff, 1, 0xf4).unwrap();
        core.context.set_mode_and_cpl(Mode::Protected, 3);
        let debug = unsafe { &mut (*core.state.get()).debug.registers };
        debug.write(0, 0x2000);
        debug.write(7, 0x0003_0001);
        (core.paging.unwrap(), core.context.clone())
    });
    let lmsw = || paging::core_paging_control(7, Segment::Ds as u32, 0x1fff, PC, RF, 0);
    physical_counts::take();
    assert_eq!(lmsw(), crate::embedding::layout::PROTECTION);
    assert_eq!(physical_counts::take(), [0; 6], "no explicit read at CPL3");
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        assert_eq!(core.paging, Some(before));
        assert_eq!(core.context, context);
        let fault = core.protection_fault.take().unwrap();
        assert_eq!(
            (fault.vector, fault.error_code, fault.phase),
            (13, 0, ProtectionPhase::Privilege)
        );
        core.context.set_mode_and_cpl(Mode::Protected, 0);
    });
    assert_eq!(lmsw(), 24);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        assert_eq!(core.paging.unwrap().cr0, before.cr0);
        assert_eq!(core.paging.unwrap().cr2, 0x2000);
        assert_eq!(core.debug_candidate, 0);
        assert!(
            matches!(core.paging_failure.take(), Some(Failure::Page(fault)) if
            fault.address == 0x2000 && fault.error_code == 0 && fault.access == Access::Read)
        );
        core.image.physical_write(0x3010, 8, 0x8007).unwrap();
        core.image.physical_write(0x8000, 1, 0xff).unwrap();
    });
    assert_eq!(lmsw(), 0);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        let after = core.paging.unwrap();
        assert_eq!(after.cr0, 0x80000035);
        assert_eq!(after.cr2, 0x2000);
        assert_eq!(after.pdpte, before.pdpte, "poisoned PAE root stays dormant");
        assert_eq!(core.debug_candidate, 1, "watchpoint covers the second byte");
        assert_eq!(
            core.image.physical_read(0x3010, 8),
            Ok(0x8027),
            "read sets A only"
        );
    });
}

#[test]
fn lmsw_register_writes_only_numeric_bits_and_a_sticky_pe() {
    use crate::x86::context::Mode;

    fixture("pg0", true, true);
    let initial_context = BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.paging.as_mut().unwrap().cr0 = 0x6004003e;
        core.context.set_mode_and_cpl(Mode::Real, 0);
        core.context.clone()
    });
    assert_eq!(paging::core_paging_control(6, 0, 0xfff1, PC, RF, 0), 0);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        let mut expected = initial_context;
        expected.set_mode_and_cpl(Mode::Protected, 0);
        assert_eq!(core.context, expected);
        assert_eq!(core.paging.unwrap().cr0, 0x60040031);
    });
    assert_eq!(paging::core_paging_control(6, 0, 0xfffa, PC, RF, 0), 0);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert_eq!(core.context.mode(), Mode::Protected);
        assert_eq!(core.paging.unwrap().cr0, 0x6004003b);
        assert_eq!(core.paging.unwrap().cr3, 0xdead000);
    });
}

#[test]
fn warm_decode_keeps_each_later_byte_physical_execute_admission() {
    use crate::memory::{MappingInput, MemoryLimits, Permissions};
    for (family, physical) in [("ia32e", 0x9000), ("pg0", 0x1000)] {
        for mapped_tail in [false, true] {
            let bytes = fixture(family, true, true);
            BRIDGE.with_borrow_mut(|bridge| {
                let core = bridge.core.as_mut().unwrap();
                let mut mappings = vec![MappingInput {
                    base: 0,
                    backing: "ram".into(),
                    offset: 0,
                    length: physical + 1,
                    permissions: Permissions {
                        read: true,
                        write: true,
                        execute: true,
                    },
                }];
                if mapped_tail {
                    mappings.push(MappingInput {
                        base: u64::from(physical + 1),
                        backing: "ram".into(),
                        offset: physical + 1,
                        length: 0xa000 - physical - 1,
                        permissions: Permissions {
                            read: true,
                            write: true,
                            execute: false,
                        },
                    });
                }
                core.image = Image::physical_mapped(
                    vec![("ram".into(), bytes)],
                    mappings,
                    MemoryLimits {
                        max_backing_bytes: 0xa000,
                        max_backings: 1,
                        max_mappings: 2,
                    },
                )
                .unwrap();
                let bank = core.paging;
                let context = core.context.clone();
                let before = unsafe { (*core.state.get()).registers };
                let fault = match core.paged_decode(PC) {
                    Err(DecodeError::Paging(Failure::Physical(fault))) => fault,
                    other => panic!("expected later-byte physical refusal: {other:?}"),
                };
                assert_eq!(fault.linear, PC + 1);
                assert_eq!(fault.access, Access::Execute);
                assert_eq!(fault.phase, PhysicalPhase::FinalAccess);
                assert_eq!(fault.physical.address, u64::from(physical + 1));
                assert_eq!(fault.physical.failing, u128::from(physical + 1));
                assert_eq!(fault.physical.width, 1);
                assert_eq!(fault.physical.access, Access::Execute);
                assert_eq!(
                    fault.physical.cause,
                    if mapped_tail {
                        Cause::Permission
                    } else {
                        Cause::Unmapped
                    }
                );
                assert_eq!(core.paging, bank);
                assert_eq!(core.context, context);
                unsafe {
                    assert_eq!((*core.state.get()).registers, before);
                    assert_eq!((*core.state.get()).rip, PC);
                    assert_eq!((*core.state.get()).flags, RF);
                }
            });
        }
    }
}

#[test]
fn warm_decode_hits_remain_in_cold_cross_page_accessed_alias_revalidation() {
    use crate::memory::{MappingInput, MemoryLimits, Permissions};
    let mut bytes = fixture("ia32e", true, true);
    bytes.resize(0xb000, 0xa5);
    bytes[0x9ffc] = 0xb8; // MOV EAX, imm32 crosses onto the cold next page.
    bytes[0x9ffd..0xa005].copy_from_slice(&0xa007u64.to_le_bytes());
    let mappings = [
        (0, 0, 0x4010),
        (0x4010, 0x9ffd, 8), // PTE2.A aliases an already consumed immediate byte.
        (0x4018, 0x4018, 0xb000 - 0x4018),
    ]
    .into_iter()
    .map(|(base, offset, length)| MappingInput {
        base,
        backing: "ram".into(),
        offset,
        length,
        permissions: Permissions {
            read: true,
            write: true,
            execute: true,
        },
    })
    .collect();
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.image = Image::physical_mapped(
            vec![("ram".into(), bytes)],
            mappings,
            MemoryLimits {
                max_backing_bytes: 0xb000,
                max_backings: 1,
                max_mappings: 3,
            },
        )
        .unwrap();
        let bank = core.paging;
        let context = core.context.clone();
        let before = unsafe { (*core.state.get()).registers };
        assert!(matches!(
            core.paged_decode(0x1ffc),
            Err(DecodeError::PagingRestart)
        ));
        assert_eq!(core.image.physical_read(0x4010, 8).unwrap(), 0xa027);
        assert_eq!(core.image.physical_read(0x9ffd, 1).unwrap(), 0x27);
        let instruction = core.paged_decode(0x1ffc).unwrap();
        assert_eq!(instruction.pc, 0x1ffc);
        assert_eq!(instruction.next_pc, 0x2001);
        assert_eq!(
            instruction.operation,
            Operation::Mov {
                dst: RegisterView::low(0),
                src: DataOperand::Immediate(0xa027),
                width: Width::W32,
            }
        );
        assert_eq!(core.paging, bank);
        assert_eq!(core.context, context);
        unsafe {
            assert_eq!((*core.state.get()).registers, before);
            assert_eq!((*core.state.get()).rip, PC);
            assert_eq!((*core.state.get()).flags, RF);
        }
    });
}

#[test]
fn pg0_decode_keeps_later_required_byte_cs_limit_priority() {
    fixture("pg0", true, true);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let mut context = core.context.to_json();
        context["segments"]["cs"]["limit"] = serde_json::json!(0x1000);
        core.context = ExecutionContext::from_json(&context).unwrap();
        let context = core.context.clone();
        let bank = core.paging;
        let registers = unsafe { (*core.state.get()).registers };
        let fault = match core.paged_decode(PC) {
            Err(DecodeError::Segment(fault)) => fault,
            other => panic!("expected next required-byte CS limit refusal: {other:?}"),
        };
        assert_eq!(fault.segment, Segment::Cs);
        assert_eq!(fault.offset, PC + 1);
        assert_eq!(fault.width, 1);
        assert_eq!(fault.access, Access::Execute);
        assert_eq!(fault.cause, SegmentCause::Limit);
        assert_eq!(core.context, context);
        assert_eq!(core.paging, bank);
        unsafe {
            assert_eq!((*core.state.get()).registers, registers);
            assert_eq!((*core.state.get()).rip, PC);
            assert_eq!((*core.state.get()).flags, RF);
        }
    });
}

#[test]
fn required_paged_decode_admits_fifteen_bytes_but_never_fetches_byte_sixteen() {
    use crate::memory::{MappingInput, MemoryLimits, Permissions};
    for length in [15, 16] {
        let mut bytes = fixture("ia32e", true, true);
        let mut encoding = vec![0x66; length - 1];
        encoding.push(0x90);
        bytes[0x9ff8..0xa000].copy_from_slice(&encoding[..8]);
        bytes[0x8000..0x8000 + length - 8].copy_from_slice(&encoding[8..]);
        bytes[0x4010..0x4018].copy_from_slice(&0x8027u64.to_le_bytes());
        let mappings = [
            (0, 0x8007, true),
            (0x8007, 1, false),
            (0x8008, 0xa000 - 0x8008, true),
        ]
        .into_iter()
        .map(|(base, length, execute)| MappingInput {
            base,
            backing: "ram".into(),
            offset: base as u32,
            length,
            permissions: Permissions {
                read: true,
                write: true,
                execute,
            },
        })
        .collect();
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            core.image = Image::physical_mapped(
                vec![("ram".into(), bytes.clone())],
                mappings,
                MemoryLimits {
                    max_backing_bytes: 0xa000,
                    max_backings: 1,
                    max_mappings: 3,
                },
            )
            .unwrap();
            let bank = core.paging;
            let before = unsafe { (*core.state.get()).registers };
            if length == 15 {
                let instruction = core.paged_decode(0x1ff8).unwrap();
                assert_eq!(instruction.pc, 0x1ff8);
                assert_eq!(instruction.next_pc, 0x2007);
                assert_eq!(instruction.operation, Operation::Nop);
            } else {
                assert_eq!(
                    core.paged_decode(0x1ff8),
                    Err(DecodeError::InstructionLength)
                );
            }
            assert_eq!(core.paging, bank);
            assert_eq!(core.image.physical_read_bytes(0, 0xa000).unwrap(), bytes);
            unsafe {
                assert_eq!((*core.state.get()).registers, before);
                assert_eq!((*core.state.get()).flags, RF);
            }
        });
    }
}
