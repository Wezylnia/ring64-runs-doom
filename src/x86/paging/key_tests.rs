use super::State;
use super::access::{AccessState, Memory};
use super::cache::Cache;
use super::walk::{self, AccessClass, Failure, PageFault, Request, Rights};
use crate::memory::{Access, Image, MappingInput, MemoryLimits, Permissions};
use crate::x86::access::GuestMemory;
use crate::x86::context::AccessFailure;
use crate::x86::protection_keys::State as Keys;

fn image() -> Image {
    Image::physical_mapped(
        vec![("ram".into(), vec![0; 0x10000])],
        vec![MappingInput {
            base: 0,
            backing: "ram".into(),
            offset: 0,
            length: 0x10000,
            permissions: Permissions {
                read: true,
                write: true,
                execute: true,
            },
        }],
        MemoryLimits {
            max_backing_bytes: 0x10000,
            max_backings: 1,
            max_mappings: 1,
        },
    )
    .unwrap()
}

fn fixture(first: u64, second: u64) -> (Image, State, AccessState, Cache) {
    let mut image = image();
    for (address, value) in [
        (0x1000, 0x2027),
        (0x2000, 0x3027),
        (0x3000, 0x4027),
        (0x4008, first),
        (0x4010, second),
    ] {
        image.physical_write(address, 8, value).unwrap();
    }
    image.physical_write(0x9000, 1, 0xa1).unwrap();
    image.physical_write(0xa000, 1, 0xb2).unwrap();
    (
        image,
        State {
            cr3: 0x1000,
            ..State::default()
        },
        AccessState::default(),
        Cache::default(),
    )
}

fn request(access: Access, class: AccessClass) -> Request {
    Request {
        linear: 0x1000,
        access,
        class,
    }
}

fn page_fault(address: u64, access: Access, error_code: u32) -> Failure {
    Failure::Page(PageFault {
        address,
        access,
        error_code,
    })
}

