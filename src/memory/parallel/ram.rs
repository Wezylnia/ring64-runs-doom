#![allow(
    clippy::useless_conversion,
    reason = "shared offsets follow the RAM profile"
)]

use super::super::MemoryError;
use super::{Lanes, SHARED_RAM_MAX_PAGES, SharedOffset, each_lane, lane, shift};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "ring64_ram")]
unsafe extern "C" {
    fn load_lane(offset: SharedOffset) -> u64;
    fn store_lane(offset: SharedOffset, value: u64);
    fn compare_exchange_lane(offset: SharedOffset, expected: u64, replacement: u64) -> u64;
    fn load_control(offset: SharedOffset) -> u32;
    fn store_control(offset: SharedOffset, value: u32);
    fn compare_exchange_control(offset: SharedOffset, expected: u32, replacement: u32) -> u32;
    fn enter_exclusive() -> i32;
    fn leave_exclusive(token: i32);
    fn note_write(offset: SharedOffset, length: u32);
}

#[cfg(not(target_arch = "wasm32"))]
mod unavailable {
    use super::SharedOffset;

    pub(super) unsafe fn load_lane(_: SharedOffset) -> u64 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn store_lane(_: SharedOffset, _: u64) {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn compare_exchange_lane(_: SharedOffset, _: u64, _: u64) -> u64 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn load_control(_: SharedOffset) -> u32 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn store_control(_: SharedOffset, _: u32) {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn compare_exchange_control(_: SharedOffset, _: u32, _: u32) -> u32 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn enter_exclusive() -> i32 {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn leave_exclusive(_: i32) {
        unreachable!("shared RAM requires Wasm")
    }
    pub(super) unsafe fn note_write(_: SharedOffset, _: u32) {
        unreachable!("shared RAM requires Wasm")
    }
}

#[cfg(not(target_arch = "wasm32"))]
use unavailable::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SharedBacking {
    pub offset: SharedOffset,
    pub length: SharedOffset,
    pub incarnation: u64,
}

impl SharedBacking {
    fn padded_end(self) -> u64 {
        u64::from(self.offset) + u64::from(self.length).next_multiple_of(4096)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SharedLayout {
    base: SharedOffset,
    capacity: SharedOffset,
    guards: SharedOffset,
    watches: SharedOffset,
}

impl Lanes for SharedLayout {
    fn load(&self, offset: SharedOffset) -> u64 {
        unsafe { load_lane(offset) }
    }

    fn store(&self, offset: SharedOffset, value: u64) {
        unsafe { store_lane(offset, value) }
    }

    fn compare_exchange(&self, offset: SharedOffset, expected: u64, replacement: u64) -> u64 {
        unsafe { compare_exchange_lane(offset, expected, replacement) }
    }
}

#[derive(Debug)]
pub(crate) struct SharedExclusive(i32);

impl SharedExclusive {
    pub(super) fn acquire() -> Self {
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
    offsets: [SharedOffset; 16],
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
    pub(super) fn new(
        base: SharedOffset,
        capacity: SharedOffset,
        guards: SharedOffset,
        watches: SharedOffset,
    ) -> Result<Self, MemoryError> {
        const END: u64 = SHARED_RAM_MAX_PAGES * 65_536;
        if capacity == 0
            || base == 0
            || base & 4095 != 0
            || capacity & 4095 != 0
            || guards & 3 != 0
            || watches & 3 != 0
        {
            return Err(MemoryError::Invalid("misaligned shared RAM layout"));
        }
        if [base, capacity, guards, watches]
            .into_iter()
            .any(|value| u64::from(value) >= END)
        {
            return Err(MemoryError::Invalid("out-of-range shared RAM layout"));
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

    #[cfg(parallel_memory64)]
    pub(super) fn requires_memory64(self) -> bool {
        (self.base + self.capacity)
            .max(self.guards + self.capacity / 16)
            .max(self.watches + self.capacity / 8)
            > 1 << 32
    }

    pub(super) fn check_backing(
        self,
        present: impl IntoIterator<Item = SharedBacking>,
        next: SharedBacking,
    ) -> Result<(), MemoryError> {
        if next.incarnation == 0
            || next.length == 0
            || next.offset & 4095 != 0
            || next.offset < self.base
            || next.length > self.capacity
            || u64::from(next.offset) >= u64::from(self.base) + u64::from(self.capacity)
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

    fn stripe(self, absolute: SharedOffset) -> SharedOffset {
        self.guards + ((absolute - self.base) / 64) * 4
    }

    pub(super) fn watched(self, start: SharedOffset, length: u32) -> bool {
        let mut cur = start - self.base;
        let end = cur + SharedOffset::from(length);
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

    fn stripes(self, offsets: &[SharedOffset]) -> ([SharedOffset; 16], usize) {
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

    fn try_hold(self, stripes: &[SharedOffset], require_promoted: bool) -> Option<SharedHeld> {
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

    fn promote(self, stripes: &[SharedOffset]) {
        for &stripe in stripes {
            if unsafe { load_control(stripe) } == 0 {
                unsafe { store_control(stripe, 1) };
            }
        }
    }

    fn has_watched_byte(self, offsets: &[SharedOffset]) -> bool {
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

    pub(super) fn access<T>(
        self,
        offsets: &[SharedOffset],
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
            let mut pending: Option<(SharedOffset, u32)> = None;
            for &offset in offsets {
                if let Some((start, length)) = pending.take() {
                    if start.checked_add(SharedOffset::from(length)) == Some(offset) {
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

    fn note_if_watched(self, start: SharedOffset, length: u32) {
        if self.watched(start, length) {
            unsafe { note_write(start, length) };
        }
    }

    #[cfg(target_arch = "wasm32")]
    #[inline]
    pub(super) fn unpromoted(self, absolute: SharedOffset) -> bool {
        unsafe { load_control(self.stripe(absolute)) == 0 }
    }

    #[cfg(target_arch = "wasm32")]
    pub(super) fn guard_bias(self, last: SharedOffset) -> Option<u32> {
        super::direct_offset(self.stripe(last))?;
        let bias = self.guards.wrapping_sub((self.base / 64) * 4);
        #[cfg(parallel_memory64)]
        let bias = bias as u32;
        Some(bias)
    }

    #[cfg(target_arch = "wasm32")]
    #[inline]
    pub(super) fn byte_store_room(self, absolute: SharedOffset) -> Option<u32> {
        let in_word = (absolute - self.base) & 31;
        // Both the source-watch word and its containing stripe must stay clear.
        if !self.unpromoted(absolute) || self.watched(absolute - in_word, 32) {
            return None;
        }
        Some(super::direct_offset(32 - in_word).expect("in-word byte count"))
    }

    #[cfg(target_arch = "wasm32")]
    #[inline]
    pub(super) fn begin_rmw(self, offsets: &[SharedOffset], hold: &mut Option<SharedHeld>) -> bool {
        if self.has_watched_byte(offsets) {
            return false;
        }
        let (stripes, count) = self.stripes(offsets);
        let stripes = &stripes[..count];
        if stripes
            .iter()
            .any(|&stripe| unsafe { load_control(stripe) } == 0)
        {
            if each_lane(offsets).take(2).count() > 1 {
                let _grant = SharedExclusive::acquire();
                self.promote(stripes);
            }
            return false;
        }
        *hold = self.try_hold(stripes, true);
        hold.is_some()
    }

    // True leaves the read to generated Wasm. Otherwise publish the loaded dword
    // while the stripe hold or exclusive grant is still live.
    #[inline(always)]
    pub(super) fn read_contiguous_dword<const NATIVE_DWORD: bool>(
        self,
        absolute: SharedOffset,
        bytes: &mut [u8],
    ) -> bool {
        debug_assert!(bytes.len() == 4 && (absolute & 63) <= 60);
        let crosses_lane = (absolute & 7) > 4;
        let stripes = [self.stripe(absolute)];
        // Same one-attempt hold and exclusive promotion order as access().
        let held = self.try_hold(&stripes, crosses_lane);
        if NATIVE_DWORD
            && super::direct_offset(absolute).is_some()
            && held.as_ref().is_some_and(|held| held.count == 0)
        {
            debug_assert!(!crosses_lane);
            return true;
        }
        let _grant = held.is_none().then(|| {
            let grant = SharedExclusive::acquire();
            if crosses_lane {
                self.promote(&stripes);
            }
            grant
        });
        let base = lane(absolute);
        let bits = shift(absolute);
        let low = self.load(base) >> bits;
        let value = if crosses_lane {
            low | (self.load(base + 8) << (64 - bits))
        } else {
            low
        };
        bytes.copy_from_slice(&(value as u32).to_le_bytes());
        false
    }
}
