use super::*;
use crate::memory::{MappingInput, MemoryLimits, Permissions};

fn fixture() {
    initialize(0x1000, &[0x90]);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let mut json = ExecutionContext::default().to_json();
        json["mode"] = serde_json::json!("protected");
        json["cpl"] = serde_json::json!(3);
        for name in ["cs", "ss", "ds", "es", "fs", "gs"] {
            json["segments"][name]["dpl"] = serde_json::json!(3);
            let selector = json["segments"][name]["selector"].as_u64().unwrap();
            json["segments"][name]["selector"] = serde_json::json!(selector | 3);
        }
        json["segments"]["cs"]["long"] = serde_json::json!(false);
        json["segments"]["cs"]["default32"] = serde_json::json!(true);
        json["segments"]["ss"]["default32"] = serde_json::json!(true);
        core.context = ExecutionContext::from_json_profile(&json, true).unwrap();
        core.paging = Some(crate::x86::paging::State {
            cr0: 0x40011,
            ..crate::x86::paging::State::reset()
        });
        core.physical_controls = true;
        core.image = Image::physical_mapped(
            vec![("ram".into(), vec![0x90; 0x5000])],
            vec![MappingInput {
                base: 0,
                backing: "ram".into(),
                offset: 0,
                length: 0x5000,
                permissions: Permissions {
                    read: true,
                    write: true,
                    execute: true,
                },
            }],
            MemoryLimits {
                max_backing_bytes: 0x5000,
                max_backings: 1,
                max_mappings: 1,
            },
        )
        .unwrap();
        unsafe {
            (*core.state.get()).flags = 0x40002;
        }
    });
}

#[test]
fn paging_import_clears_transient_alignment_only_after_validation_and_keeps_pending_source() {
    fixture();
    assert_eq!(core_paged_load(0x4001, 4), 0);
    let (failure, bank, event) = BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let state = unsafe { &mut *core.state.get() };
        state.rip = 0x1000;
        let event = crate::x86::exception::EventState::Pending(
            crate::x86::exception::ExceptionEvent::fault(
                17,
                0x1000,
                state.flags,
                &core.context,
                Some(1),
            ),
        );
        state.set_event(event);
        (core.alignment_check, core.paging, event)
    });
    let input = |cr0| {
        BRIDGE.with_borrow_mut(|bridge| {
            bridge.input = serde_json::to_vec(
                &serde_json::json!({"cr0":cr0,"cr2":"0xdead","cr3":"0x0","cr4":"0x0","efer":"0x0"}),
            )
            .unwrap();
        })
    };
    input("0x80040010");
    assert_eq!(paging::core_import_paging(), 1);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert_eq!(core.alignment_check, failure);
        assert_eq!(core.paging, bank);
        assert_eq!(unsafe { (*core.state.get()).event_state() }, Ok(event));
    });
    input("0x11");
    assert_eq!(paging::core_import_paging(), 0);
    assert_eq!(paging::core_paged_access_failed(), 0);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert!(core.alignment_check.is_none());
        assert_eq!(unsafe { (*core.state.get()).event_state() }, Ok(event));
    });
}

#[test]
fn each_operand_admission_clears_a_previous_alignment_refusal() {
    for operation in 0..6 {
        fixture();
        assert_eq!(core_paged_load(0x4001, 4), 0);
        assert_eq!(
            paging::core_paged_access_reason(),
            super::super::super::layout::ALIGNMENT_CHECK
        );
        assert_eq!(paging::core_paged_access_failed(), 1);
        match operation {
            0 => {
                assert_eq!(core_paged_load(0x4000, 4), 0x90909090);
            }
            1 => {
                assert_eq!(core_paged_load_for_write(0x4000, 4), 0x90909090);
            }
            2 => {
                assert_eq!(core_paged_store(0x4000, 4, 0x12345678), 1);
            }
            3 => {
                assert_eq!(
                    scalar::core_paged_segmented_load(0x4000, 4, Segment::Ds as u32),
                    0x90909090
                );
            }
            4 => {
                assert_eq!(
                    scalar::core_paged_segmented_store(0x4000, 4, 0x12345678, Segment::Ds as u32),
                    1
                );
            }
            5 => {
                assert_eq!(paging::core_paged_debug_begin(0x1000, 0x40002, 0), 0);
            }
            _ => unreachable!(),
        }
        assert_eq!(
            paging::core_paged_access_failed(),
            0,
            "operation {operation}"
        );
        assert!(
            BRIDGE.with_borrow(|bridge| bridge.core.as_ref().unwrap().alignment_check.is_none())
        );
    }
}

#[test]
fn alignment_report_bank_switches_classes_without_resurrecting_an_older_fault() {
    use super::super::super::layout::{ALIGNMENT, ALIGNMENT_CHECK, DATA};

    fixture();
    let bank = BRIDGE.with_borrow(|bridge| bridge.core.as_ref().unwrap().paging);
    let mandatory = || {
        assert_eq!(paging::core_paged_access_reason(), ALIGNMENT);
        BRIDGE.with_borrow(|bridge| {
            let core = bridge.core.as_ref().unwrap();
            assert!(core.alignment_check.is_none());
            assert_eq!(
                core.alignment_fault.as_ref().map(|fault| (
                    fault.segment,
                    fault.offset,
                    fault.address,
                    fault.width,
                    fault.access,
                    fault.alignment,
                )),
                Some((Segment::Ds, 0x4001, 0x4001, 16, Access::Read, 16))
            );
        });
    };
    let natural = || {
        assert_eq!(paging::core_paged_access_reason(), ALIGNMENT_CHECK);
        assert_eq!(paging::core_paged_access_failed(), 1);
        BRIDGE.with_borrow(|bridge| {
            let core = bridge.core.as_ref().unwrap();
            assert!(core.alignment_fault.is_none());
            assert_eq!(
                core.alignment_check,
                Some(crate::x86::alignment::Fault {
                    address: 0x4001,
                    width: 4,
                    access: Access::Read,
                    alignment: 4,
                })
            );
        });
    };

    assert_eq!(
        vector::core_paged_vector_load(0x4001, 16, Segment::Ds as u32, 16),
        0
    );
    mandatory();
    // A scalar admission clears only natural AC, preserving the captured GP.
    assert_eq!(core_paged_load(0x4000, 4), 0x90909090);
    assert_eq!(paging::core_paged_access_failed(), 0);
    mandatory();

    assert_eq!(core_paged_load(0x4001, 4), 0);
    natural();
    assert_eq!(core_paged_load(0x4000, 4), 0x90909090);
    assert_eq!(paging::core_paged_access_failed(), 0);
    assert_eq!(paging::core_paged_access_reason(), DATA);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert!(core.alignment_check.is_none());
        assert!(
            core.alignment_fault.is_none(),
            "older GP cannot reappear after AC repair"
        );
    });

    assert_eq!(core_paged_load(0x4001, 4), 0);
    natural();
    assert_eq!(
        vector::core_paged_vector_load(0x4001, 16, Segment::Ds as u32, 16),
        0
    );
    mandatory();
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert_eq!(
            core.paging, bank,
            "neither alignment class changes CR2 or controls"
        );
        assert_eq!(
            core.image.physical_read_bytes(0, 0x5000),
            Ok(vec![0x90; 0x5000])
        );
    });
}
