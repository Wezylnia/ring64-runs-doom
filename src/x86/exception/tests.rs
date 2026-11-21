use super::*;
use crate::x86::context::ExecutionContext;
use crate::x86::flags;

#[test]
fn alignment_exception_checkpoint_accepts_only_its_zero_or_ext_error_slot() {
    let context = ExecutionContext::default();
    let source = ExceptionEvent::fault(17, 0x1000, 0x40002, &context, Some(0));
    for code in [None, Some(0), Some(1), Some(2), Some(4), Some(0x8000)] {
        let event = ExceptionEvent {
            error_code: code,
            ..source
        };
        assert_eq!(
            event.validate(0x1000, 0x40002, &context).is_ok(),
            matches!(code, Some(0 | 1)),
            "{code:?}"
        );
    }
    assert!(
        ExceptionEvent {
            page_fault_address: Some(0x6001),
            ..source
        }
        .validate(0x1000, 0x40002, &context)
        .is_err()
    );
    assert!(
        ExceptionEvent {
            debug: Some(super::super::debug::Source::Execute(1)),
            ..source
        }
        .validate(0x1000, 0x40002, &context)
        .is_err()
    );
    assert_eq!(source.saved_flags, 0x50002);
}

#[test]
fn secondary_alignment_is_benign_after_page_or_contributory_faults() {
    let context = ExecutionContext::default();
    for first in [0, 1, 6, 10, 11, 12, 13, 14, 17, 8] {
        let event = ExceptionEvent {
            page_fault_address: (first == 14).then_some(0x7000),
            ..ExceptionEvent::fault(
                first,
                0x1000,
                0x40002,
                &context,
                matches!(first, 8 | 10..=14 | 17).then_some(0),
            )
        };
        let next = event.escalate_alignment_check();
        if first == 8 {
            assert_eq!(next, EventState::Shutdown);
            continue;
        }
        let EventState::Pending(next) = next else {
            panic!("{first}")
        };
        assert_eq!(
            (
                next.vector,
                next.error_code,
                next.page_fault_address,
                next.numeric,
                next.debug
            ),
            (17, Some(1), None, None, None),
            "{first}"
        );
        assert_eq!(
            (next.instruction_pc, next.return_pc, next.saved_flags),
            (0x1000, 0x1000, 0x50002)
        );
        assert_eq!(next.validate(0x1000, 0x40002, &context), Ok(()));
    }
    let alignment = ExceptionEvent::fault(17, 0x1000, 0x40002, &context, Some(0));
    for second in 10..=14 {
        let EventState::Pending(next) =
            alignment.secondary(second, 0, (second == 14).then_some(0x7000))
        else {
            panic!("{second}")
        };
        assert_eq!(next.vector, second);
    }
    let interrupt = InterruptEvent {
        source: InterruptSource::Nmi,
        return_pc: 0x1000,
        saved_flags: 0x40002,
    };
    let EventState::Pending(next) = interrupt.escalate_alignment_check() else {
        panic!()
    };
    assert_eq!(
        (next.vector, next.error_code, next.saved_flags),
        (17, Some(1), 0x50002)
    );
}

#[test]
fn page_fault_checkpoint_requires_its_frozen_canonical_address_and_exact_error_class() {
    let context = ExecutionContext::default();
    let event = ExceptionEvent::page_fault(0x1000, 0x202, &context, 6, 0x501000);
    assert_eq!(event.page_fault_address, Some(0x501000));
    assert_eq!(event.saved_flags, 0x10202);
    let valid_codes = [
        0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 15, 16, 17, 20, 21, 25, 29, 0x21, 0x23, 0x25, 0x27,
    ];
    for code in 0..=127 {
        let changed = ExceptionEvent {
            error_code: Some(code),
            ..event
        };
        assert_eq!(
            changed.validate(0x1000, 0x202, &context).is_ok(),
            valid_codes.contains(&code),
            "code{code}"
        );
    }
    for address in [0, 0x501000, 0xffff_8000_0000_0000, u64::MAX] {
        assert!(
            ExceptionEvent {
                page_fault_address: Some(address),
                ..event
            }
            .validate(0x1000, 0x202, &context)
            .is_ok()
        );
    }
    for address in [
        None,
        Some(0x0000_8000_0000_0000),
        Some(0x000f_ffff_ffff_ffff),
    ] {
        assert!(
            ExceptionEvent {
                page_fault_address: address,
                ..event
            }
            .validate(0x1000, 0x202, &context)
            .is_err()
        );
    }
    assert!(
        ExceptionEvent {
            error_code: None,
            ..event
        }
        .validate(0x1000, 0x202, &context)
        .is_err()
    );
    for vector in [0, 1, 6, 8, 10, 11, 12, 13, 16, 19] {
        let changed = ExceptionEvent {
            vector,
            error_code: matches!(vector, 8 | 10..=13).then_some(0),
            ..event
        };
        assert!(
            changed.validate(0x1000, 0x202, &context).is_err(),
            "vector{vector}"
        );
    }
    let historical = ExceptionEvent {
        instruction_pc: 0xff0,
        ..event
    };
    assert!(historical.validate(0x1000, 0x202, &context).is_ok());
}

