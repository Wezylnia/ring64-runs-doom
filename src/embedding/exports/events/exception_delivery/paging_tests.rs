use super::*;
use crate::embedding::layout::{PAGE_FAULT, PHYSICAL_MEMORY};
use crate::x86::paging::walk::{Failure, PageFault, PhysicalFault, PhysicalPhase};

fn initialize() {
    BRIDGE.with_borrow_mut(|bridge| {
        *bridge = Bridge::default();
        bridge.input = vec![0x90];
        bridge.input.extend_from_slice(&[0xa5; 16]);
    });
    assert_eq!(core_init(0x1000, 1, 0x4000, 16), 0);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.paging = Some(crate::x86::paging::State::default());
        let state = unsafe { &mut *core.state.get() };
        state.flags = 0x202;
        state.interruptibility = 2;
        state.debug.continuation = crate::embedding::layout::debug::Continuation {
            slots: 1,
            reserved: 0,
            origin: 0xff0,
        };
        core.debug_candidate = 2;
    });
}

#[test]
fn alignment_staging_uses_captured_fault_even_after_eligibility_is_cleared() {
    for enabled in [0, 1] {
        initialize();
        assert_eq!(core_configure_delivery(enabled), 0);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            core.paging.as_mut().unwrap().cr2 = 0xdead;
            core.alignment_check = Some(crate::x86::alignment::Fault {
                address: 0x6001,
                width: 4,
                access: Access::Read,
                alignment: 4,
            });
        });
        assert_eq!(core_stage_exception(ALIGNMENT_CHECK), enabled);
        BRIDGE.with_borrow(|bridge| {
            let core = bridge.core.as_ref().unwrap();
            let state = unsafe { &*core.state.get() };
            assert_eq!(core.paging.unwrap().cr2, 0xdead);
            assert_eq!(core.debug_candidate, 0);
            assert_eq!(
                state.debug.continuation,
                crate::embedding::layout::debug::Continuation::default()
            );
            if enabled != 0 {
                let EventState::Pending(event) = state.event_state().unwrap() else {
                    panic!()
                };
                assert_eq!(
                    (
                        event.vector,
                        event.error_code,
                        event.saved_flags,
                        event.page_fault_address
                    ),
                    (17, Some(0), 0x10202, None)
                );
            } else {
                assert_eq!(state.event_state(), Ok(EventState::Ready));
            }
        });
    }
    initialize();
    assert_eq!(core_stage_exception(ALIGNMENT_CHECK), -1);
}

