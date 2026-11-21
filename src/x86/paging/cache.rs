use std::cell::{Cell, RefCell};

use super::{
    State,
    walk::{Failure, Request, Translation},
};
use crate::memory::Access;
use crate::x86::protection_keys::State as ProtectionKeys;

const CAPACITY: usize = 64;

#[cfg(test)]
mod review_tests;

#[cfg(test)]
mod work_tests;

#[derive(Clone, Copy)]
struct Entry {
    translation: Translation,
    pcid: u16,
    global: bool,
    family: u8,
    used: u64,
}

struct Entries {
    slots: [Option<Entry>; CAPACITY],
    capacity: usize,
    clock: u64,
    next_id: u64,
    exhausted: bool,
    hint: Option<usize>,
}

impl Entries {
    fn find(&self, state: &State, request: Request) -> Option<usize> {
        let matches = |entry: &Entry| {
            entry.family == family(state)
                && (entry.global || entry.pcid == state.pcid())
                && entry.translation.contains(request.linear)
        };
        if let Some(index) = self.hint
            && let Some(entry) = self.slots[index]
            && !entry.global
            && matches(&entry)
        {
            return Some(index);
        }
        self.slots[..self.capacity]
            .iter()
            .position(|entry| entry.is_some_and(|e| !e.global && matches(&e)))
            .or_else(|| {
                self.slots[..self.capacity]
                    .iter()
                    .position(|entry| entry.is_some_and(|e| e.global && matches(&e)))
            })
    }

    fn touch(&mut self, index: usize) {
        if self.clock == u64::MAX {
            let mut ordered: Vec<_> = self.slots[..self.capacity]
                .iter()
                .enumerate()
                .filter_map(|(i, entry)| entry.map(|e| (i, e.used)))
                .collect();
            ordered.sort_unstable_by_key(|&(_, used)| used);
            for (age, &(i, _)) in ordered.iter().enumerate() {
                self.slots[i].as_mut().unwrap().used = age as u64 + 1;
            }
            self.clock = ordered.len() as u64;
        }
        self.clock += 1;
        self.slots[index].as_mut().unwrap().used = self.clock;
        self.hint = Some(index);
    }
}

/// One CPU's bounded retained translation authority; backing admission stays separate.
pub struct Cache {
    entries: RefCell<Entries>,
    changed: Cell<bool>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new(CAPACITY)
    }
}

