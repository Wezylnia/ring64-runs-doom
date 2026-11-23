use std::cell::{Cell, RefCell};
#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
use std::num::NonZeroU8;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
use crate::memory::DirectSpan;

use super::{
    State,
    walk::{Failure, Request, Translation},
};
use crate::memory::Access;
use crate::x86::protection_keys::State as ProtectionKeys;

pub(crate) const CAPACITY: usize = 64;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
const COPY_SOURCE_SLOTS: usize = 16;

mod lookup_index;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
mod recency;

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
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct FetchAuthority {
    pcid: u16,
    family: u8,
    user: bool,
    smep: bool,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
impl FetchAuthority {
    fn current(state: &State, request: Request) -> Self {
        Self {
            pcid: state.pcid(),
            family: family(state),
            user: request.class.is_user(),
            smep: state.cr4 & super::CR4_SMEP != 0,
        }
    }
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[derive(Clone, Copy)]
struct AdmittedFetch {
    index: usize,
    id: u64,
    linear_base: u64,
    physical_base: u64,
    authority: FetchAuthority,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[derive(Clone, Copy)]
struct IndexedWriteSlot {
    index: usize,
    id: u64,
    linear_base: u64,
    physical_base: u64,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[derive(Clone, Copy)]
struct DwordReadMemo {
    slot: usize,
    id: u64,
    span: DirectSpan,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Default)]
#[repr(C)]
pub(crate) struct IndexedDwordWrite {
    // Published for one synchronous guarded entry, after proving whole RAM pages.
    pub(crate) tags: [u64; 3],
    pub(crate) biases: [u32; 3],
    pub(crate) ages: [u32; 4],
    pub(crate) recency: u32,
    pub(crate) generation: u64,
    pub(crate) fetch: u32,
    pub(crate) index: u32,
    pub(crate) value: u32,
    pub(crate) host: u32,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Default)]
pub(crate) struct IndexedWriteMemo {
    index: [Option<IndexedWriteSlot>; 4],
    table: Option<IndexedWriteSlot>,
    target: Option<IndexedWriteSlot>,
    destination: Option<crate::memory::DirectSpan>,
    pub(crate) unsupported_leaf: bool,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) const INDEXED_READ_ENTRIES: usize = 256;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) const INDEXED_VALID: u64 = 1;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) const INDEXED_WRITE_READABLE: u64 = 2;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct IndexedReadEntry {
    pub(crate) tag: u64,
    pub(crate) slot_offset: u32,
    pub(crate) host_bias: u32,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
impl IndexedReadEntry {
    fn new(page: u64, epoch: u64, slot: usize, host: u32, readable_write: bool) -> Self {
        debug_assert_eq!(page & 4095, 0);
        debug_assert!(epoch & !0xffc == 0 && slot < CAPACITY && host != 0);
        let readable = if readable_write {
            INDEXED_WRITE_READABLE
        } else {
            0
        };
        Self {
            tag: page | epoch | INDEXED_VALID | readable,
            slot_offset: slot as u32 * 8,
            host_bias: host.wrapping_sub(page as u32),
        }
    }
}

// Generated Wasm and Rust use these same ages. The translation slot
// topology and the publication of read records stay Rust-owned.
#[repr(C)]
pub(crate) struct IndexedReadTable {
    pub(crate) clock: u64,
    pub(crate) ages: [u64; CAPACITY],
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) live_ids: [u64; CAPACITY],
    pub(crate) hint: u32,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) epoch: u64,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) generation: u64,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) entries: [IndexedReadEntry; INDEXED_READ_ENTRIES],
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) writes: [IndexedReadEntry; INDEXED_READ_ENTRIES],
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
impl IndexedReadTable {
    fn retire_pages(&mut self) {
        // The low tag bits carry the epoch without truncating canonicality.
        self.generation = self.generation.wrapping_add(1);
        self.epoch = (self.epoch + 4) & 0xffc;
        if self.epoch == 0 {
            self.entries.fill(IndexedReadEntry::default());
            self.writes.fill(IndexedReadEntry::default());
        }
    }
}

impl Default for IndexedReadTable {
    fn default() -> Self {
        Self {
            clock: 0,
            ages: [0; CAPACITY],
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            live_ids: [0; CAPACITY],
            hint: u32::MAX,
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            epoch: 0,
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            generation: 0,
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            entries: [IndexedReadEntry::default(); INDEXED_READ_ENTRIES],
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            writes: [IndexedReadEntry::default(); INDEXED_READ_ENTRIES],
        }
    }
}

struct Entries {
    slots: [Option<Entry>; CAPACITY],
    lookup_index: lookup_index::LookupIndex,
    capacity: usize,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    indexed_reads: Box<std::cell::UnsafeCell<IndexedReadTable>>,
    #[cfg(not(all(not(feature = "parallel"), any(target_arch = "wasm32", test))))]
    indexed_reads: IndexedReadTable,
    next_id: u64,
    exhausted: bool,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    admitted_fetch: Option<AdmittedFetch>,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
struct CopySpan {
    physical: u64,
    first: usize,
    split: Option<(u32, usize)>,
}

#[cfg(not(feature = "parallel"))]
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RetainedSpan {
    Hit(u64),
    Miss,
    Refused,
}

impl Entries {
    fn recency(&self) -> &IndexedReadTable {
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        // Generated Wasm accesses this only between synchronous Rust calls.
        unsafe {
            &*self.indexed_reads.get()
        }
        #[cfg(not(all(not(feature = "parallel"), any(target_arch = "wasm32", test))))]
        &self.indexed_reads
    }

