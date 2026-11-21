use super::super::image::Backing;
#[cfg(target_arch = "wasm32")]
use super::super::{Access, Fault};
use super::super::{Image, MemoryError};
use super::{Lanes, each_lane, gather, lane, scatter, shift};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "ring64_ram")]
unsafe extern "C" {
    fn load_lane(offset: u32) -> u64;
    fn store_lane(offset: u32, value: u64);
    fn compare_exchange_lane(offset: u32, expected: u64, replacement: u64) -> u64;
    fn load_control(offset: u32) -> u32;
    fn store_control(offset: u32, value: u32);
    fn compare_exchange_control(offset: u32, expected: u32, replacement: u32) -> u32;
    fn enter_exclusive() -> i32;
    fn leave_exclusive(token: i32);
    fn note_write(offset: u32, length: u32);
}

#[cfg(not(target_arch = "wasm32"))]
mod unavailable {
    pub(super) unsafe fn load_lane(_: u32) -> u64 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn store_lane(_: u32, _: u64) {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn compare_exchange_lane(_: u32, _: u64, _: u64) -> u64 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn load_control(_: u32) -> u32 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn store_control(_: u32, _: u32) {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn compare_exchange_control(_: u32, _: u32, _: u32) -> u32 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn enter_exclusive() -> i32 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn leave_exclusive(_: i32) {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn note_write(_: u32, _: u32) {
        unreachable!("shared RAM requires Wasm")
    }
}

#[cfg(not(target_arch = "wasm32"))]
use unavailable::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SharedBacking {
    pub offset: u32,
    pub length: u32,
    pub incarnation: u64,
}

#[cfg(target_arch = "wasm32")]
pub(crate) enum SharedDword {
    Direct { address: u32, bytes: u32 },
    Loaded(u32),
}

impl SharedBacking {
    fn padded_end(self) -> u64 {
        u64::from(self.offset) + u64::from(self.length).next_multiple_of(4096)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SharedLayout {
    base: u32,
    capacity: u32,
    guards: u32,
    watches: u32,
}

impl Lanes for SharedLayout {
    fn load(&self, offset: u32) -> u64 {
        unsafe { load_lane(offset) }
    }

    fn store(&self, offset: u32, value: u64) {
        unsafe { store_lane(offset, value) }
    }

    fn compare_exchange(&self, offset: u32, expected: u64, replacement: u64) -> u64 {
        unsafe { compare_exchange_lane(offset, expected, replacement) }
    }
}

#[derive(Debug)]
pub(crate) struct SharedExclusive(i32);

impl SharedExclusive {
    fn acquire() -> Self {
        let token = unsafe { enter_exclusive() };
        assert!(token >= 0, "parallel RAM exclusive grant declined");
        Self(token)
    }
}

impl Drop for SharedExclusive {
    fn drop(&mut self) {
        unsafe { leave_exclusive(self.0) };
    }
}

#[derive(Debug)]
pub(crate) struct SharedHeld {
    offsets: [u32; 16],
    count: usize,
}

impl Drop for SharedHeld {
    fn drop(&mut self) {
        for &offset in self.offsets[..self.count].iter().rev() {
            unsafe { store_control(offset, 1) };
        }
    }
}

impl SharedLayout {
    fn new(base: u32, capacity: u32, guards: u32, watches: u32) -> Result<Self, MemoryError> {
        const END: u64 = 1 << 32;
        if capacity == 0
            || base == 0
            || base & 4095 != 0
            || capacity & 4095 != 0
            || guards & 3 != 0
            || watches & 3 != 0
        {
            return Err(MemoryError::Invalid("misaligned shared RAM layout"));
        }
        let ram = u64::from(base)..u64::from(base) + u64::from(capacity);
        let locks = u64::from(guards)..u64::from(guards) + u64::from(capacity / 64 * 4);
        let watch = u64::from(watches)..u64::from(watches) + u64::from(capacity / 8);
        let overlap =
            |a: &std::ops::Range<u64>, b: &std::ops::Range<u64>| a.start < b.end && b.start < a.end;
        if ram.end > END
            || locks.end > END
            || watch.end > END
            || overlap(&ram, &locks)
            || overlap(&ram, &watch)
            || overlap(&locks, &watch)
        {
            return Err(MemoryError::Invalid(
                "overlapping or out-of-range shared RAM layout",
            ));
        }
        Ok(Self {
            base,
            capacity,
            guards,
            watches,
        })
    }

    fn check_backing(
        self,
        present: impl IntoIterator<Item = SharedBacking>,
        next: SharedBacking,
    ) -> Result<(), MemoryError> {
        if next.incarnation == 0
            || next.length == 0
            || next.offset & 4095 != 0
            || next.offset < self.base
            || next.padded_end() > u64::from(self.base) + u64::from(self.capacity)
        {
            return Err(MemoryError::Invalid("shared backing outside its RAM arena"));
        }
        for other in present {
            if other.incarnation == next.incarnation
                || u64::from(next.offset) < other.padded_end()
                    && u64::from(other.offset) < next.padded_end()
            {
                return Err(MemoryError::Invalid(
                    "shared backing aliases a distinct incarnation",
                ));
            }
        }
        Ok(())
    }

    fn stripe(self, absolute: u32) -> u32 {
        self.guards + ((absolute - self.base) / 64) * 4
    }

    fn watched(self, start: u32, length: u32) -> bool {
        let mut cur = start - self.base;
        let end = cur + length;
        while cur < end {
            let bit = cur & 31;
            let count = (end - cur).min(32 - bit);
            let mask = (u32::MAX >> (32 - count)) << bit;
            if unsafe { load_control(self.watches + (cur / 32) * 4) } & mask != 0 {
                return true;
            }
            cur += count;
        }
        false
    }

    fn stripes(self, offsets: &[u32]) -> ([u32; 16], usize) {
        let mut stripes = [0; 16];
        for (stripe, &offset) in stripes.iter_mut().zip(offsets) {
            *stripe = self.stripe(offset);
        }
        let count = offsets.len();
        stripes[..count].sort_unstable();
        let mut unique = 0;
        for i in 0..count {
            if unique == 0 || stripes[i] != stripes[unique - 1] {
                stripes[unique] = stripes[i];
                unique += 1;
            }
        }
        (stripes, unique)
    }

    fn try_hold(self, stripes: &[u32], require_promoted: bool) -> Option<SharedHeld> {
        let mut held = SharedHeld {
            offsets: [0; 16],
            count: 0,
        };
        for &stripe in stripes {
            match unsafe { load_control(stripe) } {
                0 if !require_promoted => continue,
                1 if unsafe { compare_exchange_control(stripe, 1, 2) } == 1 => {
                    held.offsets[held.count] = stripe;
                    held.count += 1;
                }
                _ => return None,
            }
        }
        Some(held)
    }

    fn promote(self, stripes: &[u32]) {
        for &stripe in stripes {
            if unsafe { load_control(stripe) } == 0 {
                unsafe { store_control(stripe, 1) };
            }
        }
    }

    fn has_watched_byte(self, offsets: &[u32]) -> bool {
        let Some(&first) = offsets.first() else {
            return false;
        };
        let relative = first - self.base;
        let word = relative / 32;
        let mut mask = 0u32;
        for &offset in offsets {
            let bit = offset - self.base;
            if bit / 32 != word {
                return offsets.iter().any(|&byte| self.watched(byte, 1));
            }
            mask |= 1 << (bit & 31);
        }
        (unsafe { load_control(self.watches + word * 4) } & mask) != 0
    }

    fn access<T>(
        self,
        offsets: &[u32],
        write: bool,
        held_rmw: Option<&SharedHeld>,
        mut operation: impl FnMut(bool) -> Option<T>,
    ) -> T {
        if let Some(held) = held_rmw {
            let occupied = &held.offsets[..held.count];
            assert!(
                offsets
                    .iter()
                    .all(|byte| occupied.binary_search(&self.stripe(*byte)).is_ok()),
                "parallel RMW access escaped its admitted stripes"
            );
            return operation(true).expect("guarded parallel RMW did not complete");
        }
        let multiple = each_lane(offsets).take(2).count() > 1;
        let (stripes, count) = self.stripes(offsets);
        let stripes = &stripes[..count];
        let watch = write && self.has_watched_byte(offsets);
        if !watch
            && let Some(held) = self.try_hold(stripes, multiple)
            && let Some(result) = operation(held.count != 0)
        {
            return result;
        }

        let _grant = SharedExclusive::acquire();
        if multiple {
            self.promote(stripes);
        }
        let result = operation(true).expect("exclusive parallel RAM operation was incomplete");
        if write {
            let mut pending: Option<(u32, u32)> = None;
            for &offset in offsets {
                if let Some((start, length)) = pending.take() {
                    if start.checked_add(length) == Some(offset) {
                        pending = Some((start, length + 1));
                        continue;
                    }
                    self.note_if_watched(start, length);
                }
                pending = Some((offset, 1));
            }
            if let Some((start, length)) = pending {
                self.note_if_watched(start, length);
            }
        }
        result
    }

    fn note_if_watched(self, start: u32, length: u32) {
        if self.watched(start, length) {
            unsafe { note_write(start, length) };
        }
    }
}

impl Image {
    pub(crate) fn parallel_attached(&self) -> bool {
        self.shared.is_some()
    }

    pub(crate) fn shared_exclusive(&self) -> Option<SharedExclusive> {
        self.shared.map(|_| SharedExclusive::acquire())
    }

    pub(crate) fn bind_shared_layout(
        &mut self,
        ram_base: u32,
        ram_capacity: u32,
        guard_base: u32,
        watch_base: u32,
        extents: &[(u32, u64)],
    ) -> Result<(), MemoryError> {
        if self.shared.is_some() || extents.len() != self.backings.len() {
            return Err(MemoryError::Invalid(
                "shared RAM attachment does not match backings",
            ));
        }
        let layout = SharedLayout::new(ram_base, ram_capacity, guard_base, watch_base)?;
        let mut prepared = Vec::with_capacity(extents.len());
        for (backing, &(offset, incarnation)) in self.backings.iter().zip(extents) {
            let record = SharedBacking {
                offset,
                incarnation,
                length: backing.bytes.len() as u32,
            };
            layout.check_backing(prepared.iter().copied(), record)?;
            prepared.push(record);
        }
        for (backing, record) in self.backings.iter_mut().zip(prepared) {
            backing.shared = Some(record);
            backing.bytes = Vec::new();
        }
        self.shared = Some(layout);
        Ok(())
    }

    pub(crate) fn add_shared_backing(
        &mut self,
        id: String,
        length: u32,
        offset: u32,
        incarnation: u64,
    ) -> Result<(), MemoryError> {
        let layout = self
            .shared
            .ok_or(MemoryError::Invalid("no shared RAM attachment"))?;
        self.check_backing(&id, length as usize)?;
        let record = SharedBacking {
            offset,
            incarnation,
            length,
        };
        layout.check_backing(
            self.backings.iter().filter_map(|backing| backing.shared),
            record,
        )?;
        self.backings
            .try_reserve(1)
            .map_err(|_| MemoryError::Capacity("backing allocation refused"))?;
        self.backings.push(Backing {
            id,
            bytes: Vec::new(),
            incarnation: std::rc::Rc::new(()),
            has_sources: false,
            shared: Some(record),
        });
        Ok(())
    }

    pub(crate) fn parallel_invalidate_range(&mut self, absolute: u32, length: u32) {
        let end = u64::from(absolute) + u64::from(length);
        for backing in &self.backings {
            let Some(shared) = backing.shared else {
                continue;
            };
            let first = absolute.max(shared.offset);
            let last = end.min(u64::from(shared.offset) + u64::from(shared.length));
            if u64::from(first) < last {
                self.validity.write(
                    &backing.incarnation,
                    first - shared.offset,
                    (last - u64::from(first)) as u32,
                );
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn shared_direct_lane(
        &self,
        address: u64,
        width: u32,
        access: Access,
        physical: bool,
        rmw: bool,
    ) -> Result<Option<u32>, Fault> {
        let Some(layout) = self.shared else {
            return Ok(None);
        };
        assert!(matches!(width, 1 | 2 | 4 | 8));
        let index = if physical {
            self.preflight::<true>(address, width, access, rmw, true)?
        } else {
            self.preflight::<false>(address, width, access, rmw, true)?
        };
        let mapping = &self.mappings[index];
        let relative = (address - mapping.base) as u32;
        if width > mapping.length - relative {
            return Ok(None);
        }
        let backing = self.backings[mapping.backing]
            .shared
            .expect("shared backing binding");
        let absolute = backing.offset + mapping.offset + relative;
        if (absolute & 7) + width > 8
            || unsafe { load_control(layout.stripe(absolute)) } != 0
            || access == Access::Write && layout.watched(absolute, width)
        {
            return Ok(None);
        }
        Ok(Some(absolute))
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn shared_vector_dword<const SPAN: bool>(
        &self,
        address: u64,
    ) -> Result<SharedDword, Fault> {
        let index = self.preflight::<false>(address, 4, Access::Read, false, true)?;
        let mut bytes = [0; 4];
        Ok(
            match self.shared_read_after_preflight::<true, SPAN>(address, &mut bytes, index) {
                Some((address, bytes)) => SharedDword::Direct { address, bytes },
                None => SharedDword::Loaded(u32::from_le_bytes(bytes)),
            },
        )
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn begin_shared_guarded_rmw(&mut self, address: u64, width: u32) -> bool {
        let Some(layout) = self.shared else {
            return false;
        };
        assert!(self.shared_rmw.is_none(), "nested resolved parallel RMW");
        if !matches!(width, 1 | 2 | 4 | 8 | 16) || width == 16 && address & 15 != 0 {
            return false;
        }
        let Ok(index) = self.preflight::<false>(address, width, Access::Write, true, true) else {
            return false;
        };
        let mut offsets = [0; 16];
        let offsets = &mut offsets[..width as usize];
        self.offsets(address, offsets, index);
        if layout.has_watched_byte(offsets) {
            return false;
        }
        let (stripes, count) = layout.stripes(offsets);
        let stripes = &stripes[..count];
        if stripes
            .iter()
            .any(|&stripe| unsafe { load_control(stripe) } == 0)
        {
            if each_lane(offsets).take(2).count() > 1 {
                let _grant = SharedExclusive::acquire();
                layout.promote(stripes);
            }
            return false;
        }
        self.shared_rmw = layout.try_hold(stripes, true);
        self.shared_rmw.is_some()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn end_shared_guarded_rmw(&mut self) {
        self.shared_rmw.take().expect("no resolved parallel RMW");
    }

    // Form exact canonical coordinates after architectural access admission.
    fn offsets(&self, address: u64, offsets: &mut [u32], mut index: usize) {
        for (current, offset) in (u128::from(address)..).zip(offsets) {
            if self.mappings[index].end() <= current {
                index += 1;
            }
            let mapping = &self.mappings[index];
            let backing = self.backings[mapping.backing]
                .shared
                .expect("shared backing binding");
            *offset = backing.offset + mapping.offset + (current as u64 - mapping.base) as u32;
        }
    }

    fn pieces(
        &self,
        address: u64,
        length: usize,
        mut index: usize,
        mut visit: impl FnMut(u32, usize, usize),
    ) {
        let mut current = u128::from(address);
        let mut done = 0;
        while done < length {
            let mapping = &self.mappings[index];
            let backing = self.backings[mapping.backing]
                .shared
                .expect("shared backing binding");
            let absolute = backing.offset + mapping.offset + (current as u64 - mapping.base) as u32;
            let remainder = (mapping.end() - current) as usize;
            let count = (length - done)
                .min(remainder)
                .min((8 - (absolute & 7)) as usize);
            visit(absolute, done, count);
            done += count;
            current += count as u128;
            if current == mapping.end() {
                index += 1;
            }
        }
    }

    pub(in crate::memory) fn shared_read(&self, address: u64, bytes: &mut [u8], index: usize) {
        let direct = self.shared_read_after_preflight::<false, false>(address, bytes, index);
        debug_assert!(direct.is_none());
    }

    // A returned canonical byte address leaves the dword load to generated Wasm.
    fn shared_read_after_preflight<const NATIVE_DWORD: bool, const SPAN: bool>(
        &self,
        address: u64,
        bytes: &mut [u8],
        index: usize,
    ) -> Option<(u32, u32)> {
        let Some(layout) = self.shared else {
            unreachable!()
        };
        if bytes.len() <= 8 && (address & 63) + bytes.len() as u64 <= 64 {
            if bytes.len() == 4 && self.shared_rmw.is_none() {
                let mapping = &self.mappings[index];
                let relative = (address - mapping.base) as u32;
                if 4 <= mapping.length - relative {
                    let backing = self.backings[mapping.backing]
                        .shared
                        .expect("shared backing binding");
                    let absolute = backing.offset + mapping.offset + relative;
                    if (absolute & 63) <= 60 {
                        let crosses_lane = (absolute & 7) > 4;
                        let stripes = [layout.stripe(absolute)];
                        // Same one-attempt hold and exclusive promotion order as access().
                        let held = layout.try_hold(&stripes, crosses_lane);
                        if NATIVE_DWORD && held.as_ref().is_some_and(|held| held.count == 0) {
                            debug_assert!(!crosses_lane);
                            let bytes = if SPAN && absolute & 3 == 0 {
                                (mapping.length - relative)
                                    .min(64 - (absolute & 63))
                                    .min(64 - (address as u32 & 63))
                            } else {
                                4
                            };
                            return Some((absolute, bytes));
                        }
                        let _grant = held.is_none().then(|| {
                            let grant = SharedExclusive::acquire();
                            if crosses_lane {
                                layout.promote(&stripes);
                            }
                            grant
                        });
                        let base = lane(absolute);
                        let bits = shift(absolute);
                        let low = layout.load(base) >> bits;
                        let value = if crosses_lane {
                            low | (layout.load(base + 8) << (64 - bits))
                        } else {
                            low
                        };
                        bytes.copy_from_slice(&(value as u32).to_le_bytes());
                        return None;
                    }
                }
            }
            let mut offsets = [0; 8];
            let offsets = &mut offsets[..bytes.len()];
            self.offsets(address, offsets, index);
            layout.access(offsets, false, self.shared_rmw.as_ref(), |_| {
                gather(&layout, offsets, bytes);
                Some(())
            });
            return None;
        }
        self.pieces(address, bytes.len(), index, |absolute, done, count| {
            let offsets = std::array::from_fn::<_, 8, _>(|i| lane(absolute) + i as u32);
            let start = (absolute & 7) as usize;
            let offsets = &offsets[start..start + count];
            layout.access(offsets, false, self.shared_rmw.as_ref(), |_| {
                gather(&layout, offsets, &mut bytes[done..done + count]);
                Some(())
            });
        });
        None
    }

    pub(in crate::memory) fn shared_write(&mut self, address: u64, bytes: &[u8], index: usize) {
        let Some(layout) = self.shared else {
            unreachable!()
        };
        if bytes.len() <= 8 && (address & 63) + bytes.len() as u64 <= 64 {
            let mut offsets = [0; 8];
            let offsets = &mut offsets[..bytes.len()];
            self.offsets(address, offsets, index);
            layout.access(offsets, true, self.shared_rmw.as_ref(), |held| {
                scatter(&layout, offsets, bytes, held).then_some(())
            });
        } else {
            self.pieces(address, bytes.len(), index, |absolute, done, count| {
                let offsets = std::array::from_fn::<_, 8, _>(|i| lane(absolute) + i as u32);
                let start = (absolute & 7) as usize;
                let offsets = &offsets[start..start + count];
                layout.access(offsets, true, self.shared_rmw.as_ref(), |held| {
                    scatter(&layout, offsets, &bytes[done..done + count], held).then_some(())
                });
            });
        }
        // Local translated owners need the same invalidation as before, even for identity writes.
        let mut current = u128::from(address);
        let last = current + bytes.len() as u128;
        let mut position = index;
        while current < last {
            let mapping = &self.mappings[position];
            let backing = &self.backings[mapping.backing];
            let offset = mapping.offset + (current as u64 - mapping.base) as u32;
            let count = (last.min(mapping.end()) - current) as u32;
            if backing.has_sources {
                self.validity.write(&backing.incarnation, offset, count);
            }
            current += u128::from(count);
            position += 1;
        }
    }
}

impl Image {
    pub(crate) fn mapped_shared(
        backings: Vec<(String, u32, u32, u64)>,
        mappings: Vec<super::super::MappingInput>,
        limits: super::super::MemoryLimits,
        physical: bool,
        [ram_base, ram_capacity, guard_base, watch_base]: [u32; 4],
    ) -> Result<Self, MemoryError> {
        let mut image = if physical {
            Self::physical_mapped(Vec::new(), Vec::new(), limits)?
        } else {
            Self::mapped(Vec::new(), Vec::new(), limits)?
        };
        image.bind_shared_layout(ram_base, ram_capacity, guard_base, watch_base, &[])?;
        for (id, length, offset, incarnation) in backings {
            image.add_shared_backing(id, length, offset, incarnation)?;
        }
        image.set_mappings(mappings)?;
        Ok(image)
    }
}
