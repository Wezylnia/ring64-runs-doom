use super::*;
use crate::memory::{Cause, Fault, MappingInput, MemoryLimits, Permissions};
use crate::x86::paging::walk::{Failure, PhysicalFault};
use crate::x86::protection_keys::State as Keys;

fn fixture(second_readable: bool, top_readable: bool) -> (Image, State, AccessState) {
    let mapping = |base, offset, length, read| MappingInput {
        base,
        backing: "ram".into(),
        offset,
        length,
        permissions: Permissions {
            read,
            write: true,
            execute: false,
        },
    };
    let image = Image::physical_mapped(
        vec![("ram".into(), vec![0xa5; 8])],
        vec![
            mapping(0x1000, 0, 2, true),
            mapping(0x1002, 2, 3, second_readable),
            mapping(0xffff_ffff, 7, 1, top_readable),
        ],
        MemoryLimits {
            max_backing_bytes: 8,
            max_backings: 1,
            max_mappings: 3,
        },
    )
    .unwrap();
    (
        image,
        State {
            cr0: 0x40011,
            ..State::reset()
        },
        AccessState::default(),
    )
}

#[test]
fn compact_pg0_plan_captures_physical_spans_without_fake_dirty_metadata() {
    let (mut image, state, mut work) = fixture(true, true);
    let mut memory = Memory::new(&mut image, &state, 0, 2, &mut work, &Keys::ZERO);
    memory.check_write(0x1001, 4).unwrap();
    {
        let storage = memory.storage.borrow();
        let fragment = &storage.work.pending[0].fragments[0];
        assert_eq!(
            (
                fragment.physical,
                fragment.length,
                fragment.leaf,
                fragment.entry_width,
                fragment.dirty,
                fragment.cache_id
            ),
            (0x1001, 4, 0, 0, false, None)
        );
    }
    memory.prepare_publication().unwrap();
    memory.write(0x1001, 4, 0x11223344).unwrap();
    assert_eq!(memory.read(0x1001, 4).unwrap(), 0x11223344);
    assert!(memory.storage.borrow().work.pending.is_empty());
}

#[test]
fn pg0_later_private_fragment_refusal_precedes_natural_alignment() {
    for second_readable in [false, true] {
        let (mut image, state, mut work) = fixture(second_readable, true);
        let memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
        let failure = memory
            .read_aligned(0x1001, 4, Intent::Natural(4))
            .unwrap_err();
        if second_readable {
            assert!(matches!(failure, AccessFailure::AlignmentCheck(_)));
        } else {
            assert_eq!(
                failure,
                AccessFailure::Paging(Failure::Physical(PhysicalFault {
                    linear: 0x1001,
                    access: Access::Read,
                    physical: Fault {
                        address: 0x1001,
                        width: 4,
                        access: Access::Read,
                        cause: Cause::Permission,
                        failing: 0x1002
                    },
                    phase: PhysicalPhase::FinalAccess
                }))
            );
        }
        assert!(memory.storage.borrow().work.pending.is_empty());
    }
}

#[test]
fn pg0_earlier_private_byte_or_later_domain_refusal_precedes_natural_alignment() {
    for top_readable in [false, true] {
        let (mut image, state, mut work) = fixture(true, top_readable);
        let memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
        let failure = memory
            .read_aligned(0xffff_ffff, 4, Intent::Natural(4))
            .unwrap_err();
        let address = if top_readable {
            0x1_0000_0000
        } else {
            0xffff_ffff
        };
        assert_eq!(
            failure,
            AccessFailure::Paging(Failure::Physical(PhysicalFault {
                linear: address,
                access: Access::Read,
                physical: Fault {
                    address,
                    width: 1,
                    access: Access::Read,
                    cause: if top_readable {
                        Cause::AddressRange
                    } else {
                        Cause::Permission
                    },
                    failing: u128::from(address)
                },
                phase: PhysicalPhase::FinalAccess
            }))
        );
        assert!(memory.storage.borrow().work.pending.is_empty());
    }
}