    fn recency_mut(&mut self) -> &mut IndexedReadTable {
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        return self.indexed_reads.get_mut();
        #[cfg(not(all(not(feature = "parallel"), any(target_arch = "wasm32", test))))]
        &mut self.indexed_reads
    }

    fn hint(&self) -> Option<usize> {
        let hint = self.recency().hint;
        (hint != u32::MAX).then_some(hint as usize)
    }

    fn set_hint(&mut self, hint: Option<usize>) {
        self.recency_mut().hint = hint.map_or(u32::MAX, |index| index as u32);
    }

    fn find(&self, state: &State, request: Request) -> Option<usize> {
        let family = family(state);
        let pcid = state.pcid();
        let matches = |entry: &Entry| {
            entry.family == family
                && (entry.global || entry.pcid == pcid)
                && entry.translation.contains(request.linear)
        };
        if let Some(index) = self.hint()
            && let Some(entry) = self.slots[index].as_ref()
            && !entry.global
            && matches(entry)
        {
            return Some(index);
        }
        let mut candidates = self.lookup_index.candidates(request.linear);
        let mut global = None;
        while candidates != 0 {
            let index = candidates.trailing_zeros() as usize;
            candidates &= candidates - 1;
            let entry = self.slots[index].as_ref().unwrap();
            if matches(entry) {
                if !entry.global {
                    return Some(index);
                }
                global.get_or_insert(index);
            }
        }
        global
    }

    fn insert(&mut self, index: usize, entry: Entry) {
        self.lookup_index.insert(index, &entry);
        self.recency_mut().ages[index] = 0;
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        {
            if self.slots[index].is_some() {
                self.recency_mut().retire_pages();
            }
            self.recency_mut().live_ids[index] = entry.translation.cache_id.unwrap_or(0);
        }
        self.slots[index] = Some(entry);
    }

    fn remove(&mut self, index: usize) {
        self.lookup_index.remove(index);
        self.recency_mut().ages[index] = 0;
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        {
            self.recency_mut().retire_pages();
            self.recency_mut().live_ids[index] = 0;
        }
        self.slots[index] = None;
    }

    #[cfg(all(not(feature = "parallel"), test))]
    fn find_linear_reference(&self, state: &State, request: Request) -> Option<usize> {
        let matches = |entry: &Entry| {
            entry.family == family(state)
                && (entry.global || entry.pcid == state.pcid())
                && entry.translation.contains(request.linear)
        };
        if let Some(index) = self.hint()
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
        let index = self.find(state, request)?;
        let translation =
            self.retained_data_span_at::<WRITE>(state, request, keys, width, index)?;
        Some((index, translation))
    }

    #[cfg(any(feature = "parallel", target_arch = "wasm32", test))]
    fn retained_data_span_at<const WRITE: bool>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
        index: usize,
    ) -> Option<Translation> {
        let translation = &self.slots[index].as_ref().unwrap().translation;
        Self::checked_data_extent::<WRITE>(translation, state, request, keys, width)?;
        Some(translation.at(request.linear))
    }

    #[cfg(any(feature = "parallel", target_arch = "wasm32", test))]
    #[inline]
    fn checked_data_extent<const WRITE: bool>(
        translation: &Translation,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
    ) -> Option<(u64, u64)> {
        debug_assert_eq!(
            request.access,
            if WRITE { Access::Write } else { Access::Read }
        );
        let offset = request.linear - translation.linear_base;
        let remaining = translation.span - offset;
        if WRITE && translation.dirty
            || remaining < u64::from(width)
            || !translation
                .rights
                .allows_data(state, request.class, keys, WRITE)
        {
            return None;
        }
        Some((translation.physical_base + offset, remaining))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    fn retained_copy_span<const WRITE: bool>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
    ) -> Option<CopySpan> {
        let (first, start) = self.retained_data_span::<WRITE>(state, request, keys, 1)?;
        if self.slots[first].as_ref()?.global {
            return None;
        }
        let split = if start.remaining < u64::from(width) {
            let next = Request {
                linear: request.linear.checked_add(start.remaining)?,
                ..request
            };
            let offset = start.remaining as u32;
            let (last, end) =
                self.retained_data_span::<WRITE>(state, next, keys, width - offset)?;
            if self.slots[last].as_ref()?.global
                || start.physical.checked_add(start.remaining) != Some(end.physical)
            {
                return None;
            }
            Some((offset, last))
        } else {
            None
        };
        Some(CopySpan {
            physical: start.physical,
            first,
            split,
        })
    }

