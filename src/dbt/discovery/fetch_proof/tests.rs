use super::super::{discover_with_profile, tests::context};
use crate::{
    dbt::{Limits, SourceRange, unit::Unit},
    memory::{Access, Image, MappingInput, MemoryLimits, Permissions},
    x86::{
        paging::{
            State,
            cache::Cache,
            walk::{self, AccessClass, Request},
        },
        protection_keys::State as Keys,
    },
};

fn ram() -> Image {
    let length = 0xc000;
    Image::physical_mapped(
        vec![("ram".into(), vec![0; length])],
        vec![MappingInput {
            base: 0,
            backing: "ram".into(),
            offset: 0,
            length: length as u32,
            permissions: Permissions {
                read: true,
                write: true,
                execute: true,
            },
        }],
        MemoryLimits {
            max_backing_bytes: length as u32,
            max_backings: 1,
            max_mappings: 1,
        },
    )
    .unwrap()
}

fn legacy() -> (Image, State, Cache) {
    let mut image = ram();
    image.physical_write(0x1000, 4, 0x2007).unwrap();
    image.physical_write(0x2010, 4, 0x8007).unwrap();
    image.physical_write(0x2014, 4, 0x9007).unwrap();
    // MOV EAX, imm32 straddles the two pages. HLT ends exploration.
    image.physical_write(0x8ffe, 2, 0x11b8).unwrap();
    image.physical_write(0x9000, 4, 0xf444_3322).unwrap();
    (
        image,
        State {
            cr0: 0x8001_0011,
            cr3: 0x1000,
            ..State::reset()
        },
        Cache::default(),
    )
}

fn discover(image: &Image, state: &State, cache: &Cache, pc: u64, max_bytes: u32) -> Unit {
    discover_with_profile(
        image,
        &context(0),
        Some(state),
        Some(cache),
        pc,
        Limits {
            max_instructions: 32,
            max_source_bytes: max_bytes,
            max_artifact_bytes: 65536,
        },
    )
    .unwrap()
}

const USER_EXEC: Request = Request {
    linear: 0x4000,
    access: Access::Execute,
    class: AccessClass::User,
};

#[cfg(not(feature = "parallel"))]
#[test]
fn guarded_dword_walk_import_requires_a_small_executable_leaf_and_keeps_the_cap_fallback() {
    use crate::{dbt::wasm::emit_profile, x86::context::ExecutionContext};

    let mut image = ram();
    let state = State {
        cr3: 0x1000,
        ..State::default()
    };
    let cache = Cache::default();
    let pc = 0x8000;
    let code = [
        0x8b, 0x07, 0x8b, 0x4f, 4, 0x8b, 0x57, 8, 0x8b, 0x1e, 0x83, 0xed, 1, 0x75, 0xf1,
    ];
    image.physical_write_bytes(pc, &code).unwrap();
    for (address, word) in [
        (0x1000, 0x2007),
        (0x2000, 0x3007),
        (0x3000, 0x4007),
        (0x4040, 0x8007),
    ] {
        image.physical_write(address, 8, word).unwrap();
    }
    let discover = |image: &Image| {
        discover_with_profile(
            image,
            &ExecutionContext::default(),
            Some(&state),
            Some(&cache),
            pc,
            Limits {
                max_instructions: 32,
                max_source_bytes: code.len() as u32,
                max_artifact_bytes: 65536,
            },
        )
        .unwrap()
    };
    let small = discover(&image);
    image.physical_write(0x3000, 8, 0x87).unwrap();
    let mut wide = discover(&image);
    assert!(small.fetch_proof.leaf_is_4k(pc));
    assert!(!wide.fetch_proof.leaf_is_4k(pc));
    assert_eq!(small.instructions, wide.instructions);

    let with_wide_proof = emit_profile(&wide, true, true, usize::MAX);
    let tight_cap = with_wide_proof.len() - 1;
    let wide_tight = emit_profile(&wide, true, true, tight_cap);
    wide.fetch_proof = Default::default();
    assert_eq!(emit_profile(&wide, true, true, usize::MAX), with_wide_proof);
    assert_eq!(emit_profile(&wide, true, true, tight_cap), wide_tight);
    assert_eq!(emit_profile(&small, true, true, tight_cap), wide_tight);
    let mut with_small_proof = emit_profile(&small, true, true, usize::MAX);
    let walk = b"paged_loop_walk_dword";
    let start = with_small_proof
        .windows(walk.len())
        .position(|part| part == walk)
        .unwrap();
    with_small_proof[start..start + walk.len()].copy_from_slice(b"paged_loop_read_dword");
    assert_eq!(with_small_proof, with_wide_proof);
}

