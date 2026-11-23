mod legacy;
mod pcid;

use super::*;
use ControlFailure::{GeneralProtection, Undefined, Unsupported};

#[test]
fn protection_key_controls_are_admitted_in_fixed_and_dormant_physical_profiles() {
    for bits in [1 << 22, 1 << 24, (1 << 22) | (1 << 24)] {
        let fixed = State {
            cr4: State::default().cr4 | bits,
            ..State::default()
        };
        assert_eq!(fixed.validate(), Ok(()), "fixed controls {bits:x}");
        for (bank, mode) in [
            (State::reset(), Mode::Real),
            (
                State {
                    cr0: 0x60000011,
                    ..State::reset()
                },
                Mode::Protected,
            ),
            (State::default(), Mode::Long64),
        ] {
            let candidate = State {
                cr4: bank.cr4 | bits,
                ..bank
            };
            assert_eq!(
                candidate.validate_physical(mode),
                Ok(()),
                "physical {mode:?} controls {bits:x}"
            );
        }
    }
}

#[test]
fn protection_key_toggles_preserve_translation_and_pdpte_authority() {
    let fixed = State::default();
    for bits in [1 << 22, 1 << 24, (1 << 22) | (1 << 24)] {
        let enabled = State {
            cr4: fixed.cr4 | bits,
            ..fixed
        };
        assert_eq!(
            fixed.prepare_cr_write(4, enabled.cr4, Mode::Long64),
            Ok(enabled)
        );
        assert_eq!(
            enabled.prepare_cr_write(4, fixed.cr4, Mode::Compatibility),
            Ok(fixed)
        );
        assert!(!fixed.translation_changed(enabled));
        assert!(!enabled.translation_changed(fixed));
        let legacy = State {
            efer: 0,
            pdpte: Some([0x2001, 0, 0, 0]),
            ..fixed
        };
        let enabled = State {
            cr4: legacy.cr4 | bits,
            ..legacy
        };
        assert_eq!(
            legacy.prepare_physical_cr_write(4, enabled.cr4, Mode::Protected),
            Ok(enabled)
        );
        assert!(!legacy.translation_changed(enabled));
        assert!(!enabled.translation_changed(legacy));
        assert!(!legacy.needs_pdpte_reload(enabled, 4));
        assert!(!enabled.needs_pdpte_reload(legacy, 4));
        assert!(legacy.translation_changed(State {
            cr3: 0x1000,
            ..enabled
        }));
    }
    assert_eq!(
        fixed.prepare_cr_write(4, fixed.cr4 | (1 << 14), Mode::Long64),
        Err(Unsupported)
    );
}

#[test]
fn enabling_segment_base_instructions_preserves_page_translation_state() {
    let fixed = State::default();
    let bit = 1 << 16;
    assert_eq!(State::reset().cr4 & bit, 0);
    assert_eq!(fixed.cr4 & bit, 0);
    let enabled = State {
        cr4: fixed.cr4 | bit,
        ..fixed
    };
    assert_eq!(enabled.validate(), Ok(()));
    assert_eq!(
        fixed.prepare_cr_write(4, enabled.cr4, Mode::Long64),
        Ok(enabled)
    );
    assert_eq!(
        enabled.prepare_cr_write(4, fixed.cr4, Mode::Compatibility),
        Ok(fixed)
    );
    for (old, mode) in [
        (fixed, Mode::Long64),
        (State::reset(), Mode::Real),
        (
            State {
                efer: 0,
                pdpte: Some([0x2001, 0, 0, 0]),
                ..fixed
            },
            Mode::Protected,
        ),
    ] {
        let enabled = State {
            cr4: old.cr4 | bit,
            ..old
        };
        assert_eq!(enabled.validate_physical(mode), Ok(()));
        assert_eq!(
            old.prepare_physical_cr_write(4, enabled.cr4, mode),
            Ok(enabled)
        );
        assert_eq!(enabled.prepare_physical_cr_write(4, old.cr4, mode), Ok(old));
        assert!(!old.translation_changed(enabled));
        assert!(!enabled.translation_changed(old));
        assert!(!old.needs_pdpte_reload(enabled, 4));
        assert!(!enabled.needs_pdpte_reload(old, 4));
    }
}

