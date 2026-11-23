mod legacy;
mod supervisor;
mod unpaged;

use super::*;
use crate::memory::{MappingInput, MemoryLimits, Permissions};

fn fixture() -> (Image, State) {
    let mut bytes = vec![0; 0x5000];
    for (offset, word) in [
        (0x1000, 0x2007u64),
        (0x2000, 0x3007),
        (0x3000, 0x4007),
        (0x4008, 0xa007),
    ] {
        bytes[offset..offset + 8].copy_from_slice(&word.to_le_bytes());
    }
    let image = Image::physical_mapped(
        vec![("tables".into(), bytes)],
        vec![MappingInput {
            base: 0,
            backing: "tables".into(),
            offset: 0,
            length: 0x5000,
            permissions: Permissions {
                read: true,
                write: true,
                execute: false,
            },
        }],
        MemoryLimits {
            max_backing_bytes: 0x5000,
            max_backings: 1,
            max_mappings: 1,
        },
    )
    .unwrap();
    (
        image,
        State {
            cr3: 0x1000,
            ..State::default()
        },
    )
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_upper_walk_scratch_is_independent_and_leaves_the_leaf_live() {
    let (mut first, state) = fixture();
    let (mut second, _) = fixture();
    second.physical_write(0x3000, 8, 0x27).unwrap();
    second.physical_write(8, 8, 0xb027).unwrap();
    let request = Request {
        linear: 0x1234,
        access: Access::Read,
        class: AccessClass::User,
    };
    let keys = &ProtectionKeys::ZERO;
    let mut first_words = UpperWords::default();
    let mut second_words = UpperWords::default();
    physical_counts::take();
    assert_eq!(
        first_words
            .peek(&first, &state, request, keys)
            .unwrap()
            .physical,
        0xa234
    );
    assert_eq!(
        second_words
            .peek(&second, &state, request, keys)
            .unwrap()
            .physical,
        0xb234
    );
    assert_eq!(physical_counts::take(), [8, 64, 0, 0, 0, 0]);
    first.physical_write(0x4008, 8, 0xc027).unwrap();
    assert_eq!(
        first_words
            .peek(&first, &state, request, keys)
            .unwrap()
            .physical,
        0xc234
    );
    assert_eq!(
        second_words
            .peek(&second, &state, request, keys)
            .unwrap()
            .physical,
        0xb234
    );
    assert_eq!(physical_counts::take(), [2, 16, 0, 0, 0, 0]);
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_leaf_table_matches_independent_walks_for_live_leaf_changes() {
    let (mut image, state) = fixture();
    image.physical_write(0x1000, 8, 0x2025).unwrap();
    image.physical_write(0x2000, 8, 0x3027).unwrap();
    image.physical_write(0x3000, 8, 0x4027).unwrap();
    let read = Request {
        linear: 0x1234,
        access: Access::Read,
        class: AccessClass::User,
    };
    for efer in [state.efer, state.efer | (1 << 11)] {
        let state = State { efer, ..state };
        let mut words = UpperWords::default();
        let keys = &ProtectionKeys::ZERO;
        assert_eq!(
            words.peek(&image, &state, read, keys),
            peek(&image, &state, read, keys)
        );
        assert!(words.ia32e.is_some());
        for leaf in [0u64, 0xb001, 0xb125, 0xb167, (1 << 63) | 0xb027] {
            image.physical_write(0x4010, 8, leaf).unwrap();
            for access in [Access::Read, Access::Write, Access::Execute] {
                let request = Request {
                    linear: 0x2345,
                    access,
                    ..read
                };
                assert_eq!(
                    words.peek(&image, &state, request, keys),
                    peek(&image, &state, request, keys)
                );
            }
        }
    }
}

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_leaf_word_split_between_physical_mappings_uses_checked_read() {
    let (_, state) = fixture();
    let mut bytes = vec![0; 0x5000];
    for (address, word) in [
        (0x1000, 0x2027u64),
        (0x2000, 0x3027),
        (0x3000, 0x4027),
        (0x4008, 0xa027),
        (0x4010, 0xb027),
    ] {
        bytes[address..address + 8].copy_from_slice(&word.to_le_bytes());
    }
    let split = 0x4014;
    let before = MappingInput {
        base: 0,
        backing: "tables".into(),
        offset: 0,
        length: split,
        permissions: Permissions {
            read: true,
            write: false,
            execute: false,
        },
    };
    let after = MappingInput {
        base: u64::from(split),
        offset: split,
        length: 0x5000 - split,
        ..before.clone()
    };
    let mut image = Image::physical_mapped(
        vec![("tables".into(), bytes)],
        vec![before.clone(), after],
        MemoryLimits {
            max_backing_bytes: 0x5000,
            max_backings: 1,
            max_mappings: 2,
        },
    )
    .unwrap();
    let request = Request {
        linear: 0x1234,
        access: Access::Read,
        class: AccessClass::User,
    };
    let keys = &ProtectionKeys::ZERO;
    let mut words = UpperWords::default();
    assert_eq!(
        words.peek(&image, &state, request, keys).unwrap().physical,
        0xa234
    );
    assert!(words.ia32e.is_some());

    let request = Request {
        linear: 0x2345,
        ..request
    };
    assert_eq!(
        words.peek(&image, &state, request, keys).unwrap().physical,
        0xb345
    );
    image.set_mappings(vec![before]).unwrap();
    let missing = peek(&image, &state, request, keys).unwrap_err();
    assert_eq!(words.peek(&image, &state, request, keys), Err(missing));
}

#[test]
fn literal_nonidentity_walk_peeks_without_accessed_effects() {
    let (image, state) = fixture();
    let request = Request {
        linear: 0x1234,
        access: Access::Read,
        class: AccessClass::User,
    };
    let result = peek(&image, &state, request, &ProtectionKeys::ZERO).unwrap();
    assert_eq!(
        (result.physical, result.leaf, result.dirty),
        (0xa234, 0x4008, true)
    );
    assert_eq!(result.entries(), &[0x1000, 0x2000, 0x3000, 0x4008]);
    assert_eq!(image.physical_read(0x1000, 8).unwrap(), 0x2007);
}

#[test]
fn actual_walk_sets_only_visited_valid_accessed_bits_before_nonpresent() {
    let (mut image, state) = fixture();
    let failure = walk(
        &mut image,
        &state,
        Request {
            linear: 0x2000,
            access: Access::Write,
            class: AccessClass::User,
        },
        &ProtectionKeys::ZERO,
    )
    .unwrap_err();
    assert_eq!(
        failure,
        Failure::Page(PageFault {
            address: 0x2000,
            access: Access::Write,
            error_code: 6
        })
    );
    for (address, word) in [(0x1000, 0x2027), (0x2000, 0x3027), (0x3000, 0x4027)] {
        assert_eq!(image.physical_read(address, 8).unwrap(), word);
    }
    assert_eq!(image.physical_read(0x4010, 8).unwrap(), 0);
}

#[test]
fn leaf_permission_and_nx_error_codes_use_literal_access_provenance() {
    for (user, access, wp, nxe, leaf, expected) in [
        (true, Access::Write, true, false, 0xa005, 7),
        (false, Access::Write, true, false, 0xa005, 3),
        (true, Access::Execute, false, true, 0x800000000000a007, 21),
        (true, Access::Read, false, false, 0x800000000000a007, 13),
        (true, Access::Execute, false, true, 0, 20),
        (true, Access::Execute, false, false, 0, 4),
    ] {
        let (mut image, mut state) = fixture();
        state.cr0 |= u64::from(wp) << 16;
        state.efer |= u64::from(nxe) << 11;
        image.physical_write(0x4008, 8, leaf).unwrap();
        assert_eq!(
            walk(
                &mut image,
                &state,
                Request {
                    linear: 0x1234,
                    access,
                    class: AccessClass::operand(if user { 3 } else { 0 }, false),
                },
                &ProtectionKeys::ZERO
            ),
            Err(Failure::Page(PageFault {
                address: 0x1234,
                access,
                error_code: expected
            }))
        );
        assert_eq!(image.physical_read(0x4008, 8).unwrap() & 64, 0);
    }
}

#[test]
fn lower_absence_precedes_upper_permission_and_nonpresent_garbage_is_not_reserved() {
    let (mut image, state) = fixture();
    image.physical_write(0x1000, 8, 0x2001).unwrap();
    image.physical_write(0x4008, 8, 0xfffffffffffffffe).unwrap();
    assert_eq!(
        walk(
            &mut image,
            &state,
            Request {
                linear: 0x1234,
                access: Access::Read,
                class: AccessClass::User,
            },
            &ProtectionKeys::ZERO
        ),
        Err(Failure::Page(PageFault {
            address: 0x1234,
            access: Access::Read,
            error_code: 4
        }))
    );
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0xfffffffffffffffe);
}

#[test]
fn huge_leaves_accept_pat_and_ignored_high_bits_but_reject_reserved_address_bits() {
    let (mut image, state) = fixture();
    let request = Request {
        linear: 0x1234,
        access: Access::Read,
        class: AccessClass::User,
    };
    for (entry, word, expected) in [
        (0x2000, 0x7ff0000040001087, 0x40001234),
        (0x3000, 0x7ff0000000201087, 0x201234),
    ] {
        let old = image.physical_read(entry, 8).unwrap();
        image.physical_write(entry, 8, word).unwrap();
        assert_eq!(
            peek(&image, &state, request, &ProtectionKeys::ZERO)
                .unwrap()
                .physical,
            expected
        );
        image.physical_write(entry, 8, word | (1 << 13)).unwrap();
        assert_eq!(
            peek(&image, &state, request, &ProtectionKeys::ZERO),
            Err(Failure::Page(PageFault {
                address: 0x1234,
                access: Access::Read,
                error_code: 13
            }))
        );
        image.physical_write(entry, 8, old).unwrap();
    }
}

#[test]
fn family_specialization_preserves_literal_entry_access_traces_and_geometry() {
    fn trace<const EFFECTS: bool, const FETCH: bool>(
        state: &State,
        words: &[(u64, u64)],
        mut expected: Translation,
        accessed: bool,
        accesses: &[(u64, bool)],
    ) {
        let mut words: std::collections::HashMap<_, _> = words.iter().copied().collect();
        let mut actual = Vec::new();
        let translated = walk_with::<EFFECTS, FETCH>(
            state,
            Request {
                linear: 0x1234,
                access: Access::Read,
                class: AccessClass::User,
            },
            |address, width, accessed| {
                actual.push((address, width, accessed));
                let word = words.get_mut(&address).expect("unexpected table access");
                if accessed {
                    *word |= 0x20;
                }
                Ok(Some(*word))
            },
            &ProtectionKeys::ZERO,
        )
        .unwrap()
        .unwrap();
        expected.accessed = accessed;
        assert_eq!(translated, expected);
        let expected_accesses: Vec<_> = accesses
            .iter()
            .map(|&(address, accessed)| (address, expected.entry_width, accessed))
            .collect();
        assert_eq!(actual, expected_accesses);
    }
    // Trace only the walker's entry-access boundary: an update is one callback,
    // not a claim that its backing preflight/current-word OR is one memory read.
    let cases = [
        (
            [false, true, true],
            State {
                cr3: 0x1000,
                ..State::default()
            },
            vec![
                (0x1000, 0x2007),
                (0x2000, 0x3007),
                (0x3000, 0x4007),
                (0x4008, 0xa007),
            ],
            vec![
                (0x1000, 0x2027),
                (0x2000, 0x3027),
                (0x3000, 0x4027),
                (0x4008, 0xa027),
            ],
            Translation {
                physical: 0xa234,
                leaf: 0x4008,
                dirty: true,
                remaining: 0xdcc,
                entry_width: 8,
                entries: [0x1000, 0x2000, 0x3000, 0x4008],
                count: 4,
                linear_base: 0x1000,
                physical_base: 0xa000,
                span: 0x1000,
                rights: Rights {
                    user: true,
                    writable: true,
                    executable: true,
                    key: 0,
                },
                global: false,
                accessed: false,
                cache_id: None,
            },
            vec![
                (0x1000, false),
                (0x2000, false),
                (0x3000, false),
                (0x4008, false),
            ],
            vec![
                (0x1000, false),
                (0x1000, true),
                (0x2000, false),
                (0x2000, true),
                (0x3000, false),
                (0x3000, true),
                (0x4008, false),
                (0x4008, true),
            ],
            vec![
                (0x1000, false),
                (0x1000, true),
                (0x1000, false),
                (0x2000, false),
                (0x2000, true),
                (0x1000, false),
                (0x2000, false),
                (0x3000, false),
                (0x3000, true),
                (0x1000, false),
                (0x2000, false),
                (0x3000, false),
                (0x4008, false),
                (0x4008, true),
                (0x1000, false),
                (0x2000, false),
                (0x3000, false),
                (0x4008, false),
            ],
        ),
        (
            [false, true, true],
            State {
                cr0: 0x80000033,
                cr2: 0,
                cr3: 0x1020,
                cr4: 0x628,
                efer: 0,
                pdpte: Some([0x3001, 0, 0, 0]),
            },
            vec![(0x3000, 0x4007), (0x4008, 0xa007)],
            vec![(0x3000, 0x4027), (0x4008, 0xa027)],
            Translation {
                physical: 0xa234,
                leaf: 0x4008,
                dirty: true,
                remaining: 0xdcc,
                entry_width: 8,
                entries: [0x3000, 0x4008, 0, 0],
                count: 2,
                linear_base: 0x1000,
                physical_base: 0xa000,
                span: 0x1000,
                rights: Rights {
                    user: true,
                    writable: true,
                    executable: true,
                    key: 0,
                },
                global: false,
                accessed: false,
                cache_id: None,
            },
            vec![(0x3000, false), (0x4008, false)],
            vec![
                (0x3000, false),
                (0x3000, true),
                (0x4008, false),
                (0x4008, true),
            ],
            vec![
                (0x3000, false),
                (0x3000, true),
                (0x3000, false),
                (0x4008, false),
                (0x4008, true),
                (0x3000, false),
                (0x4008, false),
            ],
        ),
        (
            [false, true, true],
            State {
                cr0: 0x80000033,
                cr2: 0,
                cr3: 0x1000,
                cr4: 0x608,
                efer: 0,
                pdpte: None,
            },
            vec![(0x1000, 0x4007), (0x4004, 0xa007)],
            vec![(0x1000, 0x4027), (0x4004, 0xa027)],
            Translation {
                physical: 0xa234,
                leaf: 0x4004,
                dirty: true,
                remaining: 0xdcc,
                entry_width: 4,
                entries: [0x1000, 0x4004, 0, 0],
                count: 2,
                linear_base: 0x1000,
                physical_base: 0xa000,
                span: 0x1000,
                rights: Rights {
                    user: true,
                    writable: true,
                    executable: true,
                    key: 0,
                },
                global: false,
                accessed: false,
                cache_id: None,
            },
            vec![(0x1000, false), (0x4004, false)],
            vec![
                (0x1000, false),
                (0x1000, true),
                (0x4004, false),
                (0x4004, true),
            ],
            vec![
                (0x1000, false),
                (0x1000, true),
                (0x1000, false),
                (0x4004, false),
                (0x4004, true),
                (0x1000, false),
                (0x4004, false),
            ],
        ),
        (
            [false, false, false],
            State {
                cr3: 0xdead000,
                cr4: 0x620,
                efer: 0x100,
                ..State::reset()
            },
            vec![],
            vec![],
            Translation {
                physical: 0x1234,
                leaf: 0,
                dirty: false,
                remaining: 0xffffedcc,
                entry_width: 0,
                entries: [0; 4],
                count: 0,
                linear_base: 0,
                physical_base: 0,
                span: 1u64 << 32,
                rights: Rights {
                    user: true,
                    writable: true,
                    executable: true,
                    key: 0,
                },
                global: false,
                accessed: false,
                cache_id: None,
            },
            vec![],
            vec![],
            vec![],
        ),
    ];
    for (readiness, state, cold, warm, geometry, reads, updates, coherent) in cases {
        trace::<false, false>(&state, &cold, geometry, readiness[0], &reads);
        trace::<true, false>(&state, &cold, geometry, readiness[1], &updates);
        trace::<true, true>(&state, &cold, geometry, readiness[1], &coherent);
        trace::<false, false>(&state, &warm, geometry, readiness[2], &reads);
        trace::<true, false>(&state, &warm, geometry, readiness[2], &reads);
        trace::<true, true>(&state, &warm, geometry, readiness[2], &reads);
    }
}

#[test]
fn actual_physical_walk_calls_have_literal_width_and_effect_counts() {
    fn image(words: &[(u64, u64)], width: usize, warm: bool, length: usize) -> Image {
        let mut bytes = vec![0; length];
        for &(address, word) in words {
            let word = word | if warm { 0x20 } else { 0 };
            bytes[address as usize..address as usize + width]
                .copy_from_slice(&word.to_le_bytes()[..width]);
        }
        Image::physical_mapped(
            vec![("tables".into(), bytes)],
            vec![MappingInput {
                base: 0,
                backing: "tables".into(),
                offset: 0,
                length: length as u32,
                permissions: Permissions {
                    read: true,
                    write: true,
                    execute: false,
                },
            }],
            MemoryLimits {
                max_backings: 1,
                max_mappings: 1,
                max_backing_bytes: length as u32,
            },
        )
        .unwrap()
    }
    // Counts are explicit Image read/read-bytes, write-check/check-bytes,
    // write/write-bytes calls in this owner, not full-engine instrumentation.
    // FETCH uses a no-op encoding-currentness callback. Payload/D work and
    // any nested embedding encoding checks are outside these measurements.
    let cases = [
        (
            State {
                cr3: 0x1000,
                ..State::default()
            },
            0x1234,
            vec![
                (0x1000, 0x2007),
                (0x2000, 0x3007),
                (0x3000, 0x4007),
                (0x4008, 0xa007),
            ],
            8,
            0xa234,
            [4, 32, 0, 0, 0, 0],
            [8, 64, 4, 32, 4, 32],
            [18, 144, 4, 32, 4, 32],
        ),
        (
            State {
                cr0: 0x80000033,
                cr2: 0,
                cr3: 0x1020,
                cr4: 0x628,
                efer: 0,
                pdpte: Some([0x3001, 0, 0, 0]),
            },
            0x1234,
            vec![(0x3000, 0x4007), (0x4008, 0xa007)],
            8,
            0xa234,
            [2, 16, 0, 0, 0, 0],
            [4, 32, 2, 16, 2, 16],
            [7, 56, 2, 16, 2, 16],
        ),
        (
            State {
                cr0: 0x80000033,
                cr2: 0,
                cr3: 0x1000,
                cr4: 0x608,
                efer: 0,
                pdpte: None,
            },
            0x1234,
            vec![(0x1000, 0x4007), (0x4004, 0xa007)],
            4,
            0xa234,
            [2, 8, 0, 0, 0, 0],
            [4, 16, 2, 8, 2, 8],
            [7, 28, 2, 8, 2, 8],
        ),
        (
            State {
                cr0: 0x80000033,
                cr2: 0,
                cr3: 0x1000,
                cr4: 0x618,
                efer: 0,
                pdpte: None,
            },
            0x400004,
            vec![(0x1004, 0x2087)],
            4,
            0x100000004,
            [1, 4, 0, 0, 0, 0],
            [2, 8, 1, 4, 1, 4],
            [3, 12, 1, 4, 1, 4],
        ),
        (
            State {
                cr0: 0x80000033,
                cr2: 0,
                cr3: 0x1020,
                cr4: 0x628,
                efer: 0,
                pdpte: Some([0x3001, 0, 0, 0]),
            },
            0x200004,
            vec![(0x3008, 0x400087)],
            8,
            0x400004,
            [1, 8, 0, 0, 0, 0],
            [2, 16, 1, 8, 1, 8],
            [3, 24, 1, 8, 1, 8],
        ),
        (
            State {
                cr3: 0xdead000,
                cr4: 0x620,
                efer: 0x100,
                ..State::reset()
            },
            0x1234,
            vec![],
            8,
            0x1234,
            [0; 6],
            [0; 6],
            [0; 6],
        ),
    ];
    for (state, linear, words, width, physical, warm, cold, coherent) in cases {
        let request = Request {
            linear,
            access: Access::Read,
            class: AccessClass::User,
        };
        let mut actual = image(&words, width, false, 0x6000);
        physical_counts::take();
        assert_eq!(
            peek(&actual, &state, request, &ProtectionKeys::ZERO)
                .unwrap()
                .physical,
            physical
        );
        assert_eq!(
            physical_counts::take(),
            warm,
            "observational cold table reads"
        );
        assert_eq!(
            walk(&mut actual, &state, request, &ProtectionKeys::ZERO)
                .unwrap()
                .physical,
            physical
        );
        assert_eq!(
            physical_counts::take(),
            cold,
            "actual current-word A effects"
        );
        assert_eq!(
            walk(&mut actual, &state, request, &ProtectionKeys::ZERO)
                .unwrap()
                .physical,
            physical
        );
        assert_eq!(physical_counts::take(), warm, "warm walk skips A updates");
        assert_eq!(
            fetch_walk(
                &mut actual,
                &state,
                request,
                |_| true,
                &ProtectionKeys::ZERO
            )
            .unwrap()
            .unwrap()
            .physical,
            physical
        );
        assert_eq!(
            physical_counts::take(),
            warm,
            "warm FETCH skips hierarchy rereads"
        );
        let mut actual = image(&words, width, false, 0x6000);
        assert_eq!(
            fetch_walk(
                &mut actual,
                &state,
                request,
                |_| true,
                &ProtectionKeys::ZERO
            )
            .unwrap()
            .unwrap()
            .physical,
            physical
        );
        assert_eq!(
            physical_counts::take(),
            coherent,
            "cold FETCH includes hierarchy rereads"
        );
    }
    let quartet = [0x1001, 0xfffffffffffffffe, 0, 0x2001];
    let words: Vec<_> = quartet
        .into_iter()
        .enumerate()
        .map(|(slot, word)| (0x5fe0 + slot as u64 * 8, word))
        .collect();
    let state = State {
        cr3: 0x5fe0,
        ..State::reset()
    };
    let actual = image(&words, 8, false, 0x6000);
    assert_eq!(state.load_pdpte(&actual).unwrap(), quartet);
    assert_eq!(
        physical_counts::take(),
        [4, 32, 0, 0, 0, 0],
        "captured quartet has four exact reads and no A/D"
    );
}