#[test]
fn external_frame_alignment_refusal_stages_ext_one_and_repairs_without_reacceptance() {
    use crate::x86::exception::{InterruptEvent, InterruptSource};
    initialize();
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let mut bytes = vec![0xa5; 0x10000];
        bytes[0x1008..0x1010].copy_from_slice(&0x00cf_fb00_0000_ffffu64.to_le_bytes());
        for vector in [2usize, 17] {
            let offset = 0x2000 + vector * 8;
            bytes[offset..offset + 8].copy_from_slice(&0x0000_ee00_000b_7000u64.to_le_bytes());
        }
        core.image = Image::physical_mapped(
            vec![("bytes".into(), bytes)],
            vec![crate::memory::MappingInput {
                base: 0,
                backing: "bytes".into(),
                offset: 0,
                length: 0x10000,
                permissions: crate::memory::Permissions {
                    read: true,
                    write: true,
                    execute: true,
                },
            }],
            crate::memory::MemoryLimits {
                max_backing_bytes: 0x10000,
                max_backings: 1,
                max_mappings: 1,
            },
        )
        .unwrap();
        core.paging = Some(crate::x86::paging::State {
            cr0: 0x40011,
            cr2: 0xdead,
            ..crate::x86::paging::State::reset()
        });
        core.context
            .set_mode_and_cpl(crate::x86::context::Mode::Protected, 3);
        core.context.set_gdtr(crate::x86::context::TableRegister {
            base: 0x1000,
            limit: 15,
        });
        core.context.set_idtr(crate::x86::context::TableRegister {
            base: 0x2000,
            limit: 0xfff,
        });
        let state = unsafe { &mut *core.state.get() };
        state.flags = 0x40002;
        state.rip = 0x1000;
        state.registers[4] = 0x8001;
        state.interruptibility = 0;
        state.interrupts.nmi_blocked = 1;
        state.set_event(EventState::PendingInterrupt(InterruptEvent {
            source: InterruptSource::Nmi,
            return_pc: 0x1000,
            saved_flags: 0x40002,
        }));
    });
    assert_eq!(core_deliver_exception(), DISPATCH);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let state = unsafe { &mut *core.state.get() };
        let EventState::Pending(event) = state.event_state().unwrap() else {
            panic!()
        };
        assert_eq!(
            (
                event.vector,
                event.error_code,
                event.saved_flags,
                event.page_fault_address
            ),
            (17, Some(1), 0x50002, None)
        );
        assert_eq!(
            core.alignment_check,
            Some(crate::x86::alignment::Fault {
                address: 0x7ffd,
                width: 4,
                access: Access::Write,
                alignment: 4
            })
        );
        assert_eq!(core.paging.unwrap().cr2, 0xdead);
        assert_eq!(
            (state.rip, state.registers[4], state.interrupts.nmi_blocked),
            (0x1000, 0x8001, 1)
        );
        assert_eq!(
            core.image.physical_read_bytes(0x7fe0, 0x30).unwrap(),
            vec![0xa5; 0x30]
        );
        state.registers[4] = 0x8000;
    });
    assert_eq!(core_deliver_exception(), DISPATCH);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        let state = unsafe { &*core.state.get() };
        assert_eq!(
            (state.rip, state.registers[4], state.interrupts.nmi_blocked),
            (0x7000, 0x7ff0, 1)
        );
        assert_eq!(state.event_state(), Ok(EventState::Ready));
        assert!(core.alignment_check.is_none());
        assert_eq!(
            core.image.physical_read_bytes(0x7ff0, 16).unwrap(),
            vec![1, 0, 0, 0, 0, 0x10, 0, 0, 8, 0, 0, 0, 2, 0, 5, 0]
        );
    });
}

#[test]
fn page_fault_staging_uses_frozen_detection_not_repaired_cr2_or_privilege() {
    for enabled in [0, 1] {
        initialize();
        assert_eq!(core_configure_delivery(enabled), 0);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            assert_eq!(
                core.paging_failure(Failure::Page(PageFault {
                    address: 0x501000,
                    access: Access::Write,
                    error_code: 6
                })),
                PAGE_FAULT
            );
            assert_eq!(core.paging.unwrap().cr2, 0x501000);
            core.paging.as_mut().unwrap().cr2 = 0xdead_beef;
            core.paging.as_mut().unwrap().efer = 0xd00;
        });
        assert_eq!(core_stage_exception(PAGE_FAULT), enabled);
        BRIDGE.with_borrow(|bridge| {
            let core = bridge.core.as_ref().unwrap();
            let state = unsafe { &*core.state.get() };
            assert_eq!(core.paging.unwrap().cr2, 0xdead_beef);
            assert_eq!(
                state.debug.continuation,
                crate::embedding::layout::debug::Continuation::default()
            );
            assert_eq!(core.debug_candidate, 0);
            if enabled == 0 {
                assert_eq!(state.event_state(), Ok(EventState::Ready));
            } else {
                let EventState::Pending(event) = state.event_state().unwrap() else {
                    panic!()
                };
                assert_eq!(
                    (
                        event.vector,
                        event.error_code,
                        event.page_fault_address,
                        event.saved_flags
                    ),
                    (14, Some(6), Some(0x501000), 0x10202)
                );
                assert!(event.validate(0x1000, 0x202, &core.context).is_ok());
            }
        });
    }
}

#[test]
fn physical_refusal_cannot_stage_pf_or_cancel_deferred_debug() {
    initialize();
    assert_eq!(core_configure_delivery(1), 0);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        assert_eq!(
            core.paging_failure(Failure::Physical(PhysicalFault {
                linear: 0x500000,
                access: Access::Read,
                physical: Fault {
                    address: 0x9000,
                    width: 8,
                    access: Access::Write,
                    cause: Cause::Permission,
                    failing: 0x9000
                },
                phase: PhysicalPhase::AccessedWrite
            })),
            PHYSICAL_MEMORY
        );
    });
    assert_eq!(core_stage_exception(PAGE_FAULT), -1);
    assert_eq!(core_stage_exception(PHYSICAL_MEMORY), 0);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        let state = unsafe { &*core.state.get() };
        assert_eq!(core.paging.unwrap().cr2, 0);
        assert_eq!(state.event_state(), Ok(EventState::Ready));
        assert_eq!(state.debug.continuation.slots, 1);
        assert_eq!(core.debug_candidate, 2);
    });
}