#[test]
fn osxsave_toggles_are_independent_of_osfxsr_and_translation() {
    for osfxsr in [0, 1 << 9] {
        let old = State {
            cr4: State::default().cr4 & !(1 << 9) | osfxsr,
            ..State::default()
        };
        let enabled = State {
            cr4: old.cr4 | CR4_OSXSAVE,
            ..old
        };
        assert_eq!(
            old.prepare_cr_write(4, enabled.cr4, Mode::Long64),
            Ok(enabled)
        );
        assert_eq!(
            enabled.prepare_physical_cr_write(4, old.cr4, Mode::Long64),
            Ok(old)
        );
        assert!(!old.translation_changed(enabled));
        assert!(!old.needs_pdpte_reload(enabled, 4));
    }
}

#[test]
fn alignment_mask_is_admitted_without_translation_or_pdpte_effects() {
    const AM: u64 = 1 << 18;
    let fixed = State::default();
    for mode in [Mode::Compatibility, Mode::Long64] {
        let enabled = State {
            cr0: fixed.cr0 | AM,
            ..fixed
        };
        assert_eq!(enabled.validate(), Ok(()));
        assert_eq!(fixed.prepare_cr_write(0, enabled.cr0, mode), Ok(enabled));
        assert_eq!(enabled.read_cr(0), Ok(fixed.cr0 | AM));
        assert_eq!(enabled.prepare_cr_write(0, fixed.cr0, mode), Ok(fixed));
        assert!(!fixed.translation_changed(enabled));
        assert!(!enabled.translation_changed(fixed));
        assert_eq!(fixed.numeric_controls(), enabled.numeric_controls());
        assert!(fixed.translation_changed(State {
            cr0: enabled.cr0 | (1 << 16),
            ..enabled
        }));
    }
    for (state, mode) in [
        (State::reset(), Mode::Real),
        (
            State {
                cr0: 0x60000011,
                ..State::reset()
            },
            Mode::Protected,
        ),
        (
            State {
                cr0: 0x60000011,
                ..State::reset()
            },
            Mode::Virtual8086,
        ),
        (
            State {
                cr0: 0x80000033,
                cr4: 0x608,
                efer: 0,
                ..fixed
            },
            Mode::Protected,
        ),
        (
            State {
                cr0: 0x80000033,
                cr4: 0x608,
                efer: 0,
                ..fixed
            },
            Mode::Virtual8086,
        ),
        (
            State {
                cr0: 0x80000033,
                cr4: 0x628,
                efer: 0,
                pdpte: Some([0x3001, 0, 0, 0]),
                ..fixed
            },
            Mode::Protected,
        ),
        (
            State {
                cr0: 0x80000033,
                cr4: 0x628,
                efer: 0,
                pdpte: Some([0x3001, 0, 0, 0]),
                ..fixed
            },
            Mode::Virtual8086,
        ),
        (fixed, Mode::Compatibility),
        (fixed, Mode::Long64),
    ] {
        let enabled = State {
            cr0: state.cr0 | AM,
            ..state
        };
        assert_eq!(enabled.validate_physical(mode), Ok(()), "{mode:?}");
        assert_eq!(
            state.prepare_physical_cr_write(0, enabled.cr0, mode),
            Ok(enabled)
        );
        assert_eq!(
            enabled.prepare_physical_cr_write(0, state.cr0, mode),
            Ok(state)
        );
        assert!(!state.translation_changed(enabled), "{mode:?}");
        assert!(!enabled.translation_changed(state), "{mode:?}");
        assert!(!state.needs_pdpte_reload(enabled, 0), "{mode:?}");
        assert_eq!(state.pdpte, enabled.pdpte);
        assert_eq!(state.numeric_controls(), enabled.numeric_controls());
    }
}

#[test]
fn alignment_mask_does_not_relax_cr0_architectural_or_fixed_profile_rules() {
    const AM: u64 = 1 << 18;
    let fixed = State::default();
    for value in [0x180040033, 0x80040032, 0xa0040033, 0x40033] {
        assert_eq!(
            fixed.prepare_cr_write(0, value, Mode::Long64),
            Err(GeneralProtection)
        );
        assert_eq!(
            fixed.prepare_physical_cr_write(0, value, Mode::Long64),
            Err(GeneralProtection)
        );
    }
    assert_eq!(
        fixed.prepare_cr_write(0, 0xe0040033, Mode::Long64),
        Err(Unsupported)
    );
    assert!(
        State {
            cr0: fixed.cr0 | AM | (1 << 6),
            ..fixed
        }
        .validate()
        .is_err()
    );
    assert!(
        State {
            cr0: State::reset().cr0 | AM | 0x80000000,
            ..State::reset()
        }
        .validate_physical(Mode::Real)
        .is_err()
    );
    let pae_off = State {
        cr0: 0x33 | AM,
        cr4: 0x628,
        efer: 0,
        ..fixed
    };
    let pae_on = pae_off
        .prepare_physical_cr_write(0, 0x80040033, Mode::Protected)
        .unwrap();
    assert!(pae_off.needs_pdpte_reload(pae_on, 0));
    assert!(pae_off.translation_changed(pae_on));
}

