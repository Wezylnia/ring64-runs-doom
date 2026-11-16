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
fn each_operand_admission_clears_a_previous_alignment_refusal() {
    for operation in 0..6 {
        fixture();
        assert_eq!(core_load_impl::<false, true>(0x4001, 4), 0);
        assert_eq!(
            paging::core_paged_access_reason(),
            super::super::super::layout::ALIGNMENT_CHECK
        );
        assert_eq!(paging::core_paged_access_failed(), 1);
        match operation {
            0 => {
                assert_eq!(core_load_impl::<false, true>(0x4000, 4), 0x90909090);
            }
            1 => {
                assert_eq!(
                    core_load_for_write_impl::<false, true>(0x4000, 4),
                    0x90909090
                );
            }
            2 => {
                assert_eq!(core_store_impl::<false, true>(0x4000, 4, 0x12345678), 1);
            }
            3 => {
                assert_eq!(
                    context::core_paged_segmented_load(0x4000, 4, Segment::Ds as u32),
                    0x90909090
                );
            }
            4 => {
                assert_eq!(
                    context::core_paged_segmented_store(0x4000, 4, 0x12345678, Segment::Ds as u32),
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