    fn touch(&mut self, index: usize) {
        if self.recency().clock == u64::MAX {
            let mut ordered: Vec<_> = self.slots[..self.capacity]
                .iter()
                .enumerate()
                .filter_map(|(i, entry)| entry.map(|_| (i, self.recency().ages[i])))
                .collect();
            ordered.sort_unstable_by_key(|&(_, used)| used);
            for (age, &(i, _)) in ordered.iter().enumerate() {
                self.recency_mut().ages[i] = age as u64 + 1;
            }
            self.recency_mut().clock = ordered.len() as u64;
        }
        self.recency_mut().clock += 1;
        self.recency_mut().ages[index] = self.recency().clock;
        self.set_hint(Some(index));
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline]
    fn indexed_write_span<const WRITE: bool>(
        &self,
        slot: &mut Option<IndexedWriteSlot>,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
        unsupported_leaf: &mut bool,
    ) -> Option<(usize, u64)> {
        if let Some(retained) = *slot {
            let delta = request.linear.wrapping_sub(retained.linear_base);
            if delta <= 4096 - u64::from(width)
                && self.recency().live_ids[retained.index] == retained.id
            {
                return Some((retained.index, retained.physical_base + delta));
            }
        }
        let (index, t) = self.retained_data_span::<WRITE>(state, request, keys, width)?;
        if t.global || t.span != 4096 {
            *unsupported_leaf = true;
            return None;
        }
        // Until this guarded group exits, replacement is the only way a
        // checked local leaf can lose the authority admitted for this operand.
        *slot = t.cache_id.map(|id| IndexedWriteSlot {
            index,
            id,
            linear_base: t.linear_base,
            physical_base: t.physical_base,
        });
        Some((index, t.physical))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    fn touch_indexed_group(&mut self, slots: [usize; 4]) {
        let fetch = slots[3];
        debug_assert_eq!(self.hint(), Some(fetch));
        // The group's last fetch restores the incoming hint. Keep the
        // intermediate clock and hint local unless the clock needs rebasing.
        let recency = self.recency_mut();
        if recency.clock <= u64::MAX - 4 {
            let mut previous = fetch;
            let mut clock = recency.clock;
            for slot in slots {
                if previous != slot {
                    clock += 1;
                    recency.ages[slot] = clock;
                }
                previous = slot;
            }
            recency.clock = clock;
        } else {
            for slot in slots {
                if self.hint() != Some(slot) {
                    self.touch(slot);
                }
            }
        }
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    fn touch_data_and_fetch(&mut self, data: usize, fetch: usize) {
        debug_assert_eq!(self.hint(), Some(fetch));
        debug_assert_ne!(data, fetch);
        if self.recency().clock > u64::MAX - 2 {
            self.touch(data);
            self.touch(fetch);
            return;
        }
        self.recency_mut().ages[data] = self.recency().clock + 1;
        self.recency_mut().clock += 2;
        self.recency_mut().ages[fetch] = self.recency().clock;
    }
}

/// One CPU's bounded retained translation authority; backing admission stays separate.
pub struct Cache {
    entries: RefCell<Entries>,
    changed: Cell<bool>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    loop_reads_invalidated: Cell<bool>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    guarded_order: RefCell<Option<recency::GuardedOrder>>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    guarded_read_window: Cell<Option<(usize, u64)>>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    last_dword_write: Cell<Option<NonZeroU8>>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    last_scalar_read: Cell<Option<NonZeroU8>>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    last_dword_read: Cell<Option<DwordReadMemo>>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    last_copy_destination: Cell<usize>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    copy_source_slots: [Cell<u8>; COPY_SOURCE_SLOTS],
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
                lookup_index: lookup_index::LookupIndex::default(),
                capacity,
                indexed_reads: Default::default(),
                next_id: 1,
                exhausted: false,
                #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
                admitted_fetch: None,
            }),
            changed: Cell::new(false),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            loop_reads_invalidated: Cell::new(false),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            guarded_order: RefCell::new(None),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            guarded_read_window: Cell::new(None),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            last_dword_write: Cell::new(None),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            last_scalar_read: Cell::new(None),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            last_dword_read: Cell::new(None),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            last_copy_destination: Cell::new(usize::MAX),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            copy_source_slots: std::array::from_fn(|_| Cell::new(u8::MAX)),
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn indexed_reads_ptr(&self) -> u32 {
        self.entries.borrow().indexed_reads.get() as u32
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn recent_indexed_read(
        &self,
        state: &State,
        linear: u64,
    ) -> Option<(usize, Translation)> {
        let entries = self.entries.borrow();
        let slot = entries.hint()?;
        let entry = entries.slots[slot]?;
        (!entry.global
            && entry.family == family(state)
            && entry.pcid == state.pcid()
            && entry.translation.contains(linear))
        .then(|| (slot, entry.translation.at(linear)))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn clear_indexed_pages(&self) {
        let mut entries = self.entries.borrow_mut();
        let table = entries.recency_mut();
        table.entries.fill(IndexedReadEntry::default());
        table.writes.fill(IndexedReadEntry::default());
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn clear_indexed_writes(&self) {
        self.entries
            .borrow_mut()
            .recency_mut()
            .writes
            .fill(IndexedReadEntry::default());
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn remember_indexed_read(&self, slot: usize, linear: u64, host: u32) {
        self.remember_indexed_page(slot, linear, host, None);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn remember_indexed_write(
        &self,
        slot: usize,
        linear: u64,
        host: u32,
        readable: bool,
    ) {
        self.remember_indexed_page(slot, linear, host, Some(readable));
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    fn remember_indexed_page(&self, slot: usize, linear: u64, host: u32, write: Option<bool>) {
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.slots.get(slot).copied().flatten() else {
            return;
        };
        if entry.global
            || host == 0
            || linear & 4095 != 0
            || !entry.translation.contains(linear)
            || !entry.translation.contains(linear | 4095)
        {
            return;
        }
        let table = entries.recency_mut();
        let index = (linear >> 12) as usize & (INDEXED_READ_ENTRIES - 1);
        let record = IndexedReadEntry::new(linear, table.epoch, slot, host, write.unwrap_or(false));
        if write.is_some() {
            table.writes[index] = record;
        } else {
            table.entries[index] = record;
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn index_last_guarded_dword_read(&self, address: u64, host: u32) {
        let Some(memo) = self.last_dword_read.get() else {
            return;
        };
        let page = address & !4095;
        let Some(delta) = page.checked_sub(memo.span.base) else {
            return;
        };
        let Some(last) = memo.span.length.checked_sub(4096) else {
            return;
        };
        if address - page > 4092
            || delta > u64::from(last)
            || u64::from(memo.span.host) + (address - memo.span.base) != u64::from(host)
            || self.entries.borrow().recency().live_ids[memo.slot] != memo.id
        {
            return;
        }
        self.remember_indexed_read(memo.slot, page, memo.span.host + delta as u32);
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
            entries.remove(index);
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
    #[inline(always)]
    pub(crate) fn touch_current_fetch(
        &self,
        state: &State,
        request: Request,
        physical: u64,
    ) -> bool {
        debug_assert_eq!(request.access, Access::Execute);
        const PAGE: u64 = 4096;
        let mut entries = self.entries.borrow_mut();
        if let Some(fetch) = entries.admitted_fetch.as_ref()
            && fetch.linear_base == (request.linear & !(PAGE - 1))
            && (fetch.physical_base | (request.linear & (PAGE - 1))) == physical
            && fetch.authority == FetchAuthority::current(state, request)
            && let Some(entry) = entries.slots[fetch.index].as_ref()
            && entry.translation.cache_id == Some(fetch.id)
        {
            // This local leaf is still the lookup winner. Fetching the newest
            // leaf again cannot change eviction order.
            let index = fetch.index;
            if entries.hint() != Some(index)
                || entries.recency().ages[index] != entries.recency().clock
                || entries.recency().clock == u64::MAX
            {
                entries.touch(index);
            }
            return true;
        }
        Self::touch_unadmitted_fetch(&mut entries, state, request, physical)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[cold]
    fn touch_unadmitted_fetch(
        entries: &mut Entries,
        state: &State,
        request: Request,
        physical: u64,
    ) -> bool {
        const PAGE: u64 = 4096;
        let Some(index) = entries.find(state, request) else {
            return false;
        };
        let entry = entries.slots[index].as_ref().unwrap();
        let translation = &entry.translation;
        if translation.physical_base + (request.linear - translation.linear_base) != physical
            || !translation.rights.allows_execute(state, request.class)
        {
            return false;
        }
        // Cache the checked 4 KiB subpage under its original leaf identity.
        // A local leaf can stay live only while it still wins lookup.
        if !entry.global
            && translation.span >= PAGE
            && translation.linear_base & (PAGE - 1) == 0
            && translation.physical_base & (PAGE - 1) == 0
            && let Some(id) = translation.cache_id
        {
            entries.admitted_fetch = Some(AdmittedFetch {
                index,
                id,
                linear_base: request.linear & !(PAGE - 1),
                physical_base: physical & !(PAGE - 1),
                authority: FetchAuthority::current(state, request),
            });
        }
        entries.touch(index);
        true
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn current_fetch_slot(&self, linear: u64) -> Option<(u32, u64)> {
        let entries = self.entries.borrow();
        let fetch = entries.admitted_fetch.as_ref()?;
        (fetch.linear_base == linear & !4095
            && entries.hint() == Some(fetch.index)
            && entries.recency().live_ids[fetch.index] == fetch.id)
            .then_some((fetch.index as u32, fetch.id))
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
    pub(crate) fn write_retained_span(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        width: u32,
        write: impl FnOnce(u64) -> Option<()>,
    ) -> bool {
        let mut entries = self.entries.borrow_mut();
        // The stock path renumbers entries before publishing any payload.
        if entries.recency().clock == u64::MAX {
            return false;
        }
        let Some((slot, translation)) =
            entries.retained_data_span::<true>(state, request, keys, width)
        else {
            return false;
        };
        if write(translation.physical).is_none() {
            return false;
        }
        entries.touch(slot);
        true
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
            |_, translation| access(translation.physical),
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
        access: impl FnOnce(usize, Translation) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        // The admitted same-page loop makes the code entry most recent and
        // always has a successor fetch after a successful data access.
        let fetch = entries
            .hint()
            .filter(|&index| entries.slots[index].is_some())?;
        let (data, translation) =
            entries.retained_data_span::<WRITE>(state, request, keys, width)?;
        // A local entry can shadow the interior of a retained global span.
        if local_only && entries.slots[data].unwrap().global {
            return None;
        }
        let value = access(data, translation)?;
        // A live hint is the entry last touched. The same entry or the two
        // already newest entries would keep their relative order.
        if data != fetch && entries.recency().ages[data] != entries.recency().clock - 1 {
            entries.touch_data_and_fetch(data, fetch);
        }
        Some(value)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn remember_guarded_read_window(&self, slot: usize, id: u64) {
        self.guarded_read_window.set(Some((slot, id)));
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn retouch_guarded_read_window(&self, linear: u64) -> Option<()> {
        self.retouch_guarded_read_binding(self.guarded_read_window.get()?, linear)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline(always)]
    pub(crate) fn retouch_guarded_read_binding(
        &self,
        (slot, id): (usize, u64),
        linear: u64,
    ) -> Option<()> {
        let mut entries = self.entries.borrow_mut();
        let entry = entries.slots[slot].as_ref()?;
        if entry.translation.cache_id != Some(id) || !entry.translation.contains(linear) {
            return None;
        }
        let fetch = entries
            .hint()
            .filter(|&index| entries.slots[index].is_some())?;
        if slot != fetch && entries.recency().ages[slot] != entries.recency().clock - 1 {
            entries.touch(slot);
            entries.touch(fetch);
        }
        Some(())
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn indexed_dword_write_then_fetch(
        &self,
        image: &mut crate::memory::Image,
        state: &State,
        keys: &ProtectionKeys,
        index: Request,
        (table, store): (u64, Request),
        memo: &mut IndexedWriteMemo,
    ) -> Option<IndexedDwordWrite> {
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint()
            .filter(|&slot| entries.slots[slot].is_some())?;
        // The guarded clone cannot change paging, permissions or access class.
        // Local slot generations still distinguish a replacement or eviction.
        let index_memo = (index.linear >> 12) as usize % memo.index.len();
        let (index_slot, first) = entries.indexed_write_span::<false>(
            &mut memo.index[index_memo],
            state,
            index,
            keys,
            1,
            &mut memo.unsupported_leaf,
        )?;
        let index_value = image.physical_read_mapped_byte(first)?;
        if index_value == 0 {
            if index_slot != fetch
                && entries.recency().ages[index_slot] != entries.recency().clock - 1
            {
                entries.touch_data_and_fetch(index_slot, fetch);
            }
            return Some(IndexedDwordWrite::default());
        }
        let table = Request {
            linear: table.wrapping_add(u64::from(index_value) * 4),
            ..index
        };
        if table.linear & 3 != 0
            || !crate::memory::canonical48(table.linear)
            || store.linear & 3 != 0
            || !crate::memory::canonical48(store.linear)
        {
            return None;
        }
        let (table_slot, lookup) = entries.indexed_write_span::<false>(
            &mut memo.table,
            state,
            table,
            keys,
            4,
            &mut memo.unsupported_leaf,
        )?;
        let (store_slot, target) = entries.indexed_write_span::<true>(
            &mut memo.target,
            state,
            store,
            keys,
            4,
            &mut memo.unsupported_leaf,
        )?;
        let value = image.physical_read_mapped_dword(lookup)?;
        let span = match memo.destination.as_ref() {
            Some(span) if target.wrapping_sub(span.base) <= u64::from(span.length - 4) => span,
            _ => memo
                .destination
                .insert(image.physical_direct_write_span(target, 4)?),
        };
        let host = span.host + (target - span.base) as u32;

        // No retained age changes before all operations are admitted. After
        // the straight group, the last uses are these operands and code.
        entries.touch_indexed_group([index_slot, table_slot, store_slot, fetch]);
        let mut proof = IndexedDwordWrite {
            index: u32::from(index_value),
            value,
            host,
            ..Default::default()
        };
        let slots = [index_slot, table_slot, store_slot, fetch];
        let target_page = target & !4095;
        let offset = target_page.wrapping_sub(span.base);
        if span.length < 4096
            || offset > u64::from(span.length - 4096)
            || index_slot == fetch
            || index_slot == table_slot
            || table_slot == store_slot
            || store_slot == fetch
        {
            return Some(proof);
        }
        if let (Some(source), Some(palette)) = (
            image.physical_direct_read_pointer(first & !4095, 4096),
            image.physical_direct_read_pointer(lookup & !4095, 4096),
        ) {
            let pages = [index.linear, table.linear, store.linear].map(|address| address & !4095);
            let hosts = [source, palette, span.host + offset as u32];
            proof.tags = pages.map(|page| page | 4);
            proof.biases = std::array::from_fn(|i| hosts[i].wrapping_sub(pages[i] as u32));
            let table = entries.recency_mut();
            proof.recency = table as *mut _ as u32;
            proof.ages = slots.map(|slot| &raw mut table.ages[slot] as u32);
            proof.generation = table.generation;
            proof.fetch = fetch as u32;
        }
        Some(proof)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn copy_pair_then_fetch<T>(
        &self,
        state: &State,
        [read, write]: [Request; 2],
        keys: &ProtectionKeys,
        width: u32,
        admit: impl FnOnce([(u64, u64); 2], [usize; 2]) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint()
            .filter(|&index| entries.slots[index].is_some())?;
        let last = self.last_copy_destination.get();
        // The preceding write often stays on one leaf while reads move between pages.
        // A matching local entry is unique; globals still need the ordinary lookup.
        let destination = if let Some(Some(entry)) = entries.slots.get(last)
            && !entry.global
            && entry.family == family(state)
            && entry.pcid == state.pcid()
            && entry.translation.contains(write.linear)
        {
            last
        } else {
            entries.find(state, write)?
        };
        let write_entry = entries.slots[destination].as_ref()?;
        if write_entry.global {
            return None;
        }
        // Both operands often resolve to one large local leaf.
        let shared =
            write_entry.translation.span > 4096 && write_entry.translation.contains(read.linear);
        let source_slot =
            &self.copy_source_slots[(read.linear >> 12) as usize & (COPY_SOURCE_SLOTS - 1)];
        let hint = usize::from(source_slot.get());
        let source = if shared {
            destination
        } else if let Some(Some(entry)) = entries.slots.get(hint)
            && !entry.global
            && entry.family == write_entry.family
            && entry.pcid == write_entry.pcid
            && entry.translation.contains(read.linear)
        {
            hint
        } else {
            entries.find(state, read)?
        };
        let read_entry = entries.slots[source].as_ref()?;
        if read_entry.global {
            return None;
        }
        let read = Entries::checked_data_extent::<false>(
            &read_entry.translation,
            state,
            read,
            keys,
            width,
        )?;
        let write = Entries::checked_data_extent::<true>(
            &write_entry.translation,
            state,
            write,
            keys,
            width,
        )?;
        let value = admit([read, write], [source, destination])?;
        // An alternating read/write group has this recency whenever it completes.
        // Further accesses in the same pair of leaves preserve that order.
        for data in [source, destination] {
            if data != fetch && entries.recency().ages[data] != entries.recency().clock - 1 {
                entries.touch_data_and_fetch(data, fetch);
            }
        }
        self.last_copy_destination.set(destination);
        if !shared {
            source_slot.set(source as u8);
        }
        Some(value)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[cold]
    pub(crate) fn ready_copy_destination(
        &self,
        image: &mut crate::memory::Image,
        state: &State,
        [read, write]: [Request; 2],
        keys: &ProtectionKeys,
        width: u32,
        admit: impl FnOnce(&mut crate::memory::Image, u64, u64, u64, &Translation) -> Option<()>,
    ) -> Option<()> {
        let entries = self.entries.borrow();
        let fetch = entries.hint()?;
        let code = entries.slots[fetch].as_ref()?;
        if entries.capacity < 3
            || entries.exhausted
            || entries.next_id == u64::MAX
            || !entries.lookup_index.only_4k_leaves()
            || entries.recency().ages[fetch] != entries.recency().clock
            || code.family != family(state)
            || !code.global && code.pcid != state.pcid()
        {
            return None;
        }
        let (source, retained) = entries.retained_data_span::<false>(state, read, keys, width)?;
        if entries.slots[source].as_ref()?.global {
            return None;
        }
        let first = entries.find(state, write);
        let (prefix, physical) = if let Some(slot) = first {
            let entry = entries.slots[slot].as_ref()?;
            let (physical, remaining) =
                Entries::checked_data_extent::<true>(&entry.translation, state, write, keys, 1)?;
            if entry.global || remaining >= u64::from(width) || entries.capacity < 4 {
                return None;
            }
            (remaining, Some(physical))
        } else {
            (0, None)
        };
        let next = Request {
            linear: write.linear.checked_add(prefix)?,
            ..write
        };
        if prefix != 0 && entries.find(state, next).is_some() {
            return None;
        }
        let destination = image.probe_physical_read(|image| {
            let translation = super::walk::peek(image, state, next, keys).ok()?;
            (translation.span == 4096
                && translation.accessed
                && !translation.dirty
                && translation.remaining >= u64::from(width) - prefix
                && !(translation.global && state.cr4 & (1 << 7) != 0)
                && physical.is_none_or(|first| first + prefix == translation.physical))
            .then_some(translation)
        })?;
        admit(
            image,
            retained.physical,
            physical.unwrap_or(destination.physical),
            prefix,
            &destination,
        )?;
        drop(entries);
        // The checked reads, writes and intervening fetches precede this fill.
        // Keep all operands of the group resident across its one allocation.
        for slot in [Some(source), first].into_iter().flatten() {
            if slot != fetch {
                self.entries.borrow_mut().touch_data_and_fetch(slot, fetch);
            }
        }
        self.install_known_local_miss(state, destination, fetch);
        Some(())
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[cold]
    #[inline(never)]
    pub(crate) fn copy_split_pair_then_fetch<T>(
        &self,
        state: &State,
        [read, write]: [Request; 2],
        keys: &ProtectionKeys,
        width: u32,
        widths: [u32; 4],
        admit: impl FnOnce(u64, u64) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint()
            .filter(|&index| entries.slots[index].is_some())?;
        let spans = [
            entries.retained_copy_span::<false>(state, read, keys, width)?,
            entries.retained_copy_span::<true>(state, write, keys, width)?,
        ];
        let value = admit(spans[0].physical, spans[1].physical)?;
        // Each admitted load and store touches its operand leaves before the next fetch.
        let mut offset = 0;
        for part in widths {
            if offset == width {
                break;
            }
            if part == 0 {
                continue;
            }
            for span in &spans {
                let first = span.split.is_none_or(|(split, _)| offset < split);
                if first && entries.hint() != Some(span.first) {
                    entries.touch(span.first);
                }
                if let Some((split, last)) = span.split
                    && offset + part > split
                    && entries.hint() != Some(last)
                {
                    entries.touch(last);
                }
                if entries.hint() != Some(fetch) {
                    entries.touch(fetch);
                }
            }
            offset += part;
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
        self.data_with_slot_then_fetch::<WRITE, 1, false, T>(
            state, request, keys, slot, None, access,
        )
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline]
    pub(crate) fn read_scalar_then_fetch<const WIDTH: u32, T>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        access: impl FnOnce(u64) -> Option<T>,
    ) -> Option<T> {
        let slot = self
            .last_scalar_read
            .get()
            .map(|slot| usize::from(slot.get() - 1));
        self.data_with_slot_then_fetch::<false, WIDTH, false, T>(
            state,
            request,
            keys,
            slot,
            Some(&self.last_scalar_read),
            |_, translation| access(translation.physical),
        )
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline]
    pub(crate) fn read_local_dword_then_fetch(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        admit: impl FnOnce(u64) -> Option<DirectSpan>,
    ) -> Option<u32> {
        let memo = self.last_dword_read.get();
        if let Some(memo) = memo {
            let delta = request.linear.wrapping_sub(memo.span.base);
            if delta <= u64::from(memo.span.length - 4) {
                let mut entries = self.entries.borrow_mut();
                let fetch = entries
                    .hint()
                    .filter(|&index| entries.slots[index].is_some())?;
                if let Some(data) = entries.slots[memo.slot].as_ref()
                    && data.translation.cache_id == Some(memo.id)
                    && data.family == family(state)
                    && data.pcid == state.pcid()
                {
                    data.translation
                        .rights
                        .authorize(state, request, keys)
                        .ok()?;
                    if memo.slot != fetch
                        && entries.recency().ages[memo.slot] != entries.recency().clock - 1
                    {
                        entries.touch_data_and_fetch(memo.slot, fetch);
                    }
                    return Some(memo.span.host + delta as u32);
                }
            }
        }
        self.memo_dword_then_fetch(state, request, keys, memo.map(|memo| memo.slot), admit)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[cold]
    #[inline(never)]
    fn memo_dword_then_fetch(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        slot: Option<usize>,
        admit: impl FnOnce(u64) -> Option<DirectSpan>,
    ) -> Option<u32> {
        self.data_with_slot_then_fetch::<false, 4, true, u32>(
            state,
            request,
            keys,
            slot,
            None,
            |slot, translation| {
                let span = admit(translation.physical)?;
                // Host mapping changes clear retained translations before resuming.
                // Intersect the backing with this exact local translation.
                let start = span.base.max(translation.physical_base);
                let backing_delta = (start - span.base) as u32;
                let physical_delta = start - translation.physical_base;
                let length = u64::from(span.length - backing_delta)
                    .min(translation.span - physical_delta) as u32;
                if let Some(id) = translation.cache_id {
                    self.last_dword_read.set(Some(DwordReadMemo {
                        slot,
                        id,
                        span: DirectSpan {
                            base: translation.linear_base + physical_delta,
                            length,
                            host: span.host + backing_delta,
                        },
                    }));
                }
                Some(span.host + (translation.physical - span.base) as u32)
            },
        )
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline]
    pub(crate) fn write_dword_then_fetch<T>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        access: impl FnOnce(u64) -> Option<T>,
    ) -> Option<T> {
        let slot = self
            .last_dword_write
            .get()
            .map(|slot| usize::from(slot.get() - 1));
        self.data_with_slot_then_fetch::<true, 4, false, T>(
            state,
            request,
            keys,
            slot,
            Some(&self.last_dword_write),
            |_, translation| access(translation.physical),
        )
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline]
    fn data_with_slot_then_fetch<const WRITE: bool, const WIDTH: u32, const LOCAL_ONLY: bool, T>(
        &self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        slot: Option<usize>,
        remember: Option<&Cell<Option<NonZeroU8>>>,
        access: impl FnOnce(usize, Translation) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint()
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
                || translation.remaining < u64::from(WIDTH)
                || translation.rights.authorize(state, request, keys).is_err()
            {
                return None;
            }
            (index, translation)
        } else {
            entries.retained_data_span::<WRITE>(state, request, keys, WIDTH)?
        };
        if LOCAL_ONLY && entries.slots[data].unwrap().global {
            return None;
        }
        let value = access(data, translation)?;
        // Preserve the normal guarded data-then-fetch recency order.
        if data != fetch && entries.recency().ages[data] != entries.recency().clock - 1 {
            entries.touch_data_and_fetch(data, fetch);
        }
        if let Some(remember) = remember.filter(|_| !entries.slots[data].unwrap().global) {
            const { assert!(CAPACITY < u8::MAX as usize) };
            // Cache an index only. Each use matches the live occupant anew.
            remember.set(NonZeroU8::new(data as u8 + 1));
        }
        Some(value)
    }

    #[cfg(all(not(feature = "parallel"), test))]
    fn read_dword_then_fetch(
        &self,
        image: &crate::memory::Image,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        peek: impl FnOnce() -> Option<Translation>,
    ) -> Option<u32> {
        self.read_guarded_dword_then_fetch(image, state, request, keys, peek)
            .map(|(value, _)| value)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn read_guarded_dword_then_fetch(
        &self,
        image: &crate::memory::Image,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
        peek: impl FnOnce() -> Option<Translation>,
    ) -> Option<(u32, Translation)> {
        debug_assert_eq!(request.access, Access::Read);
        debug_assert_eq!(request.linear & 3, 0);
        if state.cr0 & (1 << 31) == 0 {
            return None;
        }
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint()
            .filter(|&index| entries.slots[index].is_some())?;
        // A guarded loop keeps the fetch in the hint. Most data misses can
        // skip matching that entry and decoding its address-space identity.
        let candidates = entries.lookup_index.candidates(request.linear);
        let found = (candidates != 0)
            .then(|| entries.find(state, request))
            .flatten();
        let Some(data) = found else {
            drop(entries);
            return self.fill_dword_miss_then_fetch(image, state, request, fetch, peek);
        };
        let translation = entries.slots[data].unwrap().translation.at(request.linear);
        if translation.remaining < 4 || translation.rights.authorize(state, request, keys).is_err()
        {
            return None;
        }
        let value = match image.physical_read_mapped_dword(translation.physical) {
            Some(value) => value,
            None => image.physical_read(translation.physical, 4).ok()? as u32,
        };
        if data != fetch && entries.recency().ages[data] != entries.recency().clock - 1 {
            let mut order = self.guarded_order.borrow_mut();
            if order.as_ref().is_some_and(|hint| !hint.matches(&entries)) {
                *order = None;
            }
            entries.touch_data_and_fetch(data, fetch);
            if let Some(hint) = order.as_mut() {
                hint.touch(&entries, data, fetch);
            }
        }
        Some((value, translation))
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
    ) -> Option<(u32, Translation)> {
        let entries = self.entries.borrow();
        // This private caller runs only in the admitted read-only loop.
        // Its last touch is the retained fetch for the entire code page.
        let code = entries.slots[fetch].as_ref()?;
        if entries.capacity < 2
            || entries.exhausted
            || entries.next_id == u64::MAX
            || entries.recency().ages[fetch] != entries.recency().clock
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
            let value = image.physical_read_mapped_dword(translation.physical)?;
            Some((translation, value))
        })?;
        let only_4k_leaves = entries.lookup_index.only_4k_leaves();
        drop(entries);
        // A nonoverlapping fill with at least two slots cannot evict the
        // newest entry. Commit the same fill and next fetch as checked execution.
        if only_4k_leaves {
            self.install_known_local_miss(state, translation, fetch);
        } else {
            let installed = self.fill(state, request, translation);
            debug_assert!(installed.cache_id.is_some());
            self.entries.borrow_mut().touch(fetch);
        }
        Some((value, translation))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    fn install_known_local_miss(&self, state: &State, mut translation: Translation, fetch: usize) {
        // The indexed scan just proved the absence of any visible hit.
        // An aligned 4 KiB leaf in this cache can overlap only the same leaf.
        let mut entries = self.entries.borrow_mut();
        let vacant = entries.lookup_index.first_vacancy(entries.capacity);
        let mut order = self.guarded_order.borrow_mut();
        let index = if let Some(vacant) = vacant {
            *order = None;
            vacant
        } else {
            if order.as_ref().is_none_or(|hint| !hint.matches(&entries)) {
                *order = Some(recency::GuardedOrder::from_entries(&entries));
            }
            self.changed.set(true);
            order.as_ref().unwrap().order.oldest().unwrap()
        };
        let id = entries.next_id;
        translation.cache_id = Some(id);
        entries.next_id += 1;
        entries.insert(
            index,
            Entry {
                translation,
                pcid: state.pcid(),
                global: false,
                family: family(state),
            },
        );
        entries.touch_data_and_fetch(index, fetch);
        if let Some(hint) = order.as_mut() {
            hint.touch(&entries, index, fetch);
        }
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
            .hint()
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
            if entries.hint() != Some(index) {
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
            .hint()
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
            if entries.hint() != Some(index) {
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
            .hint()
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
            if entries.hint() != Some(index) {
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
        let fetch = entries.hint()?;
        let code = entries.slots[fetch].as_ref()?;
        if entries.recency().ages[fetch] != entries.recency().clock
            || code.translation.span != PAGE
            || entries.exhausted
        {
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
            entries.recency_mut().ages.fill(0);
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            {
                entries.recency_mut().live_ids.fill(0);
                entries
                    .recency_mut()
                    .entries
                    .fill(IndexedReadEntry::default());
                entries
                    .recency_mut()
                    .writes
                    .fill(IndexedReadEntry::default());
            }
            entries.lookup_index = lookup_index::LookupIndex::default();
            entries.set_hint(None);
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
        for index in 0..capacity {
            let Some(entry) = entries.slots[index].as_ref() else {
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
                    entries.remove(index);
                    first_free.get_or_insert(index);
                    continue;
                }
            }
            if entries.recency().ages[index] < oldest_age {
                oldest_index = index;
                oldest_age = entries.recency().ages[index];
            }
        }
        let index = first_free.unwrap_or(oldest_index);
        if overlaps_visible_entry || first_free.is_none() {
            self.changed.set(true);
        }
        translation.cache_id = Some(entries.next_id);
        entries.next_id += 1;
        entries.insert(
            index,
            Entry {
                translation,
                pcid,
                global,
                family,
            },
        );
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

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn changed_ptr(&self) -> u32 {
        self.changed.as_ptr() as u32
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn take_loop_read_invalidation(&self) -> bool {
        self.loop_reads_invalidated.replace(false)
    }

    fn remove(&self, predicate: impl Fn(&Entry) -> bool) {
        // The compiled read table can retain a page already evicted from the slots below.
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        self.loop_reads_invalidated.set(true);
        let mut entries = self.entries.borrow_mut();
        for index in 0..entries.capacity {
            if entries.slots[index].as_ref().is_some_and(&predicate) {
                entries.remove(index);
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