#[test]
fn supervisor_protection_controls_are_admitted_in_both_control_models() {
    for bits in [1 << 20, 1 << 21, 3 << 20] {
        let fixed = State {
            cr4: State::default().cr4 | bits,
            ..State::default()
        };
        assert_eq!(fixed.validate(), Ok(()), "fixed bits {bits:x}");
        assert_eq!(
            State::default().prepare_cr_write(4, fixed.cr4, Mode::Long64),
            Ok(fixed)
        );
        let physical = State {
            cr0: 0x80000033,
            cr4: 0x608 | bits,
            efer: 0,
            ..State::default()
        };
        assert_eq!(physical.validate_physical(Mode::Protected), Ok(()));
        assert_eq!(
            State {
                cr4: 0x608,
                ..physical
            }
            .prepare_physical_cr_write(4, physical.cr4, Mode::Protected),
            Ok(physical)
        );
    }
}

#[test]
fn supervisor_protection_control_effects_keep_quartet_and_numeric_authorities_separate() {
    for (state, mode) in [
        (State::reset(), Mode::Real),
        (
            State {
                cr0: 0x60000011,
                ..State::reset()
            },
            Mode::Protected,
        ),
        (
            State {
                cr0: 0x80000033,
                cr4: 0x608,
                efer: 0,
                ..State::default()
            },
            Mode::Protected,
        ),
        (
            State {
                cr0: 0x80000033,
                cr4: 0x628,
                efer: 0,
                pdpte: Some([0x3001, 0, 0, 0]),
                ..State::default()
            },
            Mode::Protected,
        ),
        (State::default(), Mode::Long64),
    ] {
        for bits in [0, CR4_SMEP, CR4_SMAP, CR4_SMEP | CR4_SMAP] {
            let candidate = State {
                cr4: state.cr4 | bits,
                ..state
            };
            assert_eq!(candidate.validate_physical(mode), Ok(()));
            assert_eq!(
                state.prepare_physical_cr_write(4, candidate.cr4, mode),
                Ok(candidate),
                "{mode:?}/bits{bits:x}"
            );
            assert_eq!(state.numeric_controls(), candidate.numeric_controls());
            assert_eq!(state.translation_changed(candidate), bits != 0);
            assert_eq!(candidate.translation_changed(state), bits != 0);
            let reload = state.legacy_pae() && bits & CR4_SMEP != 0;
            assert_eq!(state.needs_pdpte_reload(candidate, 4), reload);
            assert_eq!(candidate.needs_pdpte_reload(state, 4), reload);
            assert!(!candidate.needs_pdpte_reload(candidate, 4));
        }
    }
}

fn bank() -> State {
    State {
        cr0: 0x80000033,
        cr2: 0xffffffffffffffff,
        cr3: 0x0008000000003018,
        cr4: 0x628,
        efer: 0x500,
        pdpte: None,
    }
}

#[test]
fn timestamp_disable_is_a_live_permission_not_translation_or_numeric_state() {
    let fixed = bank();
    let disabled = State {
        cr4: fixed.cr4 | 4,
        ..fixed
    };
    assert_eq!(disabled.validate(), Ok(()));
    assert_eq!(
        fixed.prepare_cr_write(4, disabled.cr4, Mode::Long64),
        Ok(disabled)
    );
    assert!(!fixed.translation_changed(disabled));
    assert_eq!(fixed.numeric_controls(), disabled.numeric_controls());
    for (state, mode) in [
        (State::reset(), Mode::Real),
        (
            State {
                cr0: 0x60000011,
                ..State::reset()
            },
            Mode::Protected,
        ),
        (
            State {
                cr0: 0x80000033,
                cr4: 0x608,
                efer: 0,
                ..State::default()
            },
            Mode::Protected,
        ),
        (
            State {
                cr0: 0x80000033,
                cr4: 0x628,
                efer: 0,
                pdpte: Some([0x3001, 0, 0, 0]),
                ..State::default()
            },
            Mode::Protected,
        ),
        (State::default(), Mode::Long64),
    ] {
        let candidate = State {
            cr4: state.cr4 | 4,
            ..state
        };
        assert_eq!(candidate.validate_physical(mode), Ok(()));
        assert_eq!(
            state.prepare_physical_cr_write(4, candidate.cr4, mode),
            Ok(candidate)
        );
        assert!(!state.translation_changed(candidate));
        assert_eq!(state.numeric_controls(), candidate.numeric_controls());
        assert!(!state.needs_pdpte_reload(candidate, 4));
    }
}

