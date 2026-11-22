#[cfg(not(feature = "parallel"))]
use std::cell::Cell;
use std::collections::HashMap;

use super::FetchProof;
use crate::memory::Image;
use crate::x86::{context::ExecutionContext, instruction::Instruction, paging};

#[derive(Debug)]
pub(crate) struct FetchBinding {
    cr3: u64,
    pdpte: Option<[u64; 4]>,
    proof: FetchProof,
}

impl FetchBinding {
    fn same_root(&self, state: &paging::State) -> bool {
        // CR3 includes the effective PCID. Family/control changes retire all owners.
        self.cr3 == state.cr3 && self.pdpte == state.pdpte
    }

    #[inline(never)]
    pub fn matches(
        &self,
        image: &Image,
        state: &paging::State,
        cache: &paging::cache::Cache,
        cpl: u8,
    ) -> bool {
        self.same_root(state) && self.proof.matches(image, state, cache, cpl)
    }
}

#[derive(Debug)]
struct Assumptions {
    context: ExecutionContext,
    observed: bool,
    epoch: u64,
    published: bool,
    admission: Admission,
    fetch: Option<FetchBinding>,
    instructions: Vec<Instruction>,
    #[cfg(not(feature = "parallel"))]
    instruction_hint: Cell<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    CheckContext,
    CheckFetch,
    Current,
}

/// Compiled assumptions share the memory source owner's identity and lifetime.
#[derive(Debug, Default)]
pub(crate) struct Owners {
    records: HashMap<u64, Assumptions>,
    #[cfg(not(feature = "parallel"))]
    active_fetch: Option<(u64, Assumptions)>,
}

impl Owners {
    fn record(&self, owner: u64) -> Option<&Assumptions> {
        #[cfg(not(feature = "parallel"))]
        if let Some((active, record)) = &self.active_fetch
            && *active == owner
        {
            return Some(record);
        }
        self.records.get(&owner)
    }

    fn record_mut(&mut self, owner: u64) -> Option<&mut Assumptions> {
        #[cfg(not(feature = "parallel"))]
        if let Some((active, record)) = &mut self.active_fetch
            && *active == owner
        {
            return Some(record);
        }
        self.records.get_mut(&owner)
    }

    pub fn register(
        &mut self,
        owner: u64,
        context: &ExecutionContext,
        observed: bool,
        epoch: u64,
    ) -> bool {
        if self.records.try_reserve(1).is_err() {
            return false;
        }
        self.records.insert(
            owner,
            Assumptions {
                context: context.clone(),
                observed,
                epoch,
                published: false,
                admission: Admission::CheckContext,
                fetch: None,
                instructions: Vec::new(),
                #[cfg(not(feature = "parallel"))]
                instruction_hint: Cell::new(0),
            },
        );
        true
    }

    pub fn contains(&self, owner: u64) -> bool {
        self.record(owner).is_some()
    }

    pub fn instruction_matches(&self, owner: u64, instruction: Instruction) -> bool {
        self.record(owner).is_some_and(|record| {
            record
                .instructions
                .binary_search_by_key(&instruction.pc, |i| i.pc)
                .is_ok_and(|index| record.instructions[index] == instruction)
        })
    }

    #[cfg(not(feature = "parallel"))]
    pub fn current_instruction(
        &self,
        owner: u64,
        pc: u64,
        state: &paging::State,
        linear_start: impl FnOnce(Instruction) -> Option<u64>,
    ) -> Option<(Instruction, u64, u64)> {
        let (active, record) = self.active_fetch.as_ref()?;
        if *active != owner {
            return None;
        }
        let fetch = record.fetch.as_ref()?;
        if record.admission != Admission::Current || !fetch.same_root(state) {
            return None;
        }
        let last = record.instruction_hint.get();
        let next = last + 1;
        let has_pc = |index| {
            record
                .instructions
                .get(index)
                .is_some_and(|i: &Instruction| i.pc == pc)
        };
        let index = if has_pc(next) {
            next
        } else if has_pc(last) {
            last
        } else {
            record
                .instructions
                .binary_search_by_key(&pc, |i| i.pc)
                .ok()?
        };
        let instruction = record.instructions[index];
        let linear = linear_start(instruction)?;
        let physical = fetch
            .proof
            .physical_span(linear, instruction.encoded_len())?;
        record.instruction_hint.set(index);
        Some((instruction, linear, physical))
    }

