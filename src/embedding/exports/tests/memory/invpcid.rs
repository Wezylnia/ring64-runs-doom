use crate::embedding::exports::{execution::invalidation::core_invpcid, memory::paging, tests::*};
use crate::embedding::layout::{PAGE_FAULT, PHYSICAL_MEMORY, PROTECTION};
use crate::x86::context::Mode;
use crate::x86::protection::ProtectionPhase;

fn fixture(mode: Mode, cpl: u8) {
    BRIDGE.with_borrow_mut(|bridge| {
        *bridge = Bridge::default();
        bridge.input.push(0x90);
        bridge.input.resize(1 + 0x8000, 0);
    });
    assert_eq!(core_init(0x1000, 1, 0x4000, 0x8000), 0);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.context.set_mode_and_cpl(mode, cpl);
        core.paging = Some(crate::x86::paging::State {
            cr0: if mode == Mode::Real { 0x10 } else { 0x11 },
            cr2: 0xdead,
            ..crate::x86::paging::State::reset()
        });
        core.physical_controls = true;
    });
}

fn descriptor(at: u64, pcid_word: u64, linear: u64) {
    BRIDGE.with_borrow_mut(|bridge| {
        let image = &mut bridge.core.as_mut().unwrap().image;
        image.physical_write(at, 8, pcid_word).unwrap();
        image.physical_write(at + 8, 8, linear).unwrap();
    });
}

fn phase() -> ProtectionPhase {
    BRIDGE.with_borrow(|bridge| {
        bridge
            .core
            .as_ref()
            .unwrap()
            .protection_fault
            .unwrap()
            .phase
    })
}

#[test]
fn invpcid_privilege_and_type_refusal_precede_operand_address_admission() {
    for mode in [Mode::Protected, Mode::Virtual8086] {
        fixture(mode, if mode == Mode::Virtual8086 { 0 } else { 3 });
        assert_eq!(core_invpcid(Segment::Ds as u32, u64::MAX, 4, 2), PROTECTION);
        assert_eq!(phase(), ProtectionPhase::Privilege);
    }
    fixture(Mode::Protected, 0);
    assert_eq!(core_invpcid(Segment::Ds as u32, u64::MAX, 4, 2), PROTECTION);
    assert_eq!(phase(), ProtectionPhase::Target);
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        assert!(core.fault.is_none());
        assert!(core.segment_fault.is_none());
        assert!(core.paging_failure.is_none());
        assert_eq!(core.paging.unwrap().cr2, 0xdead);
    });
}

#[test]
fn invpcid_non64_type_and_descriptor_address_widths_are_independent() {
    for mode in [Mode::Real, Mode::Protected, Mode::Compatibility] {
        fixture(mode, 0);
        descriptor(0x4001, 0, 0xffff800000004000);
        assert_eq!(
            core_invpcid(Segment::Ds as u32, 0x4001, 0x1234567800000000, 2),
            0
        );
        descriptor(0x4001, 0, 0x800000000000);
        assert_eq!(core_invpcid(Segment::Ds as u32, 0x4001, 0, 2), PROTECTION);
        assert_eq!(phase(), ProtectionPhase::Target);
        for kind in 1..4 {
            assert_eq!(core_invpcid(Segment::Ds as u32, 0x4001, kind, 2), 0);
        }
        descriptor(0x4001, 0, 0);
        assert_eq!(
            core_invpcid(Segment::Ds as u32, 0x4001, 0x10000, 2),
            PROTECTION
        );
    }
    fixture(Mode::Long64, 0);
    descriptor(0x4001, 0, 0);
    assert_eq!(
        core_invpcid(Segment::Ds as u32, 0x4001, 0x100000000, 2),
        PROTECTION
    );
}

#[test]
fn invpcid_reserved_pcid_rules_and_full_nominal_descriptor_apply_to_all_types() {
    for kind in 0..4 {
        fixture(Mode::Protected, 0);
        descriptor(0x4000, 0x1000, 0);
        assert_eq!(
            core_invpcid(Segment::Ds as u32, 0x4000, kind, 2),
            PROTECTION
        );
        assert_eq!(phase(), ProtectionPhase::Target);
        descriptor(0x4000, 0xfff, 0);
        assert_eq!(
            core_invpcid(Segment::Ds as u32, 0x4000, kind, 2),
            if kind < 2 { PROTECTION } else { 0 }
        );
        for low in [0, 0x1000] {
            BRIDGE.with_borrow_mut(|bridge| {
                bridge
                    .core
                    .as_mut()
                    .unwrap()
                    .image
                    .physical_write(0xbff8, 8, low)
                    .unwrap();
            });
            assert_eq!(
                core_invpcid(Segment::Ds as u32, 0xbff8, kind, 2),
                PHYSICAL_MEMORY
            );
            assert_eq!(metadata(PHYSICAL_MEMORY)["width"], 16);
            assert_eq!(
                metadata(PHYSICAL_MEMORY)["failingPhysicalAddress"],
                "0xc000"
            );
            BRIDGE.with_borrow(|bridge| {
                assert_eq!(bridge.core.as_ref().unwrap().paging.unwrap().cr2, 0xdead)
            });
        }
    }
}

#[test]
fn invpcid_descriptor_is_explicit_supervisor_data_using_raw_ac() {
    fixture(Mode::Protected, 0);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.image.physical_write(0x4000, 4, 0x5007).unwrap();
        core.image.physical_write(0x5010, 4, 0x8007).unwrap();
        core.paging = Some(crate::x86::paging::State {
            cr0: 0x80040033,
            cr2: 0xdead,
            cr3: 0x4000,
            cr4: 1 << 21,
            efer: 0,
            pdpte: None,
        });
    });
    descriptor(0x8000, 0, 0);
    assert_eq!(core_invpcid(Segment::Ds as u32, 0x4000, 2, 2), PAGE_FAULT);
    assert_eq!(metadata(PAGE_FAULT)["errorCode"], 1);
    assert_eq!(metadata(PAGE_FAULT)["address"], "0x4000");
    assert_eq!(core_invpcid(Segment::Ds as u32, 0x4000, 2, 0x40002), 0);
}

