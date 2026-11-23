use super::{CAPACITY, Cache, DwordReadMemo, Entry, family, overlaps, recency};
#[cfg(target_arch = "wasm32")]
use super::{IndexedDwordWrite, IndexedWriteMemo};
use crate::memory::{Access, DirectSpan};
use crate::x86::paging::{
    State,
    walk::{self, Request, Translation},
};
use crate::x86::protection_keys::State as ProtectionKeys;
use std::cell::Cell;
use std::num::NonZeroU8;

impl Cache {
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

    pub(crate) fn remember_guarded_read_window(&self, slot: usize, id: u64) {
        self.guarded_read_window.set(Some((slot, id)));
    }

    pub(crate) fn retouch_guarded_read_window(&self, linear: u64) -> Option<()> {
        self.retouch_guarded_read_binding(self.guarded_read_window.get()?, linear)
    }

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

    #[cfg(target_arch = "wasm32")]
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

    #[cfg(test)]
    pub(super) fn read_dword_then_fetch(
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
                    let current = walk::peek(image, state, operand, keys).ok()?;
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
}