#[test]
fn binding_preserves_linear_page_correspondence_and_every_accepted_walk() {
    let (mut image, state, cache) = legacy();
    let unit = discover(&image, &state, &cache, 0x4ffe, 6);
    assert_eq!(unit.fetch_proof.pages.len(), 2);
    assert_eq!(
        unit.read_dependencies,
        [
            SourceRange {
                start: 0x1000,
                length: 4
            },
            SourceRange {
                start: 0x2010,
                length: 8
            },
        ]
    );
    assert!(unit.fetch_proof.matches(&image, &state, &cache, 0));
    // The set of executable physical pages and the walk entry addresses have
    // not changed. Each address now selects the other page's instruction bytes.
    image.physical_write(0x2010, 4, 0x9007).unwrap();
    image.physical_write(0x2014, 4, 0x8007).unwrap();
    assert!(!unit.fetch_proof.matches(&image, &state, &cache, 0));
    // Remapping through an equivalent new page table invalidates the original
    // read dependencies even when the original instruction bytes have returned.
    image.physical_write(0x1000, 4, 0x3007).unwrap();
    image.physical_write(0x3010, 4, 0x8007).unwrap();
    image.physical_write(0x3014, 4, 0x9007).unwrap();
    assert!(!unit.fetch_proof.matches(&image, &state, &cache, 0));
}

#[test]
fn failed_decodes_and_over_budget_targets_do_not_add_pages() {
    let (mut image, state, cache) = legacy();
    // JZ 0x5000; HLT. The taken target is a six-byte near branch.
    image.physical_write(0x8ffc, 4, 0x90f4_0274).unwrap();
    image
        .physical_write(0x9000, 8, 0x0000_0000_0000_840f)
        .unwrap();
    for max_bytes in [3, 32] {
        if max_bytes == 32 {
            // Sixteen consecutive operand-size prefixes exceed the decode limit.
            for addr in [0x9000, 0x9008] {
                image
                    .physical_write(addr, 8, 0x6666_6666_6666_6666)
                    .unwrap();
            }
        }
        let unit = discover(&image, &state, &cache, 0x4ffc, max_bytes);
        assert_eq!(unit.fetch_proof.pages.len(), 1);
        assert_eq!(unit.fetch_proof.pages[0].linear, 0x4000);
        assert_eq!(
            unit.read_dependencies.last(),
            Some(&SourceRange {
                start: 0x2010,
                length: 4
            })
        );
        assert!(unit.fetch_proof.matches(&image, &state, &cache, 3));
    }
}

#[test]
fn walks_with_the_same_set_of_entries_but_a_different_order_do_not_match() {
    let mut image = ram();
    let state = State {
        cr3: 0x1000,
        ..State::default()
    };
    let cache = Cache::default();
    for (addr, value) in [
        (0x1000, 0x2007),
        (0x2000, 0x3007),
        (0x3000, 0x4007),
        (0x4020, 0x8007),
    ] {
        image.physical_write(addr, 8, value).unwrap();
    }
    image.physical_write(0x8000, 1, 0xf4).unwrap();
    let unit = discover_with_profile(
        &image,
        &crate::x86::context::ExecutionContext::default(),
        Some(&state),
        Some(&cache),
        0x4000,
        Limits {
            max_instructions: 1,
            max_source_bytes: 1,
            max_artifact_bytes: 65536,
        },
    )
    .unwrap();
    assert!(unit.fetch_proof.matches(&image, &state, &cache, 0));
    for (addr, value) in [(0x1000, 0x3007), (0x3000, 0x2007), (0x2000, 0x4007)] {
        image.physical_write(addr, 8, value).unwrap();
    }
    let current = cache.peek(&image, &state, USER_EXEC, &Keys::ZERO).unwrap();
    assert_eq!(current.physical, 0x8000);
    assert_eq!(current.entries(), [0x1000, 0x3000, 0x2000, 0x4020]);
    assert!(!unit.fetch_proof.matches(&image, &state, &cache, 0));
}

