use super::*;

const PGE: u64 = 1 << 7;
const PCIDE: u64 = 1 << 17;
const NO_FLUSH: u64 = 1 << 63;

#[test]
fn pge_is_admitted_without_changing_control_family_or_numeric_authority() {
    let fixed = State::default();
    let states = [
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
                ..fixed
            },
            Mode::Protected,
        ),
        (
            State {
                efer: 0,
                pdpte: Some([0x3001, 0, 0, 0]),
                ..fixed
            },
            Mode::Virtual8086,
        ),
        (fixed, Mode::Compatibility),
        (fixed, Mode::Long64),
    ];
    for (old, mode) in states {
        let next = State {
            cr4: old.cr4 | PGE,
            ..old
        };
        assert_eq!(next.validate_physical(mode), Ok(()), "{mode:?}");
        assert_eq!(
            old.prepare_physical_cr_write(4, next.cr4, mode),
            Ok(next),
            "{mode:?}"
        );
        assert_eq!(next.numeric_controls(), old.numeric_controls());
        assert_eq!(old.needs_pdpte_reload(next, 4), old.legacy_pae());
        assert_eq!(next.needs_pdpte_reload(old, 4), old.legacy_pae());
    }
    let enabled = State {
        cr4: fixed.cr4 | PGE,
        ..fixed
    };
    assert_eq!(enabled.validate(), Ok(()));
    assert_eq!(
        fixed.prepare_cr_write(4, enabled.cr4, Mode::Long64),
        Ok(enabled)
    );
    assert_eq!(
        enabled.prepare_cr_write(4, fixed.cr4, Mode::Long64),
        Ok(fixed)
    );
}

#[test]
fn pcide_transition_checks_literal_low_bits_only_while_enabling() {
    let fixed = State::default();
    for mode in [Mode::Long64, Mode::Compatibility] {
        let enabled = State {
            cr4: fixed.cr4 | PCIDE,
            ..fixed
        };
        assert_eq!(fixed.prepare_cr_write(4, enabled.cr4, mode), Ok(enabled));
        assert_eq!(
            fixed.prepare_physical_cr_write(4, enabled.cr4, mode),
            Ok(enabled)
        );
        let tagged = State {
            cr3: 0x5abc,
            ..enabled
        };
        assert_eq!(tagged.pcid(), 0xabc);
        assert_eq!(tagged.validate(), Ok(()));
        assert_eq!(tagged.validate_physical(mode), Ok(()));
        assert_eq!(tagged.prepare_cr_write(4, tagged.cr4, mode), Ok(tagged));
        assert_eq!(
            tagged.prepare_physical_cr_write(4, tagged.cr4 | PGE, mode),
            Ok(State {
                cr4: tagged.cr4 | PGE,
                ..tagged
            })
        );
        let disabled = tagged.prepare_cr_write(4, fixed.cr4, mode).unwrap();
        assert_eq!(disabled.cr3, 0x5abc);
        assert_eq!(disabled.pcid(), 0);
        assert_eq!(disabled.read_cr(3), Ok(0x5abc));
        assert_eq!(disabled.validate(), Ok(()));
        assert_eq!(disabled.validate_physical(mode), Ok(()));
        assert_eq!(
            disabled.prepare_cr_write(4, enabled.cr4, mode),
            Err(ControlFailure::GeneralProtection)
        );
        assert_eq!(
            disabled.prepare_physical_cr_write(4, enabled.cr4, mode),
            Err(ControlFailure::GeneralProtection)
        );
    }
    for cr3 in [1, 8, 0x10, 0x20, 0x800, 0xfff] {
        let state = State { cr3, ..fixed };
        assert_eq!(
            state.prepare_cr_write(4, fixed.cr4 | PCIDE, Mode::Long64),
            Err(ControlFailure::GeneralProtection),
            "cr3={cr3:x}"
        );
    }
}

#[test]
fn pcide_requires_lma_and_prevents_pg_clear_even_in_compatibility() {
    let fixed = State::default();
    for state in [
        State::reset(),
        State {
            cr0: 0x60000011,
            ..State::reset()
        },
        State {
            cr0: 0x80000033,
            cr4: 0x608,
            efer: 0,
            ..fixed
        },
        State {
            efer: 0,
            pdpte: Some([0x3001, 0, 0, 0]),
            ..fixed
        },
    ] {
        assert_eq!(
            state.prepare_physical_cr_write(4, state.cr4 | PCIDE, Mode::Protected),
            Err(ControlFailure::GeneralProtection)
        );
        assert!(
            State {
                cr4: state.cr4 | PCIDE,
                ..state
            }
            .validate_physical(Mode::Protected)
            .is_err()
        );
    }
    let enabled = State {
        cr4: fixed.cr4 | PCIDE,
        ..fixed
    };
    for mode in [Mode::Long64, Mode::Compatibility] {
        for physical in [false, true] {
            let result = if physical {
                enabled.prepare_physical_cr_write(0, 0x33, mode)
            } else {
                enabled.prepare_cr_write(0, 0x33, mode)
            };
            assert_eq!(result, Err(ControlFailure::GeneralProtection));
        }
    }
}

