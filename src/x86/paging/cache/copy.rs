use super::{COPY_SOURCE_SLOTS, Cache, Entries, family};
use crate::memory::Access;
use crate::x86::paging::{
    State,
    walk::{self, Request, Translation},
};
use crate::x86::protection_keys::State as ProtectionKeys;

impl Cache {
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

    #[cold]
    pub(crate) fn ready_copy_operand(
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
        let (held, retained, missing) = if let Some((slot, translation)) =
            entries.retained_data_span::<false>(state, read, keys, width)
        {
            (slot, translation, write)
        } else {
            let (slot, translation) =
                entries.retained_data_span::<true>(state, write, keys, width)?;
            (slot, translation, read)
        };
        if entries.slots[held].as_ref()?.global {
            return None;
        }
        let filling_write = missing.access == Access::Write;
        let first = entries.find(state, missing);
        let (prefix, physical) = if let Some(slot) = first {
            let entry = entries.slots[slot].as_ref()?;
            let (physical, remaining) = if filling_write {
                Entries::checked_data_extent::<true>(&entry.translation, state, missing, keys, 1)?
            } else {
                Entries::checked_data_extent::<false>(&entry.translation, state, missing, keys, 1)?
            };
            if entry.global || remaining >= u64::from(width) || entries.capacity < 4 {
                return None;
            }
            (remaining, Some(physical))
        } else {
            (0, None)
        };
        let next = Request {
            linear: missing.linear.checked_add(prefix)?,
            ..missing
        };
        if prefix != 0 && entries.find(state, next).is_some() {
            return None;
        }
        // A cold read can precede its first store. If it would evict the
        // retained destination, replay so that store observes its own walk.
        if !filling_write
            && held != fetch
            && entries
                .lookup_index
                .first_vacancy(entries.capacity)
                .is_none()
            && !(0..entries.capacity).any(|slot| {
                slot != fetch
                    && Some(slot) != first
                    && entries.recency().ages[slot] < entries.recency().ages[held]
            })
        {
            return None;
        }
        let translation = image.probe_physical_read(|image| {
            let translation = walk::peek(image, state, next, keys).ok()?;
            (translation.span == 4096
                && translation.accessed
                && (!filling_write || !translation.dirty)
                && translation.remaining >= u64::from(width) - prefix
                && !(translation.global && state.cr4 & (1 << 7) != 0)
                && physical.is_none_or(|first| first + prefix == translation.physical))
            .then_some(translation)
        })?;
        let start = physical.unwrap_or(translation.physical);
        let (source, destination, earlier_stores) = if filling_write {
            (retained.physical, start, prefix)
        } else {
            (
                start,
                retained.physical,
                if prefix == 0 { 0 } else { u64::from(width) },
            )
        };
        admit(image, source, destination, earlier_stores, &translation)?;
        drop(entries);
        // The checked reads, writes and intervening fetches precede this fill.
        // Keep all operands of the group resident across its one allocation.
        for slot in [Some(held), first].into_iter().flatten() {
            if slot != fetch {
                self.entries.borrow_mut().touch_data_and_fetch(slot, fetch);
            }
        }
        let installed = self.fill(state, next, translation);
        debug_assert!(installed.cache_id.is_some());
        self.entries.borrow_mut().touch(fetch);
        Some(())
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn copy_split_pair_then_fetch<T>(
        &self,
        state: &State,
        [read, write]: [Request; 2],
        keys: &ProtectionKeys,
        width: u32,
        widths: [u32; 4],
        admit: impl FnOnce(u64, u64, [usize; 2]) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.borrow_mut();
        let fetch = entries
            .hint()
            .filter(|&index| entries.slots[index].is_some())?;
        let spans = [
            entries.retained_copy_span::<false>(state, read, keys, width)?,
            entries.retained_copy_span::<true>(state, write, keys, width)?,
        ];
        let last = spans
            .each_ref()
            .map(|span| span.split.map_or(span.first, |(_, last)| last));
        let value = admit(spans[0].physical, spans[1].physical, last)?;
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
}