    pub fn record_paged(
        &mut self,
        owner: u64,
        state: &paging::State,
        proof: FetchProof,
        instructions: Vec<Instruction>,
    ) {
        let record = self
            .records
            .get_mut(&owner)
            .expect("registered paged owner");
        record.fetch = Some(FetchBinding {
            cr3: state.cr3,
            pdpte: state.pdpte,
            proof,
        });
        record.instructions = instructions;
        #[cfg(not(feature = "parallel"))]
        record.instruction_hint.set(0);
    }

    fn matches(record: &Assumptions, context: &ExecutionContext, observed: bool) -> bool {
        record.published && record.context == *context && record.observed == observed
    }

    #[cfg(test)]
    pub fn eligible(&self, owner: u64, context: &ExecutionContext, observed: bool) -> bool {
        self.record(owner)
            .is_some_and(|record| Self::matches(record, context, observed))
    }

    pub fn admit(
        &mut self,
        owner: u64,
        context: &ExecutionContext,
        observed: bool,
        activate: impl FnOnce(Option<&FetchBinding>) -> bool,
    ) -> bool {
        let Some(record) = self.record_mut(owner) else {
            return false;
        };
        if !record.published || record.observed != observed {
            return false;
        }
        let fetch = match record.admission {
            Admission::Current => None,
            _ if !Self::matches(record, context, observed) => return false,
            Admission::CheckContext => None,
            Admission::CheckFetch => match record.fetch.as_ref() {
                Some(binding) => Some(binding),
                None => return false,
            },
        };
        if !activate(fetch) {
            return false;
        }
        record.admission = Admission::Current;
        #[cfg(not(feature = "parallel"))]
        if record.fetch.is_some()
            && self
                .active_fetch
                .as_ref()
                .is_none_or(|(active, _)| *active != owner)
        {
            // Remove the incoming record first so switching never grows the map.
            let incoming = self.records.remove(&owner).expect("admitted paged owner");
            if let Some((previous, record)) = self.active_fetch.replace((owner, incoming)) {
                self.records.insert(previous, record);
            }
        }
        true
    }

    pub fn publish(
        &mut self,
        owner: u64,
        context: &ExecutionContext,
        observed: bool,
        epoch: u64,
        paging: Option<&paging::State>,
    ) -> bool {
        let Some(record) = self.record_mut(owner) else {
            return false;
        };
        if record.context != *context
            || record.observed != observed
            || !record.published && record.epoch != epoch
            || record
                .fetch
                .as_ref()
                .is_some_and(|fetch| !paging.is_some_and(|state| fetch.same_root(state)))
        {
            return false;
        }
        record.published = true;
        true
    }

    pub fn cancel_pending(&mut self, fetch_changed: bool, mut release: impl FnMut(u64)) {
        let invalidate = |record: &mut Assumptions| {
            if fetch_changed {
                record.admission = Admission::CheckFetch;
            } else if record.admission == Admission::Current {
                record.admission = Admission::CheckContext;
            }
        };
        #[cfg(not(feature = "parallel"))]
        if let Some((_, record)) = &mut self.active_fetch {
            invalidate(record);
        }
        self.records.retain(|&owner, record| {
            if record.published {
                invalidate(record);
                true
            } else {
                release(owner);
                false
            }
        });
    }

    pub fn release(&mut self, owner: u64) {
        #[cfg(not(feature = "parallel"))]
        if self
            .active_fetch
            .as_ref()
            .is_some_and(|(active, _)| *active == owner)
        {
            self.active_fetch = None;
            return;
        }
        self.records.remove(&owner);
    }

    pub fn clear(&mut self) {
        #[cfg(not(feature = "parallel"))]
        {
            self.active_fetch = None;
        }
        self.records.clear();
    }
}

#[cfg(test)]
mod proof_tests;
#[cfg(test)]
mod tests;
