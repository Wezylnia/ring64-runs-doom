use std::cell::{Cell, RefCell};

use super::{
    State,
    walk::{Failure, Request, Translation},
};
use crate::memory::Access;
use crate::x86::protection_keys::State as ProtectionKeys;

const CAPACITY: usize = 64;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
mod guarded_candidates;

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

#[cfg(not(feature = "parallel"))]
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RetainedSpan {
    Hit(u64),
    Miss,
    Refused,
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

    #[cfg(any(feature = "parallel", target_arch = "wasm32", test))]
    fn retained_data_span<const WRITE: bool>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
    ) -> Option<(usize, Translation)> {
        debug_assert_eq!(
            request.access,
            if WRITE { Access::Write } else { Access::Read }
        );
        let index = self.find(state, request)?;
        let translation = self.slots[index].unwrap().translation.at(request.linear);
        if WRITE && translation.dirty
            || translation.remaining < u64::from(width)
            || translation.rights.authorize(state, request, keys).is_err()
        {
            return None;
        }
        Some((index, translation))
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
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    guarded_candidates: RefCell<guarded_candidates::Candidates>,
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
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            guarded_candidates: RefCell::default(),
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

    #[cfg(feature = "parallel")]
    pub(super) fn touch_read_span(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
    ) -> Option<u64> {
        let mut entries = self.entries.borrow_mut();
        let (index, translation) =
            entries.retained_data_span::<false>(state, request, keys, width)?;
        entries.touch(index);
        Some(translation.physical)
    }

    #[cfg(not(feature = "parallel"))]
    #[inline(never)]
    pub(super) fn touch_read_span(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
    ) -> RetainedSpan {
        debug_assert_eq!(request.access, Access::Read);
        let mut entries = self.entries.borrow_mut();
        let Some(index) = entries.find(state, request) else {
            return RetainedSpan::Miss;
        };
        let translation = entries.slots[index].unwrap().translation.at(request.linear);
        if translation.remaining < u64::from(width)
            || translation.rights.authorize(state, request, keys).is_err()
        {
            return RetainedSpan::Refused;
        }
        entries.touch(index);
        RetainedSpan::Hit(translation.physical)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn data_then_fetch<const WRITE: bool, T>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
        local_only: bool,
        access: impl FnOnce(u64) -> Option<T>,
    ) -> Option<T> {
        self.data_translation_then_fetch::<WRITE, T>(
            state,
            request,
            keys,
            width,
            local_only,
            |translation| access(translation.physical),
        )
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline]
    pub(crate) fn data_translation_then_fetch<const WRITE: bool, T>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
        local_only: bool,
        access: impl FnOnce(Translation) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        // The admitted same-page loop makes the code entry most recent and
        // always has a successor fetch after a successful data access.
        let fetch = entries
            .hint
            .filter(|&index| entries.slots[index].is_some())?;
        let (data, translation) =
            entries.retained_data_span::<WRITE>(state, request, keys, width)?;
        // A local entry can shadow the interior of a retained global span.
        if local_only && entries.slots[data].unwrap().global {
            return None;
        }
        let value = access(translation)?;
        // A live hint is the entry last touched. The same entry or the two
        // already newest entries would keep their relative order.
        if data != fetch && entries.slots[data].as_ref().unwrap().used != entries.clock - 1 {
            entries.touch(data);
            entries.touch(fetch);
        }
        Some(value)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline]
    pub(crate) fn data_byte_then_fetch<const WRITE: bool, T>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        slot: Option<usize>,
        access: impl FnOnce(usize, Translation) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint
            .filter(|&index| entries.slots[index].is_some())?;
        let preferred = slot.and_then(|index| {
            let entry = entries.slots.get(index)?.as_ref()?;
            // A live local entry is unique for these coordinates. The normal
            // lookup must decide which translation shadows a retained global.
            (!entry.global
                && entry.family == family(state)
                && entry.pcid == state.pcid()
                && entry.translation.contains(request.linear))
            .then(|| (index, entry.translation.at(request.linear)))
        });
        let (data, translation) = if let Some((index, translation)) = preferred {
            if WRITE && translation.dirty
                || translation.remaining < 1
                || translation.rights.authorize(state, request, keys).is_err()
            {
                return None;
            }
            (index, translation)
        } else {
            entries.retained_data_span::<WRITE>(state, request, keys, 1)?
        };
        let value = access(data, translation)?;
        // Preserve the normal guarded data-then-fetch recency order.
        if data != fetch && entries.slots[data].as_ref().unwrap().used != entries.clock - 1 {
            entries.touch(data);
            entries.touch(fetch);
        }
        Some(value)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn read_dword_then_fetch(
        &self,
        image: &crate::memory::Image,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        peek: impl FnOnce() -> Option<Translation>,
    ) -> Option<u32> {
        debug_assert_eq!(request.access, Access::Read);
        debug_assert_eq!(request.linear & 3, 0);
        if state.cr0 & (1 << 31) == 0 {
            return None;
        }
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint
            .filter(|&index| entries.slots[index].is_some())?;
        let found = self
            .guarded_candidates
            .borrow_mut()
            .find(&entries, state, request);
        let Some(data) = found else {
            drop(entries);
            return self.fill_dword_miss_then_fetch(image, state, request, fetch, peek);
        };
        let translation = entries.slots[data].unwrap().translation.at(request.linear);
        if translation.remaining < 4 || translation.rights.authorize(state, request, keys).is_err()
        {
            return None;
        }
        let value = image.physical_read(translation.physical, 4).ok()?;
        if data != fetch && entries.slots[data].as_ref().unwrap().used != entries.clock - 1 {
            entries.touch(data);
            entries.touch(fetch);
        }
        Some(value as u32)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline(never)]
    fn fill_dword_miss_then_fetch(
        &self,
        image: &crate::memory::Image,
        state: &State,
        request: Request,
        fetch: usize,
        peek: impl FnOnce() -> Option<Translation>,
    ) -> Option<u32> {
        let entries = self.entries.borrow();
        // This private caller runs only in the admitted read-only loop.
        // Its last touch is the retained fetch for the entire code page.
        let code = entries.slots[fetch].as_ref()?;
        if entries.capacity < 2
            || entries.exhausted
            || entries.next_id == u64::MAX
            || code.used != entries.clock
            || code.translation.span != 4096
            || code.family != family(state)
            || !code.global && code.pcid != state.pcid()
        {
            return None;
        }
        let (translation, value) = image.probe_physical_read(|image| {
            let translation = peek()?;
            if !translation.accessed
                || translation.span != 4096
                || translation.linear_base & 4095 != 0
                || translation.remaining < 4
                || translation.global && state.cr4 & (1 << 7) != 0
                || overlaps(code.translation, translation)
            {
                return None;
            }
            let value = image.physical_read_local_dword(translation.physical)?;
            Some((translation, value))
        })?;
        drop(entries);
        // A nonoverlapping fill with at least two slots cannot evict the
        // newest entry. Commit the same fill and next fetch as checked execution.
        if self.guarded_candidates.borrow().only_4k_leaves() {
            self.install_known_local_miss(state, translation);
        } else {
            let installed = self.fill(state, request, translation);
            debug_assert!(installed.cache_id.is_some());
        }
        let mut entries = self.entries.borrow_mut();
        self.guarded_candidates
            .borrow_mut()
            .observe_fill(&entries, entries.hint.unwrap());
        entries.touch(fetch);
        Some(value)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    fn install_known_local_miss(&self, state: &State, translation: Translation) {
        // The indexed scan just proved the absence of any visible hit.
        // An aligned 4 KiB leaf in this cache can overlap only the same leaf.
        let mut entries = self.entries.borrow_mut();
        let mut oldest = 0;
        let mut age = u64::MAX;
        let vacancy = entries.slots[..entries.capacity]
            .iter()
            .enumerate()
            .find_map(|(index, slot)| match slot {
                None => Some(index),
                Some(entry) => {
                    if entry.used < age {
                        age = entry.used;
                        oldest = index;
                    }
                    None
                }
            });
        let index = vacancy.unwrap_or(oldest);
        if vacancy.is_none() {
            self.changed.set(true);
        }
        let mut translation = translation;
        translation.cache_id = Some(entries.next_id);
        entries.next_id += 1;
        entries.slots[index] = Some(Entry {
            translation,
            pcid: state.pcid(),
            global: false,
            family: family(state),
            used: 0,
        });
        entries.touch(index);
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn read_pair_then_fetch<T>(
        &self,
        state: &State,
        keys: &ProtectionKeys,
        [(first_request, first_width), (last_request, last_width)]: [(Request, u32); 2],
        access: impl FnOnce([u64; 2]) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint
            .filter(|&index| entries.slots[index].is_some())?;
        let (first, first_translation) =
            entries.retained_data_span::<false>(state, first_request, keys, first_width)?;
        let (last, last_translation) =
            entries.retained_data_span::<false>(state, last_request, keys, last_width)?;
        if entries.slots[first].unwrap().global || entries.slots[last].unwrap().global {
            return None;
        }
        let value = access([first_translation.physical, last_translation.physical])?;
        // A complete lap repeats these two data translations. Touch them in
        // the order of their last accesses, with the successor fetch last.
        for index in [first, last, fetch] {
            if entries.hint != Some(index) {
                entries.touch(index);
            }
        }
        Some(value)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn read_split_pair_then_fetch<T>(
        &self,
        state: &State,
        keys: &ProtectionKeys,
        spans: [(Request, u32); 2],
        access: impl FnOnce([u64; 2]) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint
            .filter(|&index| entries.slots[index].is_some())?;
        let mut prefix = None;
        let mut ends = [0; 2];
        let mut physical = [0; 2];
        for (slot, (request, bytes)) in spans.into_iter().enumerate() {
            let (first, start) = entries.retained_data_span::<false>(state, request, keys, 1)?;
            if entries.slots[first].as_ref()?.global {
                return None;
            }
            ends[slot] = first;
            physical[slot] = start.physical;
            if start.remaining >= u64::from(bytes) {
                continue;
            }
            // Exactly one stream may cross a leaf, outside its final lap.
            if prefix.is_some() || start.remaining > u64::from(bytes.checked_sub(16)?) {
                return None;
            }
            let next = Request {
                linear: request.linear.checked_add(start.remaining)?,
                ..request
            };
            let (last, end) = entries.retained_data_span::<false>(
                state,
                next,
                keys,
                bytes - start.remaining as u32,
            )?;
            if entries.slots[last].as_ref()?.global
                || start.physical.checked_add(start.remaining) != Some(end.physical)
            {
                return None;
            }
            prefix = Some(first);
            ends[slot] = last;
        }
        let prefix = prefix?;
        let value = access(physical)?;
        // In the funded final lap both streams touch their end leaves,
        // strictly after the prefix's last use. Aliases are touched in order.
        for index in [prefix, ends[0], ends[1], fetch] {
            if entries.hint != Some(index) {
                entries.touch(index);
            }
        }
        Some(value)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn contiguous_read_then_fetch<T>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
        access: impl FnOnce(u64) -> Option<T>,
    ) -> Option<T> {
        const PAGE: u64 = 4096;
        const MAX_PAGES: usize = 3;

        let width = u64::from(width);
        if width == 0 || width > PAGE * MAX_PAGES as u64 - (request.linear & (PAGE - 1)) {
            return None;
        }
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint
            .filter(|&index| entries.slots[index].is_some())?;
        let mut indices = [0; MAX_PAGES];
        let mut used = 0;
        let mut covered = 0;
        let mut physical_start = None;
        while covered < width {
            let linear = request.linear.checked_add(covered)?;
            let (index, translation) = entries.retained_data_span::<false>(
                state,
                Request { linear, ..request },
                keys,
                1,
            )?;
            if entries.slots[index].as_ref()?.global {
                return None;
            }
            let first = *physical_start.get_or_insert(translation.physical);
            if first.checked_add(covered) != Some(translation.physical) {
                return None;
            }
            *indices.get_mut(used)? = index;
            used += 1;
            covered += translation.remaining.min(width - covered);
        }
        if used < 2 {
            return None;
        }
        let value = access(physical_start?)?;
        // Every dense byte is read before leaving the funded loop. The final
        // touch on each data leaf ascends, and the successor fetch is newest.
        for index in indices[..used].iter().copied().chain([fetch]) {
            if entries.hint != Some(index) {
                entries.touch(index);
            }
        }
        Some(value)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline(never)]
    pub(crate) fn ready_dense_read_then_fetch<T>(
        &self,
        image: &crate::memory::Image,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
        access: impl FnOnce(u64) -> Option<T>,
    ) -> Option<T> {
        const PAGE: u64 = 4096;
        const MAX_PAGES: usize = 3;
        debug_assert_eq!(request.access, Access::Read);
        let width = u64::from(width);
        if state.cr0 & (1 << 31) == 0
            || width == 0
            || width > PAGE * MAX_PAGES as u64 - (request.linear & (PAGE - 1))
        {
            return None;
        }
        let entries = self.entries.borrow();
        let fetch = entries.hint?;
        let code = entries.slots[fetch].as_ref()?;
        if code.used != entries.clock || code.translation.span != PAGE || entries.exhausted {
            return None;
        }
        let mut plan = [None; MAX_PAGES];
        let value = image.probe_physical_read(|image| {
            let mut covered = 0;
            let mut missing = 0;
            let mut physical_start = None;
            for slot in &mut plan {
                if covered == width {
                    break;
                }
                let linear = request.linear.checked_add(covered)?;
                let operand = Request { linear, ..request };
                let index = entries.find(state, operand);
                let translation = if let Some(index) = index {
                    let entry = entries.slots[index].as_ref()?;
                    if entry.global {
                        return None;
                    }
                    let current = entry.translation.at(linear);
                    current.rights.authorize(state, operand, keys).ok()?;
                    current
                } else {
                    let current = super::walk::peek(image, state, operand, keys).ok()?;
                    if !current.accessed
                        || current.global && state.cr4 & (1 << 7) != 0
                        || overlaps(code.translation, current)
                    {
                        return None;
                    }
                    missing += 1;
                    current
                };
                if translation.span != PAGE {
                    return None;
                }
                let first = *physical_start.get_or_insert(translation.physical);
                if first.checked_add(covered) != Some(translation.physical) {
                    return None;
                }
                covered += translation.remaining.min(width - covered);
                *slot = Some((index, operand, translation));
            }
            // A true local 4 KiB miss cannot overlap another visible
            // translation. Require space so every planned hit survives.
            if covered != width
                || missing == 0
                || entries.next_id.checked_add(missing as u64).is_none()
                || entries.slots[..entries.capacity]
                    .iter()
                    .filter(|slot| slot.is_none())
                    .take(missing)
                    .count()
                    != missing
            {
                return None;
            }
            access(physical_start?)
        })?;
        drop(entries);
        // Admission covers every funded byte. Install and touch the leaves
        // in their original order, then make the successor fetch newest.
        for (index, operand, translation) in plan.into_iter().flatten() {
            if let Some(index) = index {
                self.entries.borrow_mut().touch(index);
            } else {
                let installed = self.fill(state, operand, translation);
                debug_assert!(installed.cache_id.is_some());
            }
        }
        self.entries.borrow_mut().touch(fetch);
        Some(value)
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
        let mut first_free = None;
        let mut oldest_index = 0;
        let mut oldest_age = u64::MAX;
        let mut overlaps_visible_entry = false;
        let capacity = entries.capacity;
        for (index, slot) in entries.slots[..capacity].iter_mut().enumerate() {
            let Some(entry) = slot.as_ref() else {
                first_free.get_or_insert(index);
                continue;
            };
            if entry.family == family
                && (entry.global || !global && entry.pcid == pcid)
                && overlaps(entry.translation, translation)
            {
                overlaps_visible_entry = true;
                // Local entries shadow overlapping globals for this PCID.
                // Other PCIDs can still use the global translation.
                if entry.global == global {
                    *slot = None;
                    first_free.get_or_insert(index);
                    continue;
                }
            }
            if entry.used < oldest_age {
                oldest_index = index;
                oldest_age = entry.used;
            }
        }
        let index = first_free.unwrap_or(oldest_index);
        if overlaps_visible_entry || first_free.is_none() {
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