#[test]
fn page_fault_escalation_is_asymmetric_and_replaces_obsolete_source_metadata() {
    let context = ExecutionContext::default();
    for original in [1, 6, 0, 10, 11, 12, 13, 14, 8] {
        let error_code = matches!(original, 8 | 10..=14).then_some(0);
        let mut event = ExceptionEvent::fault(original, 0x1000, 0x202, &context, error_code);
        event.page_fault_address = (original == 14).then_some(0x500000);
        event.return_pc = 0x1010;
        assert!(event.validate(0x1010, 0x202, &context).is_ok());
        let result = event.escalate_page_fault(2, 0x600000);
        if original == 8 {
            assert_eq!(result, EventState::Shutdown);
            continue;
        }
        let EventState::Pending(next) = result else {
            panic!("{original}")
        };
        assert_eq!(
            (next.vector, next.error_code, next.page_fault_address),
            if original == 14 {
                (8, Some(0), None)
            } else {
                (14, Some(2), Some(0x600000))
            }
        );
        assert_eq!(
            (
                next.instruction_pc,
                next.return_pc,
                next.saved_flags,
                next.debug
            ),
            (0x1000, 0x1010, 0x10202, None)
        );
        assert!(next.validate(0x1010, 0x202, &context).is_ok());
    }
    let pf = ExceptionEvent::page_fault(0x1000, 0x202, &context, 6, 0x501000);
    for vector in 10..=13 {
        let EventState::Pending(df) = pf.escalate(vector, 0x2b) else {
            panic!("{vector}")
        };
        assert_eq!(
            (df.vector, df.error_code, df.page_fault_address),
            (8, Some(0), None)
        );
    }
    let debug = ExceptionEvent {
        debug: Some(super::super::debug::Source::Execute(1)),
        saved_flags: 2,
        ..ExceptionEvent::fault(1, 0x1000, 2, &context, None)
    };
    assert!(debug.validate(0x1000, 2, &context).is_ok());
    let EventState::Pending(next) = debug.escalate_page_fault(0, 0x700000) else {
        panic!()
    };
    assert_eq!(
        (next.vector, next.debug, next.page_fault_address),
        (14, None, Some(0x700000))
    );
    for source in [InterruptSource::Nmi, InterruptSource::Irq(14)] {
        let event = InterruptEvent {
            source,
            return_pc: 0x2000,
            saved_flags: 0x202,
        };
        let EventState::Pending(next) = event.escalate_page_fault(0, 0x800000) else {
            panic!()
        };
        assert_eq!(
            (next.vector, next.page_fault_address, next.saved_flags),
            (14, Some(0x800000), 0x10202)
        );
        assert!(next.validate(0x2000, 0x202, &context).is_ok());
    }
}

#[test]
fn exception_checkpoint_validates_boundary_flags_vm_and_error_code_together() {
    let context = ExecutionContext::default();
    let event = ExceptionEvent::fault(13, 0x1000, 0x202, &context, Some(0));
    assert_eq!(event.saved_flags, 0x10202);
    assert!(event.validate(0x1000, 0x202, &context).is_ok());
    for changed in [
        ExceptionEvent {
            debug: None,
            vector: 14,
            ..event
        },
        ExceptionEvent {
            debug: None,
            return_pc: 0x1001,
            ..event
        },
        ExceptionEvent {
            debug: None,
            saved_flags: 0x202,
            ..event
        },
        ExceptionEvent {
            debug: None,
            saved_flags: 0x10203,
            ..event
        },
        ExceptionEvent {
            debug: None,
            saved_flags: 0x30202,
            ..event
        },
        ExceptionEvent {
            debug: None,
            saved_flags: 0x10200,
            ..event
        },
        ExceptionEvent {
            debug: None,
            saved_flags: 1 << 63 | 0x10202,
            ..event
        },
        ExceptionEvent {
            debug: None,
            error_code: None,
            ..event
        },
    ] {
        assert!(
            changed.validate(0x1000, 0x202, &context).is_err(),
            "{changed:?}"
        );
    }
    let mut virtual_context = context.to_json();
    virtual_context["mode"] = serde_json::json!("virtual8086");
    virtual_context["cpl"] = serde_json::json!(3);
    for name in ["cs", "ss", "ds", "es", "fs", "gs"] {
        virtual_context["segments"][name]["selector"] = serde_json::json!(0);
        virtual_context["segments"][name]["base"] = serde_json::json!("0x0");
        virtual_context["segments"][name]["limit"] = serde_json::json!(0xffff);
        virtual_context["segments"][name]["type"] = serde_json::json!("read-write");
        virtual_context["segments"][name]["default32"] = serde_json::json!(false);
    }
    let virtual_context = ExecutionContext::from_json(&virtual_context).unwrap();
    let vm = ExceptionEvent::fault(0, 0x1000, 2, &virtual_context, None);
    assert_eq!(vm.saved_flags, 0x30002);
    assert!(vm.validate(0x1000, 2, &virtual_context).is_ok());
    assert!(vm.validate(0x1000, 2, &context).is_err());
}