fn entry_fixture(vector: u8, frame_present: bool, frame_backed: bool) -> EventState {
    initialize();
    let mut bytes = vec![0xa5; 0xa000];
    for page in [0x1000, 0x2000, 0x3000, 0x4000] {
        bytes[page..page + 0x1000].fill(0);
    }
    for (address, word) in [
        (0x1000, 0x2027u64),
        (0x2000, 0x3027),
        (0x3000, 0x4027),
        (0x4028, 0x5027),
        (0x4030, 0x6027),
    ] {
        bytes[address..address + 8].copy_from_slice(&word.to_le_bytes());
    }
    if frame_present {
        bytes[0x4040..0x4048].copy_from_slice(&0x8027u64.to_le_bytes());
    }
    bytes[0x5008..0x5010].copy_from_slice(&0x00af_9b00_0000_ffffu64.to_le_bytes());
    for v in [1usize, 8, 13, 14] {
        let offset = 0x6000 + v * 16;
        bytes[offset..offset + 8].copy_from_slice(&0x0000_8e00_0008_7000u64.to_le_bytes());
        bytes[offset + 8..offset + 16].fill(0);
    }
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.image = Image::physical_mapped(
            vec![("bytes".into(), bytes)],
            vec![crate::memory::MappingInput {
                base: 0,
                backing: "bytes".into(),
                offset: 0,
                length: if frame_backed { 0xa000 } else { 0x8000 },
                permissions: crate::memory::Permissions {
                    read: true,
                    write: true,
                    execute: true,
                },
            }],
            crate::memory::MemoryLimits {
                max_backing_bytes: 0xa000,
                max_backings: 1,
                max_mappings: 1,
            },
        )
        .unwrap();
        core.paging = Some(crate::x86::paging::State {
            cr3: 0x1000,
            cr2: 0x501000,
            ..crate::x86::paging::State::default()
        });
        core.context.set_gdtr(crate::x86::context::TableRegister {
            base: 0x5000,
            limit: 15,
        });
        core.context.set_idtr(crate::x86::context::TableRegister {
            base: 0x6000,
            limit: 4095,
        });
        let event = if vector == 14 {
            ExceptionEvent::page_fault(0x400000, 0x202, &core.context, 6, 0x501000)
        } else {
            ExceptionEvent::fault(
                vector,
                0x400000,
                0x202,
                &core.context,
                matches!(vector, 8 | 10..=13).then_some(0),
            )
        };
        let state = unsafe { &mut *core.state.get() };
        state.rip = 0x400000;
        state.registers[4] = 0x9000;
        state.interruptibility = 0;
        state.debug.continuation = crate::embedding::layout::debug::Continuation::default();
        state.set_event(EventState::Pending(event));
        EventState::Pending(event)
    })
}

#[test]
fn automatic_frame_page_faults_apply_asymmetric_classes_without_publishing_slots() {
    for vector in [1, 13, 14, 8] {
        entry_fixture(vector, false, true);
        assert_eq!(
            core_deliver_exception(),
            if vector == 8 { SHUTDOWN } else { DISPATCH }
        );
        BRIDGE.with_borrow(|bridge| {
            let core = bridge.core.as_ref().unwrap();
            let state = unsafe { &*core.state.get() };
            assert_eq!(core.paging.unwrap().cr2, 0x8ff8);
            assert_eq!(
                core.image.physical_read_bytes(0x8fd0, 48).unwrap(),
                vec![0xa5; 48]
            );
            assert_eq!((state.rip, state.registers[4]), (0x400000, 0x9000));
            if vector == 8 {
                assert_eq!(state.event_state(), Ok(EventState::Shutdown));
            } else {
                let EventState::Pending(next) = state.event_state().unwrap() else {
                    panic!()
                };
                assert_eq!(
                    (next.vector, next.error_code, next.page_fault_address),
                    if vector == 14 {
                        (8, Some(0), None)
                    } else {
                        (14, Some(2), Some(0x8ff8))
                    }
                );
            }
        });
    }
}

