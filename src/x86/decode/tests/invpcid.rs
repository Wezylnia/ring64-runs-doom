use super::*;
use crate::memory::{Access, Cause};
use crate::x86::context::{Mode, Segment};
use crate::x86::instruction::{Address, AddressBase, Operation, PagingInstruction, Width};

fn decode_bytes(bytes: &[u8], paged: bool) -> Result<Instruction, DecodeError> {
    decode_with_profile(0x1000, &ExecutionContext::default(), paged, |address| {
        bytes
            .get((address - 0x1000) as usize)
            .copied()
            .ok_or_else(|| {
                DecodeError::Fetch(Fault {
                    address,
                    width: 1,
                    access: Access::Execute,
                    cause: Cause::Unmapped,
                    failing: u128::from(address),
                })
            })
    })
}

#[test]
fn invpcid_fetches_only_encoding_bytes_selected_by_the_profile() {
    for bytes in [
        &[0x66, 0x0f, 0x38, 0x82][..],
        &[0x66, 0x0f, 0x38, 0x82, 0x04],
        &[0x66, 0x0f, 0x38, 0x82, 0x05],
    ] {
        let result = decode_bytes(bytes, true);
        assert!(
            matches!(result, Err(DecodeError::Fetch(_))),
            "{bytes:x?}: {result:?}"
        );
        assert_eq!(decode_bytes(bytes, false), Err(DecodeError::Unsupported));
    }
    for bytes in [
        &[0x0f, 0x38, 0x82][..],
        &[0xf2, 0x66, 0x0f, 0x38],
        &[0x66, 0xf3, 0x0f, 0x38],
        &[0x66, 0x0f, 0x38, 0x81],
    ] {
        assert_eq!(decode_bytes(bytes, true), Err(DecodeError::Unsupported));
    }
}

#[test]
fn invpcid_illegal_lock_and_register_forms_do_not_fetch_address_tails() {
    assert_eq!(
        decode_bytes(&[0x66, 0xf0, 0x0f, 0x38, 0x82], true),
        Err(DecodeError::UndefinedEncoding)
    );
    assert_eq!(
        decode_bytes(&[0x66, 0x0f, 0x38, 0x82, 0xc0], true),
        Err(DecodeError::Invalid)
    );
}

#[test]
fn invpcid_type_width_is_mode_selected_and_rex_extends_the_type_register() {
    let bytes = [0x66, 0x66, 0x44, 0x0f, 0x38, 0x82, 0x1b];
    assert_eq!(
        decode_bytes(&bytes, true).unwrap().operation,
        Operation::Paging(PagingInstruction::InvalidatePcid {
            address: Address {
                base: AddressBase::Register(3),
                index: None,
                displacement: 0,
                size: Width::W64,
                segment: Segment::Ds,
            },
            gpr: 11,
            width: Width::W64,
        })
    );
    for mode in [
        Mode::Real,
        Mode::Protected,
        Mode::Virtual8086,
        Mode::Compatibility,
    ] {
        let mut context = ExecutionContext::default();
        context.set_mode_and_cpl(mode, 0);
        let bytes = [0x66, 0x0f, 0x38, 0x82, 0x03];
        let instruction =
            decode_with_profile(0, &context, true, |address| Ok(bytes[address as usize])).unwrap();
        assert!(
            matches!(
                instruction.operation,
                Operation::Paging(PagingInstruction::InvalidatePcid {
                    gpr: 0,
                    width: Width::W32,
                    ..
                })
            ),
            "{mode:?}"
        );
    }
}

#[test]
fn invpcid_preserves_the_fifteen_byte_instruction_limit() {
    let mut bytes = vec![0x66; 11];
    bytes.extend([0x0f, 0x38, 0x82, 0x00]);
    assert_eq!(decode_bytes(&bytes, true).unwrap().next_pc, 0x100f);
    bytes.insert(0, 0x66);
    assert_eq!(
        decode_bytes(&bytes, true),
        Err(DecodeError::InstructionLength)
    );
}
