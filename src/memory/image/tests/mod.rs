use super::*;

mod mapped;

#[test]
fn mapping_lookup_matches_widened_intervals_at_boundaries_gaps_and_the_final_byte() {
    let mappings = [
        (0x1000, 1),
        (0x1010, 8),
        (0x1020, 16),
        (0x1030, 1),
        (0x1_0000_0000, u32::MAX),
        (0x0000_7fff_ffff_fff8, 8),
        (0xffff_8000_0000_0000, 1),
        (u64::MAX - 15, 16),
    ]
    .map(|(base, length)| Mapping {
        base,
        backing: 0,
        offset: 0,
        length,
        permissions: Permissions {
            read: true,
            write: true,
            execute: false,
        },
    });
    let mut addresses = vec![0, 1, 0x0000_8000_0000_0000, 0xffff_7fff_ffff_ffff, u64::MAX];
    for mapping in &mappings {
        let start = u128::from(mapping.base);
        let end = start + u128::from(mapping.length);
        for point in [start - 1, start, start + 1, (start + end) / 2, end - 1, end] {
            if let Ok(address) = u64::try_from(point) {
                addresses.push(address);
            }
        }
    }
    addresses.sort_unstable();
    addresses.dedup();
    for count in 0..=mappings.len() {
        let prefix = &mappings[..count];
        for &address in &addresses {
            let expected = prefix.iter().position(|mapping| {
                let start = u128::from(mapping.base);
                (start..start + u128::from(mapping.length)).contains(&u128::from(address))
            });
            assert_eq!(
                locate_index(prefix, address),
                expected,
                "count{count} address{address:x}"
            );
        }
    }
}

#[test]
fn owned_spans_can_end_at_the_last_u64_byte() {
    let image = Image::new(u64::MAX - 4, vec![0xb8, 0, 0, 0, 0], 0x1000, vec![0; 8]).unwrap();
    assert_eq!(image.fetch(u64::MAX - 4), Ok(0xb8));
    assert_eq!(
        image.read_bytes(u64::MAX - 4, 5),
        Ok(vec![0xb8, 0, 0, 0, 0])
    );
}

#[test]
fn empty_spans_are_host_input_errors() {
    assert!(Image::new(0x1000, vec![], 0x2000, vec![0]).is_err());
    assert!(Image::new(0x1000, vec![0], 0x2000, vec![]).is_err());
}

#[test]
fn wrapping_and_noncanonical_spans_are_host_input_errors() {
    for (base, bytes) in [
        (u64::MAX, vec![0; 2]),
        (0x0000_8000_0000_0000, vec![0]),
        (0x0000_7fff_ffff_ffff, vec![0; 2]),
        (0xffff_7fff_ffff_ffff, vec![0; 2]),
    ] {
        assert!(Image::new(base, bytes.clone(), 0x2000, vec![0]).is_err());
        assert!(Image::new(0x1000, vec![0], base, bytes).is_err());
    }
}

#[test]
fn overlapping_spans_are_rejected_but_adjacent_spans_are_valid() {
    assert!(Image::new(0x1000, vec![0; 4], 0x1002, vec![0; 4]).is_err());
    assert!(Image::new(0x1002, vec![0; 4], 0x1000, vec![0; 4]).is_err());
    assert!(Image::new(0x1000, vec![0; 4], 0x1000, vec![0; 4]).is_err());
    assert!(Image::new(0x1000, vec![0; 4], 0x1004, vec![0; 4]).is_ok());
    assert!(Image::new(0x1004, vec![0; 4], 0x1000, vec![0; 4]).is_ok());
}
