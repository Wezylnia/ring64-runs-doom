use super::{at, missing, operation};
use crate::x86::decode::DecodeError;

#[test]
fn selected_mandatory_simd_forms_require_their_encoding_before_lock_faults() {
    for (repeat, tail) in [
        (0xf2, &[0x58, 0xc1][..]),
        (0xf3, &[0x58, 0xc1]),
        (0xf2, &[0xd6, 0xc1]),
        (0xf3, &[0xd6, 0xc1]),
        (0xf3, &[0x6f, 0xc1]),
        (0xf2, &[0xf0, 0x03]),
        (0xf3, &[0x16, 0x03]),
        (0xf3, &[0xc2, 0x84, 0x24, 1, 2, 3, 4, 0]),
        (0xf2, &[0x70, 0x84, 0x24, 1, 2, 3, 4, 0]),
    ] {
        operation(&[&[repeat, 0x0f], tail].concat());
        for pair in [[0xf0, repeat], [repeat, 0xf0]] {
            let bytes = [&pair[..], &[0x0f], tail].concat();
            for len in 1..bytes.len() {
                assert_eq!(
                    at(0x1000, &bytes[..len]),
                    Err(DecodeError::Fetch(missing(0x1000 + len as u64))),
                    "{bytes:02x?}/{len}"
                );
            }
            assert_eq!(at(0x1000, &bytes), Err(DecodeError::Invalid));
        }
    }
}

#[test]
fn mixed_lock_preserves_deciding_bytes_for_unselected_or_illegal_forms() {
    for repeat in [0xf2, 0xf3] {
        for pair in [[0xf0, repeat], [repeat, 0xf0]] {
            for tail in [
                &[0x87][..],   // hinted xchg
                &[0xa4],       // rep movs
                &[0x6c],       // rep ins
                &[0xc4],       // vex
                &[0x0f, 0xc1], // hinted xadd
                &[0x0f, 0xa2], // cpuid
                &[0x0f, 0x31], // rdtsc
                &[0x0f, 0x38], // unselected three-byte map
                &[0x0f, 0x18], // prefetch
            ] {
                let bytes = [&pair[..], tail].concat();
                assert_eq!(
                    at(0x1000, &bytes),
                    Err(DecodeError::Unsupported),
                    "{bytes:02x?}"
                );
            }
            let bytes = [&pair[..], &[0x0f, 0xd6, 0x84]].concat();
            assert_eq!(at(0x1000, &bytes), Err(DecodeError::Invalid));
        }
    }
    for pair in [[0xf0, 0xf2], [0xf2, 0xf0]] {
        assert_eq!(
            at(0x1000, &[&pair[..], &[0x0f, 0xae]].concat()),
            Err(DecodeError::Unsupported)
        );
    }
    for prefix in [
        [0xf0, 0xf2, 0xf3],
        [0xf2, 0xf0, 0xf3],
        [0xf3, 0xf0, 0xf2],
        [0xf3, 0xf2, 0xf0],
    ] {
        assert_eq!(at(0x1000, &prefix), Err(DecodeError::Unsupported));
    }
    for prefix in [[0xf2, 0xf3, 0xf0], [0xf3, 0xf3, 0xf0]] {
        assert_eq!(
            at(0x1000, &[&prefix[..], &[0x0f, 0xae, 0xc0]].concat()),
            Err(DecodeError::UndefinedEncoding)
        );
        assert_eq!(
            at(0x1000, &[&prefix[..], &[0x0f, 0x58]].concat()),
            Err(DecodeError::Unsupported)
        );
    }
}