impl Cache {
    fn new(capacity: usize) -> Self {
        assert!((1..=CAPACITY).contains(&capacity));
        Self {
            entries: RefCell::new(Entries {
                slots: [None; CAPACITY],
                capacity,
                clock: 0,
                next_id: 1,
                exhausted: false,
                hint: None,
            }),
            changed: Cell::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self::new(capacity)
    }

    pub fn lookup(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
    ) -> Result<Option<Translation>, Failure> {
        if state.cr0 & (1 << 31) == 0 {
            return Ok(None);
        }
        let mut entries = self.entries.borrow_mut();
        let Some(index) = entries.find(state, request) else {
            return Ok(None);
        };
        let entry = entries.slots[index].unwrap();
        if request.access == Access::Write && entry.translation.dirty {
            entries.slots[index] = None;
            self.changed.set(true);
            return Ok(None);
        }
        let translation = entry.translation.at(request.linear);
        if let Err(failure) = translation.rights.authorize(state, request, keys) {
            drop(entries);
            return Err(self.fault(state, failure));
        }
        entries.touch(index);
        Ok(Some(translation))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn touch_current_fetch(
        &self,
        state: &State,
        request: Request,
        physical: u64,
    ) -> bool {
        debug_assert_eq!(request.access, Access::Execute);
        let mut entries = self.entries.borrow_mut();
        let Some(index) = entries.find(state, request) else {
            return false;
        };
        let translation = entries.slots[index].unwrap().translation.at(request.linear);
        if translation.physical != physical
            || translation
                .rights
                .authorize(state, request, &ProtectionKeys::ZERO)
                .is_err()
        {
            return false;
        }
        entries.touch(index);
        true
    }

    pub(super) fn touch_read_span(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
    ) -> Option<u64> {
        debug_assert_eq!(request.access, Access::Read);
        let mut entries = self.entries.borrow_mut();
        let index = entries.find(state, request)?;
        let translation = entries.slots[index].unwrap().translation.at(request.linear);
        if translation.remaining < u64::from(width)
            || translation.rights.authorize(state, request, keys).is_err()
        {
            return None;
        }
        entries.touch(index);
        Some(translation.physical)
    }

    /// Pure retained-view projection: no A/D, fill, eviction or LRU effects.
    pub fn observe(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
    ) -> Result<Option<Translation>, Failure> {
        if state.cr0 & (1 << 31) == 0 {
            return Ok(None);
        }
        let entries = self.entries.borrow();
        let Some(index) = entries.find(state, request) else {
            return Ok(None);
        };
        let translation = entries.slots[index].unwrap().translation.at(request.linear);
        if request.access == Access::Write && translation.dirty {
            return Ok(None);
        }
        translation.rights.authorize(state, request, keys)?;
        Ok(Some(translation))
    }

    pub fn peek(
        &self,
        image: &crate::memory::Image,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
    ) -> Result<Translation, Failure> {
        match self.observe(state, request, keys)? {
            Some(translation) => Ok(translation),
            None => super::walk::peek(image, state, request, keys),
        }
    }

    /// Install only after the caller's complete physical/alignment admission.
    pub fn fill(
        &self,
        state: &State,
        request: Request,
        mut translation: Translation,
    ) -> Translation {
        if translation.cache_id.is_some() || translation.entry_width == 0 || !translation.accessed {
            return translation;
        }
        let mut entries = self.entries.borrow_mut();
        if entries.exhausted {
            return translation;
        }
        if entries.next_id == u64::MAX {
            // Exhaustion falls back to uncached walks rather than reusing captured IDs.
            entries.slots.fill(None);
            entries.hint = None;
            entries.exhausted = true;
            self.changed.set(true);
            return translation;
        }
        let pcid = state.pcid();
        let global = translation.global && state.cr4 & (1 << 7) != 0;
        let family = family(state);
        for slot in &mut entries.slots {
            if let Some(entry) = slot
                && entry.family == family
                && (entry.global || !global && entry.pcid == pcid)
                && overlaps(entry.translation, translation)
            {
                // Local entries shadow overlapping globals for this PCID.
                // Other PCIDs can still use the global translation.
                if entry.global == global {
                    *slot = None;
                }
                self.changed.set(true);
            }
        }
        let index = entries.slots[..entries.capacity]
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| {
                entries.slots[..entries.capacity]
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.unwrap().used)
                    .unwrap()
                    .0
            });
        if entries.slots[index].is_some() {
            self.changed.set(true);
        }
        translation.cache_id = Some(entries.next_id);
        entries.next_id += 1;
        entries.slots[index] = Some(Entry {
            translation,
            pcid,
            global,
            family,
            used: 0,
        });
        entries.touch(index);
        translation.at(request.linear)
    }

    pub fn mark_dirty(&self, cache_id: Option<u64>, leaf: u64) {
        let Some(id) = cache_id else {
            return;
        };
        for entry in self.entries.borrow_mut().slots.iter_mut().flatten() {
            if entry.translation.cache_id == Some(id) && entry.translation.leaf == leaf {
                entry.translation.dirty = false;
                break;
            }
        }
    }

    pub fn fault(&self, state: &State, failure: Failure) -> Failure {
        if let Failure::Page(fault) = failure {
            self.invalidate_page(state.pcid(), fault.address, true);
        }
        failure
    }

    pub fn invalidate_page(&self, pcid: u16, linear: u64, include_globals: bool) {
        self.remove(|entry| {
            entry.translation.contains(linear)
                && if entry.global {
                    include_globals
                } else {
                    entry.pcid == pcid
                }
        });
    }

    pub fn invalidate_pcid(&self, pcid: u16) {
        self.remove(|e| !e.global && e.pcid == pcid);
    }
    pub fn invalidate_non_global(&self) {
        self.remove(|e| !e.global);
    }
    pub fn clear(&self) {
        self.remove(|_| true);
    }
    pub fn take_changed(&self) -> bool {
        self.changed.replace(false)
    }

    fn remove(&self, predicate: impl Fn(&Entry) -> bool) {
        for slot in &mut self.entries.borrow_mut().slots {
            if slot.as_ref().is_some_and(&predicate) {
                *slot = None;
                self.changed.set(true);
            }
        }
    }
}

fn family(state: &State) -> u8 {
    u8::from(state.efer & (1 << 10) != 0) | (u8::from(state.cr4 & (1 << 5) != 0) << 1)
}

fn overlaps(a: Translation, b: Translation) -> bool {
    a.contains(b.linear_base) || b.contains(a.linear_base)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod boundary_tests;

#[cfg(test)]
mod family_tests;
