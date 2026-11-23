#![cfg_attr(
    guest_memory64,
    allow(
        clippy::useless_conversion,
        reason = "backing offsets widen from u32 only in the memory64 build"
    )
)]

use std::collections::BTreeMap;
use std::rc::Rc;

use super::image::{Backing, BackingOffset, Mapping, locate};
use super::{Access, Image, MemoryError};

#[derive(Debug)]
struct SourceSlice {
    guest: u64,
    length: u32,
    incarnation: Rc<()>,
    offset: BackingOffset,
    access: Access,
}

#[derive(Debug)]
struct Owner {
    valid: bool,
    sources: Vec<SourceSlice>,
}

#[derive(Debug)]
pub(super) struct Validity {
    next: u64,
    owners: BTreeMap<u64, Owner>,
    active: Option<u64>,
    active_invalid: bool,
    invalidated: bool,
    sources: BTreeMap<usize, Vec<(BackingOffset, u64)>>,
    completed_write: Option<(usize, BackingOffset, u64)>,
}

impl Default for Validity {
    fn default() -> Self {
        Self {
            next: 1,
            owners: BTreeMap::new(),
            active: None,
            active_invalid: false,
            invalidated: false,
            sources: BTreeMap::new(),
            completed_write: None,
        }
    }
}

impl Validity {
    fn index_source(&mut self, source: &SourceSlice) {
        let key = Rc::as_ptr(&source.incarnation) as usize;
        let ranges = self.sources.entry(key).or_default();
        let start = source.offset;
        let end = u64::from(start) + u64::from(source.length);
        let first = ranges.partition_point(|range| range.1 < u64::from(start));
        let after = ranges.partition_point(|range| u64::from(range.0) <= end);
        if first == after {
            ranges.insert(first, (start, end));
            return;
        }
        ranges[first] = (start.min(ranges[first].0), end.max(ranges[after - 1].1));
        ranges.drain(first + 1..after);
    }

    fn rebuild_sources(&mut self) {
        self.completed_write = None;
        self.sources.clear();
        for owner in self.owners.values().filter(|owner| owner.valid) {
            for source in &owner.sources {
                self.sources
                    .entry(Rc::as_ptr(&source.incarnation) as usize)
                    .or_default()
                    .push((
                        source.offset,
                        u64::from(source.offset) + u64::from(source.length),
                    ));
            }
        }
        for ranges in self.sources.values_mut() {
            ranges.sort_unstable();
            let mut merged = 0;
            for index in 0..ranges.len() {
                let current = ranges[index];
                if merged > 0 && u64::from(current.0) <= ranges[merged - 1].1 {
                    ranges[merged - 1].1 = ranges[merged - 1].1.max(current.1);
                } else {
                    ranges[merged] = current;
                    merged += 1;
                }
            }
            ranges.truncate(merged);
        }
    }

    pub(super) fn write(&mut self, incarnation: &Rc<()>, offset: BackingOffset, length: u32) {
        let end = u64::from(offset) + u64::from(length);
        let identity = Rc::as_ptr(incarnation) as usize;
        if self.completed_write.is_some_and(|(backing, start, limit)| {
            backing == identity && offset >= start && end <= limit
        }) {
            return;
        }
        let Some(ranges) = self.sources.get(&identity) else {
            return;
        };
        let index = ranges.partition_point(|range| u64::from(range.0) < end);
        if index == 0 || ranges[index - 1].1 <= u64::from(offset) {
            return;
        }
        for (&id, owner) in self.owners.iter_mut().filter(|(_, owner)| owner.valid) {
            if owner.sources.iter().any(|source| {
                Rc::ptr_eq(&source.incarnation, incarnation)
                    && u64::from(source.offset) < end
                    && u64::from(offset) < u64::from(source.offset) + u64::from(source.length)
            }) {
                owner.valid = false;
                self.invalidated = true;
                if self.active == Some(id) {
                    self.active_invalid = true;
                }
            }
        }
        // Every live owner overlapping this span is now invalid; until source
        // registration/remapping/release, writing within it cannot affect another.
        self.completed_write = Some((identity, offset, end));
    }

    pub(super) fn mapping_change(&mut self, backings: &[Backing], mappings: &[Mapping]) {
        for owner in self.owners.values_mut().filter(|owner| owner.valid) {
            owner.valid = owner
                .sources
                .iter()
                .all(|source| source_current(source, backings, mappings));
            self.invalidated |= !owner.valid;
        }
        self.active_invalid = self
            .active
            .is_some_and(|id| !self.owners.get(&id).is_some_and(|owner| owner.valid));
        self.rebuild_sources();
    }
}

fn permits(mapping: &Mapping, access: Access) -> bool {
    match access {
        Access::Read => mapping.permissions.read,
        Access::Write => mapping.permissions.write,
        Access::Execute => mapping.permissions.execute,
    }
}

fn source_current(source: &SourceSlice, backings: &[Backing], mappings: &[Mapping]) -> bool {
    let mut cursor = u128::from(source.guest);
    let end = cursor + u128::from(source.length);
    while cursor < end {
        let Some(mapping) = locate(mappings, cursor as u64) else {
            return false;
        };
        let offset = u64::from(mapping.offset) + (cursor as u64 - mapping.base);
        if !permits(mapping, source.access)
            || !Rc::ptr_eq(&backings[mapping.backing].incarnation, &source.incarnation)
            || offset != u64::from(source.offset) + (cursor - u128::from(source.guest)) as u64
        {
            return false;
        }
        cursor = end.min(mapping.end());
    }
    true
}