#[test]
fn all_key_pairs_match_independent_direction_and_requester_literals() {
    // Columns: user read/write, supervisor read/write-WP0/write-WP1, execute.
    const DENIED: [[bool; 6]; 4] = [
        [false, false, false, false, false, false],
        [true, true, true, true, true, false],
        [false, true, false, false, true, false],
        [true, true, true, true, true, false],
    ];
    // Columns select the effective user/supervisor address, not the requester.
    const ENABLED: [[bool; 2]; 4] = [[false, false], [true, false], [false, true], [true, true]];
    for key in 0..16 {
        for (pair, denied) in DENIED.iter().enumerate() {
            for user_page in [false, true] {
                for (control, bits) in [0, 1 << 22, 1 << 24, (1 << 22) | (1 << 24)]
                    .into_iter()
                    .enumerate()
                {
                    for wp in [false, true] {
                        let state = State {
                            cr0: State::default().cr0 | if wp { 1 << 16 } else { 0 },
                            cr4: State::default().cr4 | bits,
                            ..State::default()
                        };
                        let selected = (pair as u32) << (key * 2);
                        let other = ((3 - pair) as u32) << (key * 2);
                        let keys = if user_page {
                            Keys {
                                pkru: selected,
                                pkrs: other,
                            }
                        } else {
                            Keys {
                                pkru: other,
                                pkrs: selected,
                            }
                        };
                        for class in [
                            AccessClass::User,
                            AccessClass::Supervisor { ac: false },
                            AccessClass::Supervisor { ac: true },
                            AccessClass::ImplicitSupervisor,
                        ] {
                            for access in [Access::Read, Access::Write, Access::Execute] {
                                let user_request = class == AccessClass::User;
                                let column = match (access, user_request, wp) {
                                    (Access::Execute, _, _) => 5,
                                    (Access::Read, true, _) => 0,
                                    (Access::Write, true, _) => 1,
                                    (Access::Read, false, _) => 2,
                                    (Access::Write, false, false) => 3,
                                    (Access::Write, false, true) => 4,
                                };
                                let key_denied =
                                    ENABLED[control][usize::from(!user_page)] && denied[column];
                                let ordinary_denied = user_request && !user_page;
                                let expected = if ordinary_denied || key_denied {
                                    Err(page_fault(
                                        0x1000,
                                        access,
                                        1 | (u32::from(access == Access::Write) << 1)
                                            | (u32::from(user_request) << 2)
                                            | (u32::from(key_denied) << 5),
                                    ))
                                } else {
                                    Ok(())
                                };
                                assert_eq!(
                                    Rights {
                                        user: user_page,
                                        writable: true,
                                        executable: true,
                                        key,
                                    }
                                    .authorize(
                                        &state,
                                        request(access, class),
                                        &keys
                                    ),
                                    expected,
                                    "key{key} pair{pair} user-page{user_page} control{control} WP{wp} {class:?} {access:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn leaf_keys_are_captured_while_disabled_and_nonleaf_bits_do_not_select_them() {
    for shift in [12, 21, 30] {
        for key in [0, 7, 15] {
            let (mut image, state, _, _) = fixture(0x9067, 0);
            for address in [0x1000, 0x2000, 0x3000] {
                let old = image.physical_read(address, 8).unwrap();
                image.physical_write(address, 8, old | (15 << 59)).unwrap();
            }
            let (leaf, frame) = match shift {
                12 => (0x4008, 0x9000),
                21 => (0x3000, 0x200000),
                _ => (0x2000, 0x40000000),
            };
            let flags = if shift == 12 { 0x67 } else { 0xe7 };
            image
                .physical_write(leaf, 8, frame | flags | (key << 59))
                .unwrap();
            let translated = walk::peek(
                &image,
                &state,
                request(Access::Read, AccessClass::User),
                &Keys {
                    pkru: u32::MAX,
                    pkrs: u32::MAX,
                },
            )
            .unwrap();
            assert_eq!(translated.rights.key, key as u8);
            assert_eq!(translated.span, 1 << shift);
            assert_eq!(
                translated.physical,
                frame | if shift == 12 { 0 } else { 0x1000 }
            );
        }
    }
}

#[test]
fn physical_table_reads_and_ad_writes_bypass_denied_linear_aliases() {
    let (mut image, mut state, mut work, cache) = fixture(0, 0);
    for (address, value) in [(0x1000, 0x2007), (0x2000, 0x3007), (0x3000, 0x4007)] {
        image.physical_write(address, 8, value).unwrap();
    }
    for index in 1..=4 {
        image
            .physical_write(0x4000 + index * 8, 8, (index * 0x1000) | (7 << 59) | 7)
            .unwrap();
    }
    image.physical_write(0x4028, 8, 0x9007 | (3 << 59)).unwrap();
    state.cr4 |= (1 << 22) | (1 << 24);
    state.cr0 |= 1 << 16;
    let keys = Keys {
        pkru: 3 << 14,
        pkrs: 3 << 14,
    };
    for linear in [0x1000, 0x2000, 0x3000, 0x4000] {
        assert_eq!(
            walk::peek(
                &image,
                &state,
                Request {
                    linear,
                    access: Access::Read,
                    class: AccessClass::User,
                },
                &keys,
            ),
            Err(page_fault(linear, Access::Read, 0x25)),
            "the table's linear alias is denied"
        );
    }
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work, &keys)
            .with_cache(&cache)
            .read(0x5000, 1),
        Ok(0xa1),
        "cold physical table reads and A writes bypass linear key rights"
    );
    for (address, value) in [
        (0x1000, 0x2027),
        (0x2000, 0x3027),
        (0x3000, 0x4027),
        (0x4028, 0x9027 | (3 << 59)),
    ] {
        assert_eq!(image.physical_read(address, 8), Ok(value));
    }
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work, &keys)
            .with_cache(&cache)
            .write(0x5000, 1, 0x5c),
        Ok(()),
        "physical D publication bypasses the denied table alias"
    );
    assert_eq!(image.physical_read(0x4028, 8), Ok(0x9067 | (3 << 59)));
    assert_eq!(image.physical_read(0x9000, 1), Ok(0x5c));
    for index in 1..=4 {
        assert_eq!(
            image.physical_read(0x4000 + index * 8, 8),
            Ok((index * 0x1000) | (7 << 59) | 7),
            "physical access does not touch its linear alias PTE"
        );
    }
}

#[test]
fn retained_key_uses_live_bank_without_walk_until_invalidation_or_fault_eviction() {
    let (mut image, mut state, mut work, cache) = fixture(0x9067 | (3 << 59), 0);
    let mut keys = Keys::ZERO;
    let read = request(Access::Read, AccessClass::User);
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work, &keys)
            .with_cache(&cache)
            .read(0x1000, 1),
        Ok(0xa1)
    );
    let id = cache
        .observe(&state, read, &keys)
        .unwrap()
        .unwrap()
        .cache_id;
    image.physical_write(0x4008, 8, 0x9067 | (7 << 59)).unwrap();
    state.cr4 |= 1 << 22;
    keys.pkru = 1 << 14;
    walk::physical_counts::take();
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work, &keys)
            .with_cache(&cache)
            .read(0x1000, 1),
        Ok(0xa1)
    );
    assert_eq!(walk::physical_counts::take(), [0; 6]);
    assert_eq!(
        cache
            .observe(&state, read, &keys)
            .unwrap()
            .unwrap()
            .cache_id,
        id
    );
    keys.pkru = 1 << 6;
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work, &keys)
            .with_cache(&cache)
            .read(0x1000, 1),
        Err(AccessFailure::Paging(page_fault(
            0x1000,
            Access::Read,
            0x25
        )))
    );
    assert!(cache.observe(&state, read, &keys).unwrap().is_none());
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work, &keys)
            .with_cache(&cache)
            .read(0x1000, 1),
        Ok(0xa1)
    );
    assert_eq!(
        cache
            .observe(&state, read, &keys)
            .unwrap()
            .unwrap()
            .rights
            .key,
        7
    );
    keys.pkru = 0;
    image.physical_write(0x4008, 8, 0x9067 | (3 << 59)).unwrap();
    assert_eq!(
        cache
            .observe(&state, read, &keys)
            .unwrap()
            .unwrap()
            .rights
            .key,
        7
    );
    cache.invalidate_page(0, 0x1000, true);
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work, &keys)
            .with_cache(&cache)
            .read(0x1000, 1),
        Ok(0xa1)
    );
    assert_eq!(
        cache
            .observe(&state, read, &keys)
            .unwrap()
            .unwrap()
            .rights
            .key,
        3
    );
}