#[test]
fn all_numeric_enable_images_are_valid_and_do_not_change_translation() {
    for bits in 0u64..32 {
        let state = State {
            cr0: 0x80000031 | (bits & 7) << 1,
            cr4: 0x28 | (bits >> 3) << 9,
            ..bank()
        };
        assert_eq!(state.validate(), Ok(()), "{bits}");
        assert!(!bank().translation_changed(state), "{bits}");
        assert_eq!(
            bank()
                .prepare_cr_write(0, state.cr0, Mode::Long64)
                .unwrap()
                .cr0,
            state.cr0
        );
        assert_eq!(
            bank()
                .prepare_cr_write(4, state.cr4, Mode::Long64)
                .unwrap()
                .cr4,
            state.cr4
        );
    }
}

#[test]
fn imported_bank_is_complete_normalized_and_does_not_probe_a_root() {
    assert_eq!(
        State::default(),
        State {
            cr2: 0,
            cr3: 0,
            ..bank()
        }
    );
    assert_eq!(bank().validate(), Ok(()));
    assert_eq!(
        State {
            cr0: 0x80010033,
            efer: 0xd00,
            ..bank()
        }
        .validate(),
        Ok(())
    );
    for field in 0..5 {
        let mut bad = bank();
        match field {
            0 => bad.cr0 |= 1 << 6,
            1 => bad.cr0 &= !1,
            2 => bad.cr3 |= 1 << 52,
            3 => bad.cr4 |= 1,
            4 => bad.efer |= 1 << 1,
            _ => unreachable!(),
        }
        assert!(bad.validate().is_err(), "field {field}");
    }
    for bit in 52..64 {
        assert!(
            State {
                cr3: 1 << bit,
                ..bank()
            }
            .validate()
            .is_err()
        );
    }
}

#[test]
fn cr_register_legality_and_raw_cr2_are_owned_independently() {
    let state = bank();
    for (register, expected) in [
        (0, state.cr0),
        (2, state.cr2),
        (3, state.cr3),
        (4, state.cr4),
    ] {
        assert_eq!(state.read_cr(register), Ok(expected));
    }
    for register in [1, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15] {
        assert_eq!(state.read_cr(register), Err(Undefined));
        assert_eq!(
            state.prepare_cr_write(register, 0, Mode::Long64),
            Err(Undefined)
        );
    }
    assert_eq!(state.read_cr(8), Err(Unsupported));
    assert_eq!(state.prepare_cr_write(8, 0, Mode::Long64), Err(Unsupported));
    let changed = state
        .prepare_cr_write(2, 0x8000000000000000, Mode::Long64)
        .unwrap();
    assert_eq!(
        changed,
        State {
            cr2: 0x8000000000000000,
            ..state
        }
    );
    assert!(!state.translation_changed(changed));
}

#[test]
fn cr0_ignores_low_reserved_bits_but_checks_values_before_profile_support() {
    let state = bank();
    assert_eq!(
        state.prepare_cr_write(0, 0x80000023 | (1 << 6), Mode::Long64),
        Ok(state)
    );
    let wp = state.prepare_cr_write(0, 0x80010033, Mode::Long64).unwrap();
    assert_eq!(wp.cr0, 0x80010033);
    assert!(state.translation_changed(wp));
    for value in [0x180000033, 0x80000032, 0xa0000033, 0x33] {
        assert_eq!(
            state.prepare_cr_write(0, value, Mode::Long64),
            Err(GeneralProtection)
        );
    }
    assert_eq!(
        state.prepare_cr_write(0, 0x33, Mode::Compatibility),
        Err(Unsupported)
    );
    assert_eq!(
        state.prepare_cr_write(8, 16, Mode::Long64),
        Err(GeneralProtection)
    );
    for value in [0x80000013, 0xe0000033] {
        assert_eq!(
            state.prepare_cr_write(0, value, Mode::Long64),
            Err(Unsupported),
            "{value:x}"
        );
    }
}

#[test]
fn cr3_ignored_low_bits_normalize_without_losing_root_or_cache_bits() {
    let state = bank();
    let changed = state
        .prepare_cr_write(3, 0x000fffffffffffff, Mode::Long64)
        .unwrap();
    assert_eq!(changed.cr3, 0x000ffffffffff018);
    for bit in 52..=60 {
        assert_eq!(
            state.prepare_cr_write(3, (1 << bit) | (1 << 61), Mode::Long64),
            Err(GeneralProtection)
        );
    }
    assert_eq!(
        state.prepare_cr_write(3, 1 << 63, Mode::Long64),
        Err(GeneralProtection)
    );
    for bit in [61, 62] {
        assert_eq!(
            state.prepare_cr_write(3, 1 << bit, Mode::Long64),
            Err(Unsupported)
        );
    }
    assert_eq!(
        state.prepare_cr_write(3, state.cr3, Mode::Long64),
        Ok(state)
    );
}