#[test]
fn selected_vectors_have_one_error_code_and_fault_or_trap_authority() {
    let context = ExecutionContext::default();
    for vector in [0, 1, 6, 8, 10, 11, 12, 13, 16, 19] {
        let error_code = matches!(vector, 8 | 10..=13).then_some(0);
        let event = ExceptionEvent::fault(vector, 0x1000, flags::TF | 2, &context, error_code);
        assert!(event.validate(0x1000, flags::TF | 2, &context).is_ok());
        assert!(
            ExceptionEvent {
                debug: None,
                error_code: if error_code.is_some() { None } else { Some(0) },
                ..event
            }
            .validate(0x1000, flags::TF | 2, &context)
            .is_err()
        );
        let without_rf = ExceptionEvent {
            debug: None,
            saved_flags: flags::TF | 2,
            ..event
        };
        assert_eq!(
            without_rf.validate(0x1000, flags::TF | 2, &context).is_ok(),
            vector == 1
        );
    }
    let df = ExceptionEvent::fault(8, 0x1000, 2, &context, Some(1));
    assert!(df.validate(0x1000, 2, &context).is_err());
    assert!(
        EventState::Ready
            .validate(0x1000, 2, &context, 0, InterruptState::default())
            .is_ok()
    );
    assert!(
        EventState::Shutdown
            .validate(0x1000, 2, &context, 0, InterruptState::default())
            .is_ok()
    );
}

#[test]
fn escalation_distinguishes_benign_contributory_and_double_fault_entry() {
    let context = ExecutionContext::default();
    for vector in [1, 6, 16, 19, 0, 10, 11, 12, 13, 8] {
        let event = ExceptionEvent {
            numeric: None,
            page_fault_address: None,
            debug: None,
            vector,
            instruction_pc: 0x1000,
            return_pc: 0x1010,
            saved_flags: 0x302,
            error_code: None,
        };
        for secondary in [10, 11, 12, 13] {
            let result = event.escalate(secondary, 0x2b);
            if vector == 8 {
                assert_eq!(result, EventState::Shutdown);
                continue;
            }
            let EventState::Pending(next) = result else {
                panic!("{vector}")
            };
            assert_eq!(
                next.vector,
                if matches!(vector, 0 | 10..=13) {
                    8
                } else {
                    secondary
                }
            );
            assert_eq!(
                next.error_code,
                Some(if next.vector == 8 { 0 } else { 0x2b })
            );
            assert_eq!(
                (next.instruction_pc, next.return_pc, next.saved_flags),
                (0x1000, 0x1010, 0x10302)
            );
            assert!(next.validate(0x1010, 0x302, &context).is_ok());
        }
    }
}

#[test]
fn entry_escalation_uses_the_faulting_tasks_register_image() {
    let context = ExecutionContext::default();
    let first =
        |vector| EventState::Pending(ExceptionEvent::fault(vector, 0xa000, 2, &context, Some(0)));
    let ts = ExceptionEvent::fault(10, 0xb000, 0x4202, &context, Some(0x38));
    let pf = ExceptionEvent::page_fault(0xb000, 0x4202, &context, 0, 0xb123);
    assert_eq!(entry_secondary(first(6), pf), EventState::Pending(pf));
    let df = ExceptionEvent {
        vector: 8,
        error_code: Some(0),
        ..ts
    };
    assert_eq!(entry_secondary(first(13), ts), EventState::Pending(df));
    assert_eq!(entry_secondary(first(14), pf), EventState::Pending(df));
    assert_eq!(entry_secondary(first(8), ts), EventState::Shutdown);
}