#[test]
fn proof_observes_stale_effective_translation_and_never_performs_a_fetch() {
    let (mut image, state, cache) = legacy();
    image.physical_write(0x8000, 1, 0xf4).unwrap();
    let warm = walk::walk(&mut image, &state, USER_EXEC, &Keys::ZERO).unwrap();
    cache.fill(&state, USER_EXEC, warm);
    // The unit has yet to be discovered. The guest rewrites the table without
    // a flush, so the TLB's retained executable mapping still owns discovery.
    image.physical_write(0x2010, 4, 0x9007).unwrap();
    let unit = discover(&image, &state, &cache, 0x4000, 1);
    assert_eq!(unit.fetch_proof.pages[0].physical, 0x8000);
    assert!(unit.fetch_proof.matches(&image, &state, &cache, 3));
    cache.invalidate_pcid(0);
    assert!(!unit.fetch_proof.matches(&image, &state, &cache, 3));
    assert_eq!(image.physical_read(0x2010, 4).unwrap(), 0x9007);
    assert!(
        cache
            .observe(&state, USER_EXEC, &Keys::ZERO)
            .unwrap()
            .is_none()
    );
    image.physical_write(0x2010, 4, 0x8003).unwrap();
    assert!(unit.fetch_proof.matches(&image, &state, &cache, 0));
    assert!(!unit.fetch_proof.matches(&image, &state, &cache, 3));
    assert_eq!(image.physical_read(0x2010, 4).unwrap(), 0x8003);
    image.physical_write(0x2010, 4, 0).unwrap();
    assert!(!unit.fetch_proof.matches(&image, &state, &cache, 0));
}

#[test]
fn pae_keeps_nx_and_smep_live_and_captures_wide_entry_provenance() {
    let mut image = ram();
    let state = State {
        cr0: 0x8001_0011,
        cr3: 0x3000,
        cr4: 0x20,
        efer: 0x800,
        pdpte: Some([0x1001, 0, 0, 0]),
        ..State::reset()
    };
    let cache = Cache::default();
    image.physical_write(0x1000, 8, 0x2007).unwrap();
    image.physical_write(0x2020, 8, 0x8007).unwrap();
    image.physical_write(0x8000, 1, 0xf4).unwrap();
    let unit = discover(&image, &state, &cache, 0x4000, 1);
    assert_eq!(unit.fetch_proof.pages[0].entry_width, 8);
    assert_eq!(unit.fetch_proof.pages[0].entries(), [0x1000, 0x2020]);
    assert!(unit.fetch_proof.matches(&image, &state, &cache, 3));
    let smep = State {
        cr4: state.cr4 | 1 << 20,
        ..state
    };
    assert!(!unit.fetch_proof.matches(&image, &smep, &cache, 0));
    assert!(unit.fetch_proof.matches(&image, &smep, &cache, 3));
    image
        .physical_write(0x2020, 8, 0x8000_0000_0000_8007)
        .unwrap();
    assert!(!unit.fetch_proof.matches(&image, &state, &cache, 3));
    assert_eq!(image.physical_read(0x1000, 8).unwrap(), 0x2007);
    assert_eq!(
        image.physical_read(0x2020, 8).unwrap(),
        0x8000_0000_0000_8007
    );
}