#[test]
fn cr3_pcid_and_no_flush_are_distinct_from_stored_raw_image() {
    let fixed = State::default();
    let enabled = State {
        cr4: fixed.cr4 | PCIDE,
        ..fixed
    };
    for pcid in [0, 1, 8, 0x10, 0x20, 0xabc, 0xfff] {
        let word = 0x000f_ffff_ffff_f000 | pcid;
        for hint in [0, NO_FLUSH] {
            let next = enabled
                .prepare_cr_write(3, word | hint, Mode::Long64)
                .unwrap();
            assert_eq!(next.cr3, word);
            assert_eq!(next.read_cr(3), Ok(word));
            assert_eq!(next.validate(), Ok(()));
            assert_eq!(next.validate_physical(Mode::Long64), Ok(()));
        }
    }
    assert_eq!(
        fixed.prepare_cr_write(3, NO_FLUSH, Mode::Long64),
        Err(ControlFailure::GeneralProtection)
    );
    for bit in 52..=60 {
        assert_eq!(
            enabled.prepare_cr_write(3, NO_FLUSH | (1 << 61) | (1 << bit), Mode::Long64),
            Err(ControlFailure::GeneralProtection)
        );
    }
    for bit in [61, 62] {
        assert_eq!(
            enabled.prepare_cr_write(3, NO_FLUSH | (1 << bit), Mode::Long64),
            Err(ControlFailure::Unsupported)
        );
    }
    assert!(
        State {
            cr3: NO_FLUSH,
            ..enabled
        }
        .validate()
        .is_err()
    );
    assert!(
        State {
            cr3: NO_FLUSH,
            ..enabled
        }
        .validate_physical(Mode::Long64)
        .is_err()
    );
}

#[test]
fn pcide_disable_raw_cr3_survives_family_exit_until_an_actual_cr3_write() {
    let fixed = State::default();
    let enabled = State {
        cr3: 0x5abc,
        cr4: fixed.cr4 | PCIDE,
        ..fixed
    };
    let disabled = enabled
        .prepare_physical_cr_write(4, fixed.cr4, Mode::Compatibility)
        .unwrap();
    assert_eq!(disabled.cr3, 0x5abc);
    let off = disabled
        .prepare_physical_cr_write(0, 0x33, Mode::Compatibility)
        .unwrap();
    assert_eq!(off.cr3, 0x5abc);
    assert_eq!(off.efer, 0x100);
    assert_eq!(off.validate_physical(Mode::Protected), Ok(()));
    let legacy = off.prepare_physical_efer_write(0).unwrap();
    assert_eq!(legacy.cr3, 0x5abc);
    assert_eq!(legacy.validate_physical(Mode::Protected), Ok(()));
    let pae = legacy
        .prepare_physical_cr_write(3, legacy.cr3, Mode::Protected)
        .unwrap();
    assert_eq!(pae.cr3, 0x5aa0);
    let plain = State {
        cr4: 0x608,
        ..legacy
    }
    .prepare_physical_cr_write(3, legacy.cr3, Mode::Protected)
    .unwrap();
    assert_eq!(plain.cr3, 0x5018);
    let ia32e = disabled
        .prepare_physical_cr_write(3, disabled.cr3, Mode::Compatibility)
        .unwrap();
    assert_eq!(ia32e.cr3, 0x5018);
}

#[test]
fn raw_snapshot_admission_does_not_normalize_ignored_cr3_bits_or_emulate_enabling() {
    for low in 0..=0xfff {
        for cr4 in [0x628, 0x628 | PGE, 0x628 | PCIDE, 0x628 | PGE | PCIDE] {
            let snapshot = State {
                cr3: 0x000f_ffff_ffff_f000 | low,
                cr4,
                ..State::default()
            };
            assert_eq!(snapshot.validate(), Ok(()));
            assert_eq!(snapshot.validate_physical(Mode::Long64), Ok(()));
            assert_eq!(
                snapshot.pcid(),
                if cr4 & PCIDE == 0 { 0 } else { low as u16 }
            );
            assert_eq!(snapshot.read_cr(3), Ok(0x000f_ffff_ffff_f000 | low));
        }
    }
}
