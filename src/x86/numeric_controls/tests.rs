use super::*;
use crate::x86::decode::{self, DecodeError};

#[test]
fn decoded_forms_follow_all_32_independent_enable_truth_tables() {
    for (bytes, expected_family, expected) in [
        (&[0xd9, 0xe8][..], Family::X87, [24, 0, 8]),
        (&[0xdb, 0xe4][..], Family::X87, [24, 0, 8]),
        (&[0x9b][..], Family::Wait, [8, 0, 24]),
        (&[0x0f, 0xef, 0xc0][..], Family::Mmx, [8, 16, 8]),
        (&[0x66, 0x0f, 0xef, 0xc0][..], Family::Simd, [4, 24, 4]),
        (&[0xf3, 0x0f, 0xd6, 0xc1][..], Family::Bridge, [4, 24, 4]),
        (&[0x0f, 0xae, 0x00][..], Family::FxImage, [24, 0, 8]),
    ] {
        let op = decode::decode_with_context(0, &Default::default(), |pc| {
            bytes
                .get(pc as usize)
                .copied()
                .ok_or(DecodeError::Unsupported)
        })
        .unwrap()
        .operation;
        assert_eq!(family(op), Some(expected_family));
        let mut counts = [0; 3];
        for controls in 0..32 {
            let fault = admission(op, controls, 0x1000);
            counts[match fault {
                None => 2,
                Some(f) if f.kind == 1 => 0,
                _ => 1,
            }] += 1;
            if let Some(fault) = fault {
                assert_eq!(fault.validate(), Ok(()));
            }
        }
        assert_eq!(counts, expected, "{bytes:x?}");
    }
    for kind in [numeric_images::Kind::SaveX, numeric_images::Kind::RestoreX] {
        let Operation::NumericImage { mut op, address } =
            decode::decode(0, |pc| Ok([0x0f, 0xae, 0x03][pc as usize]))
                .unwrap()
                .operation
        else {
            unreachable!()
        };
        op.kind = kind;
        let operation = Operation::NumericImage { op, address };
        for controls in 0..32 {
            let result = admission(operation, controls, 0x1234);
            assert_eq!(result.is_some(), controls & 4 != 0);
            if let Some(fault) = result {
                assert_eq!(
                    (fault.vector(), fault.family, fault.instruction_pc),
                    (7, Family::XstateImage, 0x1234)
                );
                assert!(fault.validate().is_ok());
            }
        }
    }
    for bytes in [&[0x90][..], &[0x0f, 0xae, 0xf8], &[0x0f, 0xc3, 0x00]] {
        let op = decode::decode_with_context(0, &Default::default(), |pc| {
            bytes
                .get(pc as usize)
                .copied()
                .ok_or(DecodeError::Unsupported)
        })
        .unwrap()
        .operation;
        for controls in 0..32 {
            assert_eq!(admission(op, controls, 0), None);
        }
    }
}