#[test]
fn invpcid_stages_descriptor_debug_only_after_successful_content_checks() {
    fixture(Mode::Protected, 0);
    BRIDGE.with_borrow_mut(|bridge| unsafe {
        let debug = &mut (*bridge.core.as_mut().unwrap().state.get()).debug.registers;
        debug.write(0, 0x4000);
        debug.write(7, 0x30001);
    });
    descriptor(0x4000, 0x1000, 0);
    assert_eq!(core_invpcid(Segment::Ds as u32, 0x4000, 2, 2), PROTECTION);
    BRIDGE.with_borrow(|bridge| assert_eq!(bridge.core.as_ref().unwrap().debug_candidate, 0));
    descriptor(0x4000, 0, 0);
    assert_eq!(core_invpcid(Segment::Ds as u32, 0x4000, 2, 2), 0);
    BRIDGE.with_borrow(|bridge| assert_eq!(bridge.core.as_ref().unwrap().debug_candidate, 1));
}

#[test]
fn invpcid_each_type_exposes_a_guest_retargeted_translation_without_target_access() {
    for kind in 0..4 {
        fixture(Mode::Long64, 0);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            core.paging = Some(crate::x86::paging::State {
                cr3: 0x4001,
                cr4: 0x20628,
                ..crate::x86::paging::State::default()
            });
            for (at, value) in [
                (0x4000, 0x5027),
                (0x5000, 0x6027),
                (0x6000, 0x7027),
                (0x7020, 0x8067),
                (0x7028, 0x9067),
            ] {
                core.image.physical_write(at, 8, value).unwrap();
            }
            core.image.physical_write(0x8000, 4, 0x44332211).unwrap();
            core.image.physical_write(0xa000, 4, 0x88776655).unwrap();
        });
        descriptor(0x9000, 1, 0x4000);
        assert_eq!(core_paged_load(0x4000, 4), 0x44332211);
        BRIDGE.with_borrow_mut(|bridge| {
            bridge
                .core
                .as_mut()
                .unwrap()
                .image
                .physical_write(0x7020, 8, 0xa067)
                .unwrap()
        });
        assert_eq!(core_paged_load(0x4000, 4), 0x44332211);
        assert_eq!(core_invpcid(Segment::Ds as u32, 0x5000, kind, 2), 0);
        assert_eq!(core_paged_load(0x4000, 4), 0x88776655);
    }
}

#[test]
fn invpcid_type_scopes_preserve_other_pcids_addresses_and_globals() {
    for kind in 0..4 {
        fixture(Mode::Long64, 0);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            core.paging = Some(crate::x86::paging::State {
                cr3: 0x4001,
                cr4: 0x206a8,
                ..crate::x86::paging::State::default()
            });
            for (at, value) in [
                (0x4000, 0x5027),
                (0x5000, 0x6027),
                (0x6000, 0x7027),
                (0x7020, 0x8063),
                (0x7028, 0x9063),
                (0x7030, 0xa063),
                (0x7048, 0xb163),
                (0x8000, 0x11111111),
                (0xa000, 0x22222222),
                (0xb000, 0x33333333),
            ] {
                core.image.physical_write(at, 8, value).unwrap();
            }
        });
        descriptor(0x9000, 1, 0x4000);
        assert_eq!(core_paged_load(0x4000, 4), 0x11111111);
        assert_eq!(core_paged_load(0x6000, 4), 0x22222222);
        assert_eq!(core_paged_load(0x9000, 4), 0x33333333);
        assert_eq!(
            paging::control::core_paging_control(1, 3, 0x8000000000004002, 0x1000, 2, 0),
            0
        );
        assert_eq!(core_paged_load(0x4000, 4), 0x11111111);
        assert_eq!(core_paged_load(0x6000, 4), 0x22222222);
        BRIDGE.with_borrow_mut(|bridge| {
            let core = bridge.core.as_mut().unwrap();
            for (at, value) in [(0x7020, 0xa063), (0x7030, 0x8063), (0x7048, 0xa163)] {
                core.image.physical_write(at, 8, value).unwrap();
            }
        });
        assert_eq!(core_invpcid(Segment::Ds as u32, 0x5000, kind, 2), 0);
        assert_eq!(
            paging::control::core_paging_control(1, 3, 0x8000000000004001, 0x1000, 2, 0),
            0
        );
        assert_eq!(core_paged_load(0x4000, 4), 0x22222222, "type{kind} target");
        assert_eq!(
            core_paged_load(0x6000, 4),
            if kind == 0 { 0x22222222 } else { 0x11111111 },
            "type{kind} same-PCID unrelated address"
        );
        assert_eq!(
            core_paged_load(0x9000, 4),
            if kind == 2 { 0x22222222 } else { 0x33333333 },
            "type{kind} global"
        );
        assert_eq!(
            paging::control::core_paging_control(1, 3, 0x8000000000004002, 0x1000, 2, 0),
            0
        );
        assert_eq!(
            core_paged_load(0x4000, 4),
            if kind >= 2 { 0x22222222 } else { 0x11111111 },
            "type{kind} other PCID"
        );
        assert_eq!(
            core_paged_load(0x6000, 4),
            if kind >= 2 { 0x11111111 } else { 0x22222222 },
            "type{kind} other-PCID unrelated address"
        );
    }
}
