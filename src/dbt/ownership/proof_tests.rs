use super::*;
use crate::x86::context::Segment;
use std::cell::Cell;

#[test]
fn exact_root_binding_includes_pcid_and_all_cached_pae_entries() {
    for pdpte in [None, Some([0x1001, 0x2001, 0x3001, 0x4001])] {
        let state = paging::State {
            cr3: 0x5000,
            pdpte,
            ..paging::State::default()
        };
        let binding = FetchBinding {
            cr3: state.cr3,
            pdpte,
            proof: FetchProof::default(),
        };
        assert!(binding.same_root(&state));
        assert!(!binding.same_root(&paging::State {
            cr3: 0x5001,
            ..state
        }));
        if let Some(entries) = pdpte {
            for slot in 0..4 {
                let mut changed = entries;
                changed[slot] ^= 0x8000;
                assert!(!binding.same_root(&paging::State {
                    pdpte: Some(changed),
                    ..state
                }));
            }
        }
    }
}

#[test]
fn reached_instruction_proof_rejects_a_different_body_or_encoded_length() {
    use crate::x86::instruction::{Instruction, Operation};
    let context = ExecutionContext::default();
    let mut owners = Owners::default();
    let first = Instruction {
        pc: 0x1000,
        next_pc: 0x1001,
        operation: Operation::Nop,
    };
    let second = Instruction {
        pc: 0x1001,
        next_pc: 0x1002,
        operation: Operation::Halt,
    };
    assert!(owners.register(1, &context, false, 0));
    owners.record_paged(
        1,
        &paging::State::default(),
        FetchProof::default(),
        vec![first, second],
    );
    assert!(owners.instruction_matches(1, first));
    assert!(owners.instruction_matches(1, second));
    assert!(!owners.instruction_matches(
        1,
        Instruction {
            operation: Operation::Halt,
            ..first
        }
    ));
    assert!(!owners.instruction_matches(
        1,
        Instruction {
            next_pc: 0x1002,
            ..first
        }
    ));
    assert!(!owners.instruction_matches(2, first));
    owners.release(1);
    assert!(!owners.instruction_matches(1, first));
}

#[test]
fn context_proof_requires_full_match_and_successful_source_admission() {
    let context = ExecutionContext::default();
    let mut other = context.clone();
    let mut cache = other.segment(Segment::Fs);
    cache.base = 0x10000;
    other.set_segment(Segment::Fs, 0, cache, other.attributes(Segment::Fs));
    let mut owners = Owners::default();
    assert!(owners.register(1, &context, false, 0));
    assert!(owners.records[&1].admission != Admission::Current);
    let called = Cell::new(0);
    let source = |_: Option<&FetchBinding>| {
        called.set(called.get() + 1);
        true
    };
    assert!(!owners.admit(1, &context, false, source));
    assert_eq!(called.get(), 0);
    assert!(owners.publish(1, &context, false, 0, None));
    assert!(owners.records[&1].admission != Admission::Current);
    assert!(!owners.admit(1, &other, false, source));
    assert_eq!(called.get(), 0);
    assert!(!owners.admit(1, &context, false, |_| false));
    assert!(owners.records[&1].admission != Admission::Current);
    assert!(owners.admit(1, &context, false, source));
    assert!(owners.records[&1].admission == Admission::Current);
    assert!(owners.admit(1, &context, false, source));
    assert_eq!(called.get(), 2);
    assert!(!owners.admit(1, &context, true, source));
    assert_eq!(called.get(), 2);
    assert!(owners.records[&1].admission == Admission::Current);
    assert!(owners.admit(1, &context, false, source));
    assert!(!owners.admit(1, &context, false, |_| false));
    owners.release(1);
    assert!(!owners.admit(1, &context, false, source));
}

#[test]
fn context_proof_is_cleared_for_published_owners_and_pending_aba_is_cancelled() {
    for epoch in [0, u64::MAX] {
        let context = ExecutionContext::default();
        let mut other = context.clone();
        other.set_mode_and_cpl(crate::x86::context::Mode::Long64, 3);
        let mut owners = Owners::default();
        assert!(owners.register(1, &context, false, epoch));
        assert!(owners.publish(1, &context, false, epoch, None));
        assert!(owners.admit(1, &context, false, |_| true));
        assert!(owners.records[&1].admission == Admission::Current);
        assert!(owners.register(2, &context, false, epoch));
        let mut released = Vec::new();
        owners.cancel_pending(false, |owner| released.push(owner));
        assert_eq!(released, [2]);
        assert!(
            owners.records[&1].admission != Admission::Current,
            "prime before notification at {epoch}"
        );
        assert!(!owners.admit(1, &other, false, |_| panic!(
            "wrong context must not activate"
        )));
        owners.cancel_pending(false, |_| panic!("published owner retained"));
        assert!(owners.admit(1, &context, false, |_| true));
        assert!(!owners.publish(2, &context, false, epoch, None));
        owners.clear();
        assert!(!owners.admit(1, &context, false, |_| true));
    }
}
