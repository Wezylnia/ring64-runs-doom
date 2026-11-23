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

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
mod copy;
#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
mod guarded;
mod lookup_index;

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
mod recency;

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
    pub(crate) age_increments: [u8; 4],
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
// Bit 1 in the separate read table requires the clipped dword bounds.
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const INDEXED_CLIPPED_READ: u64 = 2;

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
    fn new(linear: u64, epoch: u64, slot: usize, host: u32, readable_write: bool) -> Self {
        debug_assert!(epoch & !0xffc == 0 && slot < CAPACITY && host != 0);
        let readable = if readable_write {
            INDEXED_WRITE_READABLE
        } else {
            0
        };
        Self {
            tag: (linear & !4095) | epoch | INDEXED_VALID | readable,
            slot_offset: slot as u32 * 8,
            host_bias: host.wrapping_sub(linear as u32),
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
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) clipped_base: u64,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) clipped_last_dword: u32,
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
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            clipped_base: 0,
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            clipped_last_dword: 0,
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
        if t.global || t.span < 4096 || (t.linear_base | t.physical_base) & 4095 != 0 {
            *unsupported_leaf = true;
            return None;
        }
        // Cache only this 4 KiB subpage, under the identity of its owning
        // translation. Replacing a large leaf invalidates every memo using it.
        *slot = t.cache_id.map(|id| IndexedWriteSlot {
            index,
            id,
            linear_base: request.linear & !4095,
            physical_base: t.physical & !4095,
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

    #[cfg(guest_memory64)]
    pub(crate) fn retire_backing_spans(&self) {
        self.entries.borrow_mut().recency_mut().retire_pages();
        self.last_dword_read.set(None);
        self.loop_reads_invalidated.set(true);
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
        let delta = address.wrapping_sub(memo.span.base);
        if address - page > 4092
            || delta > u64::from(memo.span.length - 4)
            || u64::from(memo.span.host) + delta != u64::from(host)
        {
            return;
        }
        let start = page.max(memo.span.base);
        let length =
            (memo.span.length - (start - memo.span.base) as u32).min(4096 - (start - page) as u32);
        let mut entries = self.entries.borrow_mut();
        let table = entries.recency_mut();
        if table.live_ids[memo.slot] != memo.id {
            return;
        }
        // The memo is the intersection of the local translation and a direct
        // readable mapping. Bounds only qualify the tagged page in this epoch.
        let mut record = IndexedReadEntry::new(address, table.epoch, memo.slot, host, false);
        if length < 4096 {
            // Absolute bounds cover at most one page. Older clipped tags on
            // other pages therefore miss when this range is replaced.
            table.clipped_base = start;
            table.clipped_last_dword = length - 4;
            record.tag |= INDEXED_CLIPPED_READ;
        }
        let index = (page >> 12) as usize & (INDEXED_READ_ENTRIES - 1);
        table.entries[index] = record;
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
