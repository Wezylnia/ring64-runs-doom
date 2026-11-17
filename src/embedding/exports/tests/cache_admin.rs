use super::*;
use crate::x86::access::GuestMemory;
use crate::x86::paging::walk::{AccessClass, Request};

fn compile_owner(publish: bool) -> u64 {
    assert_eq!(core_compile(0x1000, 8, 128, 65536), 0);
    let owner = BRIDGE.with_borrow(|bridge| {
        let info: serde_json::Value = serde_json::from_slice(&bridge.info).unwrap();
        u64::from_str_radix(info["owner"].as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    });
    if publish {
        assert_eq!(mapped::core_publish(owner), 1);
    }
    owner
}

fn fixture(epoch: u64) -> (u64, u64) {
    let mut bytes = vec![0; 0xc000];
    for (address, word) in [
        (0x1000, 0x2027u64),
        (0x2000, 0x3027),
        (0x3000, 0x4027),
        (0x4008, 0x9027),
        (0x4010, 0xa067),
    ] {
        bytes[address..address + 8].copy_from_slice(&word.to_le_bytes());
    }
    bytes[0x9000..0x9006].copy_from_slice(&[0xb8, 1, 0, 0, 0, 0xf4]);
    bytes[0xa000] = 0xa1;
    bytes[0xb000] = 0xb2;
    let config = serde_json::json!({
        "memoryModel":"ia32e-paged",
        "paging":{"cr0":"0x80000033","cr2":"0xdead","cr3":"0x1000","cr4":"0x628","efer":"0x500"},
        "memoryLimits":{"maxBackingBytes":49152,"maxBackings":1,"maxMappings":1},
        "backings":[{"id":"ram","length":49152}],
        "mappings":[{"base":"0x0","backing":"ram","offset":0,"length":49152,"permissions":{"read":true,"write":true,"execute":true}}]
    });
    let length = mapped_input(config, &bytes);
    assert_eq!(mapped::core_init_mapped(0x1000, length), 0);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.context_version = epoch;
        core.paged_decode(0x1000).unwrap();
        assert_eq!(
            crate::x86::paging::access::Memory::new(
                &mut core.image,
                core.paging.as_ref().unwrap(),
                0,
                2,
                &mut core.paging_work
            )
            .with_cache(&core.translation_cache)
            .read(0x2000, 1)
            .unwrap(),
            0xa1
        );
        // Guest mutation preserves the retained A view and does not touch code dependencies.
        core.image.physical_write(0x4010, 8, 0xb067).unwrap();
    });
    (compile_owner(true), compile_owner(false))
}

fn retained_data_physical() -> Option<u64> {
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        core.translation_cache
            .observe(
                core.paging.as_ref().unwrap(),
                Request {
                    linear: 0x2000,
                    access: Access::Read,
                    class: AccessClass::Supervisor { ac: false },
                },
            )
            .unwrap()
            .map(|t| t.physical)
    })
}

#[test]
fn unrelated_host_payload_write_revokes_tlb_and_pending_proofs_but_keeps_installed_code() {
    for epoch in [0, u64::MAX] {
        let (installed, pending) = fixture(epoch);
        assert_eq!(retained_data_physical(), Some(0xa000));
        BRIDGE.with_borrow_mut(|bridge| bridge.input = vec![0x99]);
        assert_eq!(core_write_memory(0xa010), 0);
        assert_eq!(retained_data_physical(), None);
        assert_eq!(mapped::core_validity(installed), 1);
        assert_eq!(mapped::core_validity(pending), 0);
        assert_eq!(mapped::core_publish(pending), 0);
        assert_eq!(mapped::core_activate(installed), 1);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            assert_eq!(
                crate::x86::paging::access::Memory::new(
                    &mut core.image,
                    core.paging.as_ref().unwrap(),
                    0,
                    2,
                    &mut core.paging_work
                )
                .with_cache(&core.translation_cache)
                .read(0x2000, 1)
                .unwrap(),
                0xb2
            );
            assert_eq!(core.image.physical_read(0xa010, 1).unwrap(), 0x99);
        });
    }
}

#[test]
fn rejected_host_write_preserves_cache_and_compiled_proofs() {
    let (installed, pending) = fixture(u64::MAX);
    BRIDGE.with_borrow_mut(|bridge| bridge.input = vec![0x99]);
    assert_eq!(core_write_memory(0x10000), 1);
    assert_eq!(retained_data_physical(), Some(0xa000));
    assert_eq!(mapped::core_validity(installed), 1);
    assert_eq!(mapped::core_validity(pending), 1);
    assert_eq!(mapped::core_publish(pending), 1);
}

#[test]
fn host_code_alias_write_still_retires_the_affected_installed_owner() {
    let (installed, _) = fixture(0);
    BRIDGE.with_borrow_mut(|bridge| bridge.input = vec![2]);
    assert_eq!(core_write_memory(0x9001), 0);
    assert_eq!(retained_data_physical(), None);
    assert_eq!(mapped::core_validity(installed), 0);
    let fresh = compile_owner(true);
    assert_ne!(fresh, installed);
    assert_eq!(mapped::core_activate(fresh), 1);
}