#[test]
fn physical_entry_refusal_keeps_frozen_pf_and_repair_uses_live_cr2_without_replay() {
    let pending = entry_fixture(14, true, false);
    assert_eq!(core_deliver_exception(), PHYSICAL_MEMORY);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let state = unsafe { &*core.state.get() };
        assert_eq!(state.event_state(), Ok(pending));
        assert_eq!(core.paging.unwrap().cr2, 0x501000);
        core.paging.as_mut().unwrap().cr2 = 0x1234;
        core.image
            .set_mappings(vec![crate::memory::MappingInput {
                base: 0,
                backing: "bytes".into(),
                offset: 0,
                length: 0xa000,
                permissions: crate::memory::Permissions {
                    read: true,
                    write: true,
                    execute: true,
                },
            }])
            .unwrap();
    });
    assert_eq!(core_deliver_exception(), DISPATCH);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        let state = unsafe { &*core.state.get() };
        assert_eq!(state.event_state(), Ok(EventState::Ready));
        assert_eq!(core.paging.unwrap().cr2, 0x1234);
        assert_eq!((state.rip, state.registers[4]), (0x7000, 0x8fd0));
        assert_eq!(core.image.physical_read(0x8fd0, 8), Ok(6));
        assert_eq!(core.image.physical_read(0x8fd8, 8), Ok(0x400000));
        assert_eq!(core.image.physical_read(0x8fe8, 8), Ok(0x10202));
    });
}

#[test]
fn system_entry_page_fault_keeps_supervisor_zero_code_and_gate_provenance() {
    entry_fixture(13, true, true);
    BRIDGE.with_borrow_mut(|bridge| {
        bridge
            .core
            .as_mut()
            .unwrap()
            .image
            .physical_write(0x4030, 8, 0)
            .unwrap();
    });
    assert_eq!(core_deliver_exception(), DISPATCH);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        let state = unsafe { &*core.state.get() };
        let EventState::Pending(next) = state.event_state().unwrap() else {
            panic!()
        };
        assert_eq!(
            (next.vector, next.error_code, next.page_fault_address),
            (14, Some(0), Some(0x60d0))
        );
        assert_eq!(core.paging.unwrap().cr2, 0x60d0);
        let provenance = core.paging_system.unwrap();
        assert_eq!(
            (provenance.table, provenance.reference, provenance.phase),
            (
                crate::x86::protection::Table::Idt,
                crate::x86::protection::SystemReference::Vector(13),
                crate::x86::protection::SystemPhase::GateRead
            )
        );
        assert_eq!(
            core.image.physical_read_bytes(0x8fd0, 48).unwrap(),
            vec![0xa5; 48]
        );
    });
}

#[test]
fn system_physical_refusal_keeps_original_source_and_reports_physical_gate_provenance() {
    let pending = entry_fixture(14, true, true);
    BRIDGE.with_borrow_mut(|bridge| {
        bridge
            .core
            .as_mut()
            .unwrap()
            .image
            .physical_write(0x4030, 8, 0xb027)
            .unwrap();
    });
    assert_eq!(core_deliver_exception(), PHYSICAL_MEMORY);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert_eq!(unsafe { &*core.state.get() }.event_state(), Ok(pending));
        assert_eq!(core.paging.unwrap().cr2, 0x501000);
        let provenance = core.paging_system.unwrap();
        assert_eq!(
            (provenance.table, provenance.reference, provenance.phase),
            (
                crate::x86::protection::Table::Idt,
                crate::x86::protection::SystemReference::Vector(14),
                crate::x86::protection::SystemPhase::GateRead
            )
        );
        let Failure::Physical(fault) = provenance.failure else {
            panic!()
        };
        assert_eq!(
            (
                fault.linear,
                fault.access,
                fault.physical.address,
                fault.phase
            ),
            (0x60e0, Access::Read, 0xb0e0, PhysicalPhase::FinalAccess)
        );
    });
}