#[test]
fn cr4_reserved_and_architectural_transitions_precede_deferred_features() {
    let state = bank();
    assert_eq!(state.prepare_cr_write(4, 0x628, Mode::Long64), Ok(state));
    for value in [0x608, 0x1628, 0x20628, 0x800628] {
        assert_eq!(
            state.prepare_cr_write(4, value, Mode::Long64),
            Err(GeneralProtection),
            "{value:x}"
        );
    }
    for bit in [15, 26, 29, 30, 31, 33, 63] {
        assert_eq!(
            state.prepare_cr_write(4, 0x628 | (1 << bit), Mode::Long64),
            Err(GeneralProtection)
        );
    }
    let aligned = State {
        cr0: 0x80010033,
        cr3: 0x3000,
        ..state
    };
    for bit in [0, 14, 19, 23, 25, 27, 28, 32] {
        assert_eq!(
            aligned.prepare_cr_write(4, 0x628 | (1 << bit), Mode::Long64),
            Err(Unsupported),
            "bit {bit}"
        );
    }
    assert_eq!(
        state.prepare_cr_write(4, 0x620, Mode::Long64),
        Err(Unsupported)
    );
}

#[test]
fn efer_lma_is_read_only_lme_cannot_change_and_nxe_is_live_authority() {
    let state = bank();
    assert_eq!(state.prepare_efer_write(0x100), Ok(state));
    let nxe = state.prepare_efer_write(0x900).unwrap();
    assert_eq!(nxe.efer, 0xd00);
    assert!(state.translation_changed(nxe));
    assert_eq!(nxe.prepare_efer_write(0x500), Ok(state));
    for value in [0x400, 0, 0x1500, 0x501 | (1 << 63)] {
        assert_eq!(
            state.prepare_efer_write(value),
            Err(GeneralProtection),
            "{value:x}"
        );
    }
    assert_eq!(
        state.prepare_efer_write(0x501),
        Ok(State {
            efer: 0x501,
            ..state
        })
    );
    assert_eq!(state, bank());
}

#[test]
fn efer_sce_is_live_normalized_state_without_changing_other_controls() {
    for efer in [0x500, 0x501, 0xd00, 0xd01] {
        let state = State { efer, ..bank() };
        assert_eq!(state.validate(), Ok(()));
        assert_eq!(state.read_msr(0xc0000080), Ok(efer));
        for input in [0x100, 0x101, 0x900, 0x901, 0x500, 0x501, 0xd00, 0xd01] {
            let next = state.prepare_efer_write(input).unwrap();
            assert_eq!(
                next,
                State {
                    efer: (input & 0x801) | 0x500,
                    ..state
                }
            );
            assert_eq!(next.cr2, state.cr2);
        }
    }
}

#[test]
fn sce_only_updates_do_not_change_paging_authority_but_nxe_and_cr_changes_do() {
    let state = bank();
    assert!(!state.translation_changed(State {
        efer: 0x501,
        ..state
    }));
    let nxe = State {
        efer: 0xd00,
        ..state
    };
    assert!(!nxe.translation_changed(State { efer: 0xd01, ..nxe }));
    assert!(state.translation_changed(nxe));
    assert!(state.translation_changed(State {
        cr0: 0x80010033,
        ..state
    }));
    assert!(state.translation_changed(State {
        cr3: 0x5000,
        ..state
    }));
    assert!(state.translation_changed(State {
        cr4: 0x620,
        ..state
    }));
    assert!(!state.translation_changed(State {
        cr2: 0x400000,
        ..state
    }));
}

#[test]
fn efer_is_the_selected_msr_and_other_indices_remain_private() {
    let state = bank();
    assert_eq!(state.read_msr(0xc0000080), Ok(0x500));
    assert_eq!(
        state.prepare_msr_write(0xc0000080, 0xd00),
        Ok(State {
            efer: 0xd00,
            ..state
        })
    );
    for index in [0, 0x174, 0xc0000081, u32::MAX] {
        assert_eq!(state.read_msr(index), Err(Unsupported));
        assert_eq!(state.prepare_msr_write(index, u64::MAX), Err(Unsupported));
    }
}
