use crate::embedding::exports::{
    execution::translation,
    memory::{mapped, paging},
    tests::*,
};
use crate::x86::instruction::{DataOperand, Instruction, Operation, RegisterView, Width};
use crate::x86::paging::{
    cache::Cache,
    walk::{self, AccessClass, Request},
};

const PC: u64 = 0x1ffe;

fn expected(value: u64) -> Instruction {
    Instruction {
        pc: PC,
        next_pc: 0x2003,
        operation: Operation::Mov {
            dst: RegisterView::low(0),
            src: DataOperand::Immediate(value),
            width: Width::W32,
        },
    }
}

fn compile_owner() -> u64 {
    assert_eq!(core_compile(PC, 1, 16, 65536), 0);
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

#[test]
fn one_entry_cache_admits_a_two_page_instruction_then_reconciles_evicted_source_before_old_body() {
    let mut bytes = vec![0; 0xc000];
    for (address, word) in [
        (0x1000, 0x2027u64),
        (0x2000, 0x3027),
        (0x3000, 0x4027),
        (0x4008, 0x9027),
        (0x4010, 0xa027),
    ] {
        bytes[address..address + 8].copy_from_slice(&word.to_le_bytes());
    }
    bytes[0x9ffe..0xa000].copy_from_slice(&[0xb8, 0x78]);
    bytes[0xa000..0xa003].copy_from_slice(&[0x56, 0x34, 0x12]);
    bytes[0xbffe..0xc000].copy_from_slice(&[0xb8, 0x89]);
    let config = serde_json::json!({
        "memoryModel":"ia32e-paged",
        "paging":{"cr0":"0x80000033","cr2":"0xdead","cr3":"0x1000","cr4":"0x628","efer":"0x500"},
        "memoryLimits":{"maxBackingBytes":49152,"maxBackings":1,"maxMappings":1},
        "backings":[{"id":"ram","length":49152}],
        "mappings":[{"base":"0x0","backing":"ram","offset":0,"length":49152,"permissions":{"read":true,"write":true,"execute":true}}]
    });
    let length = mapped_input(config, &bytes);
    assert_eq!(mapped::core_init_mapped(PC, length), 0);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.translation_cache = Cache::with_capacity(1);
        let bank = core.paging.unwrap();
        let request = Request {
            linear: PC,
            access: Access::Execute,
            class: AccessClass::Supervisor { ac: false },
        };
        let translation = walk::walk(
            &mut core.image,
            &bank,
            request,
            &crate::x86::protection_keys::State::ZERO,
        )
        .unwrap();
        core.image.physical_fetch(translation.physical).unwrap();
        core.translation_cache.fill(&bank, request, translation);
        // Retarget before creating the owner: the following eviction changes authority
        // without a later physical write that could independently invalidate its proof.
        core.image.physical_write(0x4008, 8, 0xb027).unwrap();
    });
    let old = compile_owner();
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert!(
            core.translation_owners
                .instruction_matches(old, expected(0x12345678))
        );
    });
    assert_eq!(
        paging::core_paged_debug_begin(PC, 2, 0),
        0,
        "one admission completes even though source pages cannot coexist"
    );
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert!(!core.image.active_changed());
        assert!(
            core.translation_cache
                .observe(
                    core.paging.as_ref().unwrap(),
                    Request {
                        linear: PC,
                        access: Access::Execute,
                        class: AccessClass::Supervisor { ac: false }
                    },
                    &crate::x86::protection_keys::State::ZERO,
                )
                .unwrap()
                .is_none()
        );
    });
    assert_eq!(
        paging::core_paged_debug_begin(PC, 2, 0),
        26,
        "freshly exposed mapping must not execute the old generated MOV"
    );
    assert_eq!(translation::core_validity(old), 0);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert!(core.image.active_changed());
        assert_eq!(unsafe { (*core.state.get()).registers[0] }, 0);
    });
    let fresh = compile_owner();
    BRIDGE.with_borrow(|bridge| {
        assert!(
            bridge
                .core
                .as_ref()
                .unwrap()
                .translation_owners
                .instruction_matches(fresh, expected(0x12345689))
        )
    });
    assert_eq!(
        paging::core_paged_debug_begin(PC, 2, 0),
        0,
        "fresh semantics admit without a recompile/evict livelock"
    );
}
