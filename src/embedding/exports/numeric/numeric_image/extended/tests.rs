use super::*;
use crate::memory::{MappingInput, MemoryLimits, Permissions};
use crate::x86::numeric_images::{Format, Kind};

fn mapped(spans: &[(u32, u32, bool, bool)]) -> Image {
    Image::mapped(
        vec![("area".into(), vec![0; 584])],
        spans
            .iter()
            .map(|&(start, length, read, write)| MappingInput {
                base: 0x4000 + u64::from(start),
                offset: start,
                length,
                backing: "area".into(),
                permissions: Permissions {
                    read,
                    write,
                    execute: false,
                },
            })
            .collect(),
        MemoryLimits {
            max_backings: 1,
            max_mappings: 16,
            max_backing_bytes: 584,
        },
    )
    .unwrap()
}

fn run(memory: &mut Image, op: Kind, mask: u64) -> Result<(), Failure> {
    let operation = ImageOp {
        kind: op,
        format: Format::Fx64Wide,
    };
    let access = if operation.is_store() {
        Access::Write
    } else {
        Access::Read
    };
    let operand = Operand {
        offset: 0x4000,
        base: 0x4000,
        segment: Segment::Ds,
        access,
    };
    let mut image = ExtendedImage::new(operation, 515, mask, 0);
    image.bytes[576..580].copy_from_slice(&0x00012345u32.to_le_bytes());
    transfer(
        &ExecutionContext::default(),
        memory,
        operand,
        &mut image,
        515,
        |_, _, _| {},
    )
}

#[test]
fn zero_save_reads_and_writes_just_the_header_even_without_a_base_mapping() {
    for (r, w) in [(true, true), (false, true), (true, false)] {
        let mut memory = mapped(&[(512, 8, r, w)]);
        let result = run(&mut memory, Kind::SaveX, 0);
        if r && w {
            assert!(result.is_ok());
        } else {
            let expected = if r { Access::Write } else { Access::Read };
            assert!(
                matches!(result,Err(Failure::Access(AccessFailure::Linear(Fault{access,cause:Cause::Permission,..}))) if access==expected)
            );
        }
    }
}

#[test]
fn late_header_write_denial_preserves_preceding_pkru_store() {
    let mut memory = mapped(&[(512, 8, true, false), (576, 4, true, true)]);
    memory.write(0x4240, 4, 0xdeadbeef).unwrap();
    assert!(matches!(
        run(&mut memory, Kind::SaveX, 512),
        Err(Failure::Access(AccessFailure::Linear(Fault {
            address: 0x4200,
            access: Access::Write,
            ..
        })))
    ));
    assert_eq!(memory.read(0x4240, 4).unwrap(), 0xdeadbeef);
}

#[test]
fn invalid_header_and_mxcsr_win_before_reads_of_later_components() {
    let mut memory = mapped(&[(24, 4, true, true), (512, 24, true, true)]);
    memory.write(0x4200, 8, 514).unwrap();
    memory.write(0x4208, 8, 1).unwrap();
    assert!(matches!(
        run(&mut memory, Kind::RestoreX, 514),
        Err(Failure::Header)
    ));
    memory.write(0x4208, 8, 0).unwrap();
    memory.write(0x4018, 4, 0x10000).unwrap();
    assert!(matches!(
        run(&mut memory, Kind::RestoreX, 514),
        Err(Failure::Mxcsr(0x10000))
    ));
    memory.write(0x4018, 4, 0x1f80).unwrap();
    assert!(matches!(
        run(&mut memory, Kind::RestoreX, 514),
        Err(Failure::Access(AccessFailure::Linear(Fault {
            address: 0x40a0,
            ..
        })))
    ));
    memory.write(0x4200, 8, 0).unwrap();
    assert!(run(&mut memory, Kind::RestoreX, 514).is_ok());
}