#[test]
fn external_selection_uses_source_kind_and_captures_before_instruction_flags() {
    let context = ExecutionContext::default();
    let mut interrupts = InterruptState {
        irq: Some(2),
        nmi_pending: true,
        nmi_blocked: false,
    };
    assert!(interrupts.accept(0x1000, 0x10202, &context, 2).is_none());
    let nmi = interrupts.accept(0x1000, 0x10202, &context, 1).unwrap();
    assert_eq!(nmi.source, InterruptSource::Nmi);
    assert_eq!(nmi.saved_flags, 0x10202);
    assert_eq!(nmi.return_pc, 0x1000);
    assert_eq!(interrupts.irq, Some(2));
    assert!(interrupts.nmi_blocked);
    assert!(!interrupts.nmi_pending);
    assert!(interrupts.accept(0x1000, 2, &context, 0).is_none());
    let irq = interrupts.accept(0x1000, 0x202, &context, 0).unwrap();
    assert_eq!(irq.source, InterruptSource::Irq(2));
    assert_eq!(irq.saved_flags, 0x202);
    assert!(interrupts.nmi_blocked);
    assert_eq!(interrupts.irq, None);
}

#[test]
fn external_checkpoint_requires_captured_eligibility_and_exact_rf() {
    let context = ExecutionContext::default();
    let event = InterruptEvent {
        source: InterruptSource::Irq(13),
        return_pc: 0x1000,
        saved_flags: 0x10202,
    };
    assert!(event.validate(0x1000, 0x10202, &context).is_ok());
    assert!(event.validate(0x1000, 0x202, &context).is_err());
    let state = EventState::PendingInterrupt(event);
    assert!(
        state
            .validate(0x1000, 0x10202, &context, 0, InterruptState::default())
            .is_ok()
    );
    for shadow in [1, 2] {
        assert!(
            state
                .validate(0x1000, 0x10202, &context, shadow, InterruptState::default())
                .is_err()
        );
    }
    let nmi = EventState::PendingInterrupt(InterruptEvent {
        source: InterruptSource::Nmi,
        return_pc: 0x1000,
        saved_flags: 2,
    });
    assert!(
        nmi.validate(0x1000, 2, &context, 0, InterruptState::default())
            .is_err()
    );
    let blocked = InterruptState {
        nmi_blocked: true,
        ..InterruptState::default()
    };
    assert!(nmi.validate(0x1000, 2, &context, 1, blocked).is_ok());
    assert!(nmi.validate(0x1000, 2, &context, 2, blocked).is_err());
    assert!(
        EventState::Halted
            .validate(0x1000, 2, &context, 1, blocked)
            .is_err()
    );
}

#[test]
fn external_entry_failure_is_benign_even_for_contributory_or_df_vectors() {
    for source in [
        InterruptSource::Irq(0),
        InterruptSource::Irq(8),
        InterruptSource::Irq(13),
        InterruptSource::Nmi,
    ] {
        let event = InterruptEvent {
            source,
            return_pc: 0x1234,
            saved_flags: 0x202,
        };
        assert_eq!(
            event.escalate(11, 0x6b),
            EventState::Pending(ExceptionEvent {
                numeric: None,
                page_fault_address: None,
                debug: None,
                vector: 11,
                instruction_pc: 0x1234,
                return_pc: 0x1234,
                saved_flags: 0x10202,
                error_code: Some(0x6b),
            })
        );
    }
}

#[test]
fn halt_activity_requires_privilege_zero_in_every_non_real_mode() {
    use crate::x86::protection::{ProtectionFault, ProtectionPhase};
    for mode in [
        "real",
        "protected",
        "compatibility",
        "long64",
        "virtual8086",
    ] {
        for cpl in 0..=3 {
            if (mode == "real" && cpl != 0) || (mode == "virtual8086" && cpl != 3) {
                continue;
            }
            let mut value = ExecutionContext::default().to_json();
            value["mode"] = serde_json::json!(mode);
            value["cpl"] = serde_json::json!(cpl);
            if mode == "virtual8086" {
                for name in ["cs", "ss", "ds", "es", "fs", "gs"] {
                    value["segments"][name]["selector"] = serde_json::json!(0);
                    value["segments"][name]["base"] = serde_json::json!("0x0");
                    value["segments"][name]["limit"] = serde_json::json!(0xffff);
                    value["segments"][name]["type"] = serde_json::json!("read-write");
                    value["segments"][name]["default32"] = serde_json::json!(false);
                }
            }
            let context = ExecutionContext::from_json(&value).unwrap();
            assert_eq!(
                EventState::halt(&context),
                if mode == "real" || cpl == 0 {
                    Ok(EventState::Halted)
                } else {
                    Err(ProtectionFault {
                        vector: 13,
                        error_code: 0,
                        phase: ProtectionPhase::Privilege,
                        selector: None,
                    })
                },
                "{mode} cpl{cpl}"
            );
        }
    }
}
