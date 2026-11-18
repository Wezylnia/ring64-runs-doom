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
}

impl Owners {
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
            },
        );
        true
    }

    pub fn contains(&self, owner: u64) -> bool {
        self.records.contains_key(&owner)
    }

    pub fn instruction_matches(&self, owner: u64, instruction: Instruction) -> bool {
        self.records.get(&owner).is_some_and(|record| {
            record
                .instructions
                .binary_search_by_key(&instruction.pc, |i| i.pc)
                .is_ok_and(|index| record.instructions[index] == instruction)
        })
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
    }

    fn matches(record: &Assumptions, context: &ExecutionContext, observed: bool) -> bool {
        record.published && record.context == *context && record.observed == observed
    }

    #[cfg(test)]
    pub fn eligible(&self, owner: u64, context: &ExecutionContext, observed: bool) -> bool {
        self.records
            .get(&owner)
            .is_some_and(|record| Self::matches(record, context, observed))
    }

    pub fn admit(
        &mut self,
        owner: u64,
        context: &ExecutionContext,
        observed: bool,
        activate: impl FnOnce(Option<&FetchBinding>) -> bool,
    ) -> bool {
        let Some(record) = self.records.get_mut(&owner) else {
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
        let Some(record) = self.records.get_mut(&owner) else {
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
        self.records.retain(|&owner, record| {
            if record.published {
                if fetch_changed {
                    record.admission = Admission::CheckFetch;
                } else if record.admission == Admission::Current {
                    record.admission = Admission::CheckContext;
                }
                true
            } else {
                release(owner);
                false
            }
        });
    }

    pub fn release(&mut self, owner: u64) {
        self.records.remove(&owner);
    }

    pub fn clear(&mut self) {
        self.records.clear();
    }
}

#[cfg(test)]
mod proof_tests;
#[cfg(test)]
mod tests;
