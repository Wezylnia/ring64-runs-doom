use super::super::*;
use crate::memory::{MappingInput, MemoryLimits, Permissions};
use crate::x86::alignment::{Fault as AlignmentFault, Intent};
use crate::x86::paging::walk::{Failure, PageFault, PhysicalPhase};
use crate::x86::protection_keys::State as Keys;

fn fixture(second: u64) -> (Image, State) {
    let mut bytes = vec![0; 0xc000];
    for (address, word) in [
        (0x1000, 0x2027u32),
        (0x2018, 0xa007),
        (0x201c, second as u32),
    ] {
        bytes[address..address + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes[0xafff] = 0x11;
    bytes[0xb000..0xb003].copy_from_slice(&[0x22, 0x33, 0x44]);
    let image = Image::physical_mapped(
        vec![("ram".into(), bytes)],
        vec![MappingInput {
            base: 0,
            backing: "ram".into(),
            offset: 0,
            length: 0xc000,
            permissions: Permissions {
                read: true,
                write: true,
                execute: true,
            },
        }],
        MemoryLimits {
            max_backing_bytes: 0xc000,
            max_backings: 1,
            max_mappings: 8,
        },
    )
    .unwrap();
    let state = State {
        cr0: 0x80040033,
        cr2: 0xdead,
        cr3: 0x1000,
        cr4: 0,
        efer: 0,
        pdpte: None,
    };
    (image, state)
}

fn alignment(address: u64, width: u32, access: Access, granule: u32) -> AccessFailure {
    AccessFailure::AlignmentCheck(AlignmentFault {
        address,
        width,
        access,
        alignment: granule,
    })
}

#[test]
fn split_page_alignment_retains_accessed_but_never_dirty_or_pending_payload() {
    for rmw in [false, true] {
        let (mut image, state) = fixture(0xb007);
        let mut work = AccessState::default();
        let memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
        let result = if rmw {
            memory
                .read_for_write_aligned(0x6fff, 4, Intent::Natural(4))
                .map(|_| ())
        } else {
            memory.check_write_aligned(0x6fff, 4, Intent::Natural(4))
        };
        assert_eq!(result, Err(alignment(0x6fff, 4, Access::Write, 4)));
        assert!(work.pending.is_empty());
        assert_eq!(image.physical_read(0x2018, 4), Ok(0xa027));
        assert_eq!(image.physical_read(0x201c, 4), Ok(0xb027));
        assert_eq!(image.physical_read(0xafff, 1), Ok(0x11));
        assert_eq!(
            image.physical_read_bytes(0xb000, 3),
            Ok(vec![0x22, 0x33, 0x44])
        );
        assert_eq!(state.cr2, 0xdead);
    }
}

#[test]
fn complete_nominal_page_admission_precedes_alignment_in_fragment_order() {
    for (second, error_code, second_after) in [(0, 6, 0), (0xb003, 7, 0xb023)] {
        let (mut image, state) = fixture(second);
        let mut work = AccessState::default();
        let memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
        assert_eq!(
            memory.check_write_aligned(0x6fff, 4, Intent::Natural(4)),
            Err(AccessFailure::Paging(Failure::Page(PageFault {
                address: 0x7000,
                access: Access::Write,
                error_code,
            }))),
        );
        assert!(work.pending.is_empty());
        assert_eq!(image.physical_read(0x2018, 4), Ok(0xa027));
        assert_eq!(image.physical_read(0x201c, 4), Ok(second_after));
    }
}

#[test]
fn final_private_denial_keeps_fragment_geometry_and_precedes_alignment() {
    let (mut image, state) = fixture(0xb007);
    let mapping = |base, length, read| MappingInput {
        base,
        backing: "ram".into(),
        offset: base as u32,
        length,
        permissions: Permissions {
            read,
            write: true,
            execute: false,
        },
    };
    image
        .set_mappings(vec![
            mapping(0, 0xb000, true),
            mapping(0xb000, 3, false),
            mapping(0xb003, 0xffd, true),
        ])
        .unwrap();
    let mut work = AccessState::default();
    let memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
    let Err(AccessFailure::Paging(Failure::Physical(fault))) =
        memory.read_aligned(0x6fff, 4, Intent::Natural(4))
    else {
        panic!("final private refusal")
    };
    assert_eq!(fault.linear, 0x7000);
    assert_eq!(fault.access, Access::Read);
    assert_eq!(fault.phase, PhysicalPhase::FinalAccess);
    assert_eq!(fault.physical.address, 0xb000);
    assert_eq!(fault.physical.width, 3);
    assert_eq!(fault.physical.failing, 0xb000);
    assert!(work.pending.is_empty());
    assert_eq!(image.physical_read(0x2018, 4), Ok(0xa027));
    assert_eq!(image.physical_read(0x201c, 4), Ok(0xb027));
}

#[test]
fn alignment_uses_current_controls_and_prospective_frame_privilege() {
    for (cr0, cpl, flags, active) in [
        (0x40011, 3, 0x40002, true),
        (0x00011, 3, 0x40002, false),
        (0x40011, 3, 2, false),
        (0x40011, 0, 0x40002, false),
        (0x40011, 1, 0x40002, false),
        (0x40011, 2, 0x40002, false),
        // Native capability input isolates PE; a public PE0/CPL3 context is invalid.
        (0x40010, 3, 0x40002, false),
    ] {
        let (mut image, mut state) = fixture(0xb007);
        state.cr0 = cr0;
        let mut work = AccessState::default();
        let memory = Memory::new(&mut image, &state, cpl, flags, &mut work, &Keys::ZERO);
        let expected = if active {
            Err(alignment(0x6001, 4, Access::Read, 4))
        } else {
            Ok(0)
        };
        assert_eq!(memory.read_aligned(0x6001, 4, Intent::Natural(4)), expected);
    }

    for (current, target, fails) in [(0, 3, true), (3, 0, false), (3, 3, true)] {
        let (mut image, mut state) = fixture(0xb007);
        state.cr0 = 0x40011;
        let mut work = AccessState::default();
        let memory = Memory::new(&mut image, &state, current, 0x40002, &mut work, &Keys::ZERO);
        let expected = if fails {
            Err(alignment(0x6001, 4, Access::Write, 4))
        } else {
            Ok(())
        };
        assert_eq!(
            memory.check_frame_write_aligned(0x6001, 4, target, Intent::Natural(4)),
            expected
        );
    }
}

#[test]
fn component_alignment_follows_complete_record_admission_and_reports_its_field() {
    let (mut image, mut state) = fixture(0xb007);
    state.cr0 = 0x40011;
    let mut work = AccessState::default();
    let memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
    assert_eq!(
        memory.check_write_aligned(0x6000, 6, Intent::SystemRecord(4)),
        Err(alignment(0x6002, 4, Access::Write, 4))
    );
    assert_eq!(
        memory.check_write_aligned(0x6002, 6, Intent::SystemRecord(4)),
        Ok(())
    );
    assert_eq!(
        memory.check_write_aligned(0x6010, 10, Intent::SystemRecord(8)),
        Err(alignment(0x6012, 8, Access::Write, 8))
    );
    assert_eq!(
        memory.check_write_aligned(0x6016, 10, Intent::SystemRecord(8)),
        Ok(())
    );
    assert_eq!(
        memory.read_vector_aligned(0x6028, 10, Intent::Natural(8)),
        Ok([0; 16])
    );
    assert_eq!(
        memory.read_vector_aligned(0x6024, 10, Intent::Natural(8)),
        Err(alignment(0x6024, 10, Access::Read, 8))
    );
    assert_eq!(
        memory.check_read_aligned(0x6001, 16, Intent::Exempt),
        Ok(())
    );
    assert_eq!(memory.probe_write(0x6001), Ok(()));
    assert_eq!(memory.read_system(0x6001, 8), Ok([0; 16]));
}

#[test]
fn refused_alignment_cancels_unpublished_batch_without_undoing_prior_publication() {
    let (mut image, mut state) = fixture(0xb007);
    state.cr0 = 0x40011;
    let mut work = AccessState::default();
    let mut memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
    memory
        .write_aligned(0x6000, 4, 0x12345678, Intent::Natural(4))
        .unwrap();
    memory
        .check_write_aligned(0x6010, 4, Intent::Natural(4))
        .unwrap();
    assert_eq!(
        memory.check_write_aligned(0x6011, 4, Intent::Natural(4)),
        Err(alignment(0x6011, 4, Access::Write, 4))
    );
    assert!(work.pending.is_empty());
    assert_eq!(image.physical_read(0x6000, 4), Ok(0x12345678));
    assert_eq!(image.physical_read(0x6010, 4), Ok(0));
}

#[test]
fn captured_frame_authority_does_not_recheck_initiating_privilege_at_commit() {
    let (mut image, mut state) = fixture(0xb007);
    state.cr0 = 0x40011;
    let mut work = AccessState::default();
    let mut memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
    memory
        .check_frame_write_aligned(0x6001, 4, 0, Intent::Natural(4))
        .unwrap();
    memory.prepare_publication().unwrap();
    // The saved prospective-CPL0 admission is authoritative, even at current CPL3.
    assert_eq!(
        memory.write_aligned(0x6001, 4, 0x12345678, Intent::Natural(4)),
        Ok(()),
        "captured prospective-CPL0 authority remains valid at commit",
    );
    assert!(work.pending.is_empty());
    assert_eq!(image.physical_read(0x6001, 4), Ok(0x12345678));
}

#[test]
fn odd_system_metadata_is_exempt_while_the_same_user_operand_faults() {
    let (mut image, mut state) = fixture(0xb007);
    state.cr0 = 0x40011;
    let mut work = AccessState::default();
    let memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
    assert_eq!(
        memory.read_system(0x6001, 8),
        Ok([0; 16]),
        "implicit metadata does not acquire ordinary natural alignment"
    );
    assert_eq!(
        memory.read_aligned(0x6001, 8, Intent::Natural(8)),
        Err(alignment(0x6001, 8, Access::Read, 8)),
        "the same address is an eligible ordinary user operand"
    );
}

#[test]
fn alignment_fault_retains_accessed_source_owner_invalidation_without_payload() {
    let (mut image, state) = fixture(0xb007);
    let owner = image.register_source([(0x2018, 4)]).unwrap();
    assert!(image.activate(owner));
    assert!(image.owner_valid(owner));
    assert!(!image.active_changed());
    let mut work = AccessState::default();
    let memory = Memory::new(&mut image, &state, 3, 0x40002, &mut work, &Keys::ZERO);
    assert_eq!(
        memory.read_for_write_aligned(0x6fff, 4, Intent::Natural(4)),
        Err(alignment(0x6fff, 4, Access::Write, 4)),
        "reached RMW faults after its permitted walk A effects",
    );
    assert!(work.pending.is_empty());
    assert_eq!(image.physical_read(0x2018, 4), Ok(0xa027));
    assert_eq!(image.physical_read(0x201c, 4), Ok(0xb027));
    assert_eq!(image.physical_read(0xafff, 1), Ok(0x11));
    assert_eq!(
        image.physical_read_bytes(0xb000, 3),
        Ok(vec![0x22, 0x33, 0x44])
    );
    assert!(
        !image.owner_valid(owner),
        "A-only publication invalidates the actual leaf-word source owner",
    );
    assert!(
        image.active_changed(),
        "zero-payload refusal retains active source invalidation"
    );
}