#[test]
fn d0_write_refresh_keeps_its_inherited_key_visibility_policy() {
    let (mut image, mut state, mut work, cache) = fixture(0x9027 | (3 << 59), 0);
    let mut keys = Keys::ZERO;
    Memory::new(&mut image, &state, 3, 2, &mut work, &keys)
        .with_cache(&cache)
        .read(0x1000, 1)
        .unwrap();
    image.physical_write(0x4008, 8, 0x9067 | (7 << 59)).unwrap();
    state.cr4 |= 1 << 22;
    keys.pkru = 1 << 6;
    assert_eq!(
        Memory::new(&mut image, &state, 3, 2, &mut work, &keys)
            .with_cache(&cache)
            .write(0x1000, 1, 0x5c),
        Ok(())
    );
    assert_eq!(image.physical_read(0x9000, 1).unwrap(), 0x5c);
    assert_eq!(
        cache
            .observe(&state, request(Access::Read, AccessClass::User), &keys)
            .unwrap()
            .unwrap()
            .rights
            .key,
        7
    );
}

#[test]
fn later_key_fault_precedes_ac_and_preserves_all_payload_and_dirty_bits() {
    let first = 0x9027 | (3 << 59);
    let second = 0xa027 | (7 << 59);
    let (mut image, mut state, mut work, _) = fixture(first, second);
    state.cr4 |= 1 << 22;
    state.cr0 |= 1 << 18;
    let keys = Keys {
        pkru: 1 << 14,
        pkrs: 0,
    };
    image.physical_write(0x9fff, 1, 0xc3).unwrap();
    let result =
        Memory::new(&mut image, &state, 3, 0x40002, &mut work, &keys).write(0x1fff, 4, 0x44332211);
    assert_eq!(
        result,
        Err(AccessFailure::Paging(page_fault(
            0x2000,
            Access::Write,
            0x27
        )))
    );
    assert_eq!(image.physical_read(0x9fff, 1).unwrap(), 0xc3);
    assert_eq!(image.physical_read(0xa000, 1).unwrap(), 0xb2);
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), first);
    assert_eq!(image.physical_read(0x4010, 8).unwrap(), second);
}

#[test]
fn implicit_metadata_obeys_keys_while_fetch_and_inactive_families_do_not() {
    let (mut image, mut state, mut work, _) = fixture(0x9067 | (15 << 59), 0);
    state.cr4 |= 1 << 22;
    let keys = Keys {
        pkru: 1 << 30,
        pkrs: 0,
    };
    {
        let memory = Memory::new(&mut image, &state, 0, 0x40002, &mut work, &keys);
        assert_eq!(
            memory.read_system(0x1000, 1),
            Err(AccessFailure::Paging(page_fault(
                0x1000,
                Access::Read,
                0x21
            )))
        );
        assert_eq!(memory.fetch(0x1000), Ok(0xa1));
    }
    for (cr0, efer) in [(0x60000010, 0), (0x80000033, 0)] {
        state.cr0 = cr0;
        state.efer = efer;
        assert_eq!(
            Rights {
                user: true,
                writable: true,
                executable: true,
                key: 15
            }
            .authorize(&state, request(Access::Read, AccessClass::User), &keys),
            Ok(())
        );
    }
}
