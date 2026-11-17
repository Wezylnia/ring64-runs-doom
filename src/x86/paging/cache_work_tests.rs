use super::*;
use crate::x86::{instruction::Instruction, paging::walk};

// outcome (0 miss/1 hit/2 uncached), admitted fill, PA, then actual walk read/check/write
// calls and byte widths, then the independently checked payload value.
type Row = [u64; 10];
const A: u64 = 0x1111222233334444;
const B: u64 = 0x5555666677778888;

fn completed_reads(cached: bool) -> (Vec<Row>, Vec<usize>) {
    let (mut image, state, request) = super::tests::fixture();
    image.physical_write(0x4008, 8, 0xa027).unwrap();
    image.physical_write(0xa234, 8, A).unwrap();
    image.physical_write(0xb234, 8, B).unwrap();
    let cache = Cache::default();
    let mut rows = Vec::new();
    let mut removals = Vec::new();
    walk::physical_counts::take();
    for index in 0..6 {
        if index == 3 {
            // Both paths perform the same PTE edit and architectural invalidation before B.
            image.physical_write(0x4008, 8, 0xb007).unwrap();
        }
        if index == 3 || index == 5 {
            let before = cache.entries.borrow().slots.iter().flatten().count();
            if index == 3 {
                cache.invalidate_page(0, 0x1234, true);
            } else {
                cache.invalidate_pcid(0);
            }
            let after = cache.entries.borrow().slots.iter().flatten().count();
            if cached {
                removals.push(before - after);
                assert!(cache.take_changed());
            } else {
                assert_eq!((before, after), (0, 0));
                assert!(!cache.take_changed());
            }
            assert_eq!(
                walk::physical_counts::take(),
                [0; 6],
                "invalidation reads no table or target"
            );
        }
        let retained = if cached {
            cache.lookup(&state, request).unwrap()
        } else {
            None
        };
        let outcome = if !cached {
            2
        } else {
            u64::from(retained.is_some())
        };
        let walked = retained.is_none();
        let mut translation =
            retained.unwrap_or_else(|| walk::walk(&mut image, &state, request).unwrap());
        image
            .physical_check_access(translation.physical, 8, Access::Read, false)
            .unwrap();
        let mut filled = 0;
        if cached && walked {
            assert_eq!(translation.cache_id, None);
            translation = cache.fill(&state, request, translation);
            filled = u64::from(translation.cache_id.is_some());
        }
        let value = image.physical_read(translation.physical, 8).unwrap();
        let counts = walk::physical_counts::take();
        rows.push([
            outcome,
            filled,
            translation.physical,
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            counts[4],
            counts[5],
            value,
        ]);
    }
    // Reads establish A, never D; no stale whole-word publication erases the B mapping.
    assert_eq!(image.physical_read(0x4008, 8).unwrap(), 0xb027);
    assert_eq!(image.physical_read(0x1000, 8).unwrap(), 0x2027);
    assert_eq!(image.physical_read(0x2000, 8).unwrap(), 0x3027);
    assert_eq!(image.physical_read(0x3000, 8).unwrap(), 0x4027);
    (rows, removals)
}

#[test]
fn bounded_cache_work_trace_matches_literal_uncached_completed_reads() {
    let (cached, removals) = completed_reads(true);
    let (uncached, no_cache_removals) = completed_reads(false);
    assert_eq!(
        cached,
        [
            [0, 1, 0xa234, 7, 56, 3, 24, 3, 24, A],
            [1, 0, 0xa234, 0, 0, 0, 0, 0, 0, A],
            [1, 0, 0xa234, 0, 0, 0, 0, 0, 0, A],
            [0, 1, 0xb234, 5, 40, 1, 8, 1, 8, B],
            [1, 0, 0xb234, 0, 0, 0, 0, 0, 0, B],
            [0, 1, 0xb234, 4, 32, 0, 0, 0, 0, B],
        ]
    );
    assert_eq!(
        uncached,
        [
            [2, 0, 0xa234, 7, 56, 3, 24, 3, 24, A],
            [2, 0, 0xa234, 4, 32, 0, 0, 0, 0, A],
            [2, 0, 0xa234, 4, 32, 0, 0, 0, 0, A],
            [2, 0, 0xb234, 5, 40, 1, 8, 1, 8, B],
            [2, 0, 0xb234, 4, 32, 0, 0, 0, 0, B],
            [2, 0, 0xb234, 4, 32, 0, 0, 0, 0, B],
        ]
    );
    assert_eq!(removals, [1, 1]);
    assert!(no_cache_removals.is_empty());
    assert_eq!(
        cached
            .iter()
            .map(|row| (row[2], row[9]))
            .collect::<Vec<_>>(),
        uncached
            .iter()
            .map(|row| (row[2], row[9]))
            .collect::<Vec<_>>()
    );
    println!("CACHE_WORK_CACHED {cached:?}");
    println!("CACHE_WORK_UNCACHED {uncached:?}");
    println!("CACHE_WORK_REMOVALS {removals:?}");
    println!(
        "CACHE_WORK_LAYOUT {{\"cache\":{},\"translation\":{},\"instruction\":{},\"capacity\":64,\"fixtureBackingBytes\":65536,\"fixtureBackings\":1,\"fixtureMappings\":1}}",
        std::mem::size_of::<Cache>(),
        std::mem::size_of::<Translation>(),
        std::mem::size_of::<Instruction>()
    );
}