impl Image {
    pub fn register_source(
        &mut self,
        ranges: impl IntoIterator<Item = (u64, u32)>,
    ) -> Result<u64, MemoryError> {
        self.register_dependencies(ranges, [])
    }

    pub fn register_dependencies(
        &mut self,
        executable_ranges: impl IntoIterator<Item = (u64, u32)>,
        read_ranges: impl IntoIterator<Item = (u64, u32)>,
    ) -> Result<u64, MemoryError> {
        let mut sources = Vec::new();
        let ranges = executable_ranges
            .into_iter()
            .map(|range| (range, Access::Execute))
            .chain(read_ranges.into_iter().map(|range| (range, Access::Read)));
        for ((start, length), access) in ranges {
            if length == 0 || self.mapping_span_failure(start, u64::from(length)) {
                return Err(MemoryError::Invalid("invalid artifact source range"));
            }
            let mut cursor = u128::from(start);
            let end = cursor + u128::from(length);
            while cursor < end {
                let mapping = self
                    .locate(cursor as u64)
                    .filter(|mapping| permits(mapping, access))
                    .ok_or(MemoryError::Invalid(if access == Access::Execute {
                        "artifact source is no longer executable"
                    } else {
                        "artifact dependency is no longer readable"
                    }))?;
                let count = (end.min(mapping.end()) - cursor) as u32;
                sources
                    .try_reserve(1)
                    .map_err(|_| MemoryError::Capacity("source bookkeeping allocation refused"))?;
                sources.push(SourceSlice {
                    guest: cursor as u64,
                    length: count,
                    incarnation: self.backings[mapping.backing].incarnation.clone(),
                    offset: mapping.offset + (cursor as u64 - mapping.base) as BackingOffset,
                    access,
                });
                cursor += u128::from(count);
            }
        }
        let owner = self.validity.next;
        let next = owner.checked_add(1).ok_or(MemoryError::Capacity(
            "artifact identity capacity exhausted",
        ))?;
        // source writes can leave stale intervals until the index is rebuilt.
        if self.validity.completed_write.is_some() {
            self.validity.rebuild_sources();
        }
        for source in &sources {
            self.validity.index_source(source);
        }
        self.validity.owners.insert(
            owner,
            Owner {
                valid: true,
                sources,
            },
        );
        self.validity.next = next;
        self.refresh_watchers();
        Ok(owner)
    }

    pub fn owner_valid(&self, owner: u64) -> bool {
        self.validity
            .owners
            .get(&owner)
            .is_some_and(|owner| owner.valid)
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn take_source_invalidation(&mut self) -> bool {
        std::mem::take(&mut self.validity.invalidated)
    }

    pub fn activate(&mut self, owner: u64) -> bool {
        if !self.owner_valid(owner) {
            return false;
        }
        self.validity.active = Some(owner);
        self.validity.active_invalid = false;
        true
    }

    pub fn active_changed(&self) -> bool {
        self.validity.active_invalid
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn active_source_invalid_ptr(&self) -> u32 {
        (&raw const self.validity.active_invalid) as u32
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn source_is_active(&self, owner: u64) -> bool {
        self.validity.active == Some(owner) && !self.validity.active_invalid
    }

    /// Stop the active generated body while retaining published source subscriptions.
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn fence_active(&mut self) {
        if self.validity.active.take().is_some() {
            self.validity.active_invalid = true;
        }
    }

    pub fn retire_sources(&mut self) {
        self.validity.invalidated |= !self.validity.owners.is_empty();
        self.validity.owners.clear();
        self.validity.active = None;
        self.validity.active_invalid = false;
        self.validity.sources.clear();
        self.validity.completed_write = None;
        self.refresh_watchers();
    }

    /// A runtime fence must survive until the currently executing body exits.
    pub fn invalidate_sources(&mut self) {
        let active = self.validity.active.is_some() || self.validity.active_invalid;
        self.retire_sources();
        self.validity.active_invalid = active;
    }

    pub fn release(&mut self, owner: u64) {
        self.validity.owners.remove(&owner);
        if self.validity.active == Some(owner) {
            self.validity.active = None;
            self.validity.active_invalid = false;
        }
        self.validity.rebuild_sources();
        self.refresh_watchers();
    }

    pub(super) fn refresh_watchers(&mut self) {
        for backing in &mut self.backings {
            backing.has_sources = self
                .validity
                .sources
                .contains_key(&(Rc::as_ptr(&backing.incarnation) as usize));
        }
    }
}

#[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
impl Image {
    pub(crate) fn parallel_proof(&self, owner: u64) -> Option<Vec<(u32, u32)>> {
        let record = self.validity.owners.get(&owner)?;
        if !record.valid {
            return None;
        }
        record
            .sources
            .iter()
            .map(|source| {
                let backing = self
                    .backings
                    .iter()
                    .find(|backing| Rc::ptr_eq(&backing.incarnation, &source.incarnation))?;
                Some((backing.shared?.offset + source.offset, source.length))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
