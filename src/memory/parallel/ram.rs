use super::super::MemoryError;
use super::{Lanes, each_lane, lane, shift};

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
    pub(super) fn new(
        base: u32,
        capacity: u32,
        guards: u32,
        watches: u32,
    ) -> Result<Self, MemoryError> {
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

    pub(super) fn check_backing(
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

    pub(super) fn access<T>(
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

    #[cfg(target_arch = "wasm32")]
    #[inline]
    pub(super) fn direct_lane_allowed(self, absolute: u32, width: u32, write: bool) -> bool {
        (absolute & 7) + width <= 8
            && unsafe { load_control(self.stripe(absolute)) } == 0
            && (!write || !self.watched(absolute, width))
    }

    #[cfg(target_arch = "wasm32")]
    pub(super) fn guard_bias(self) -> u32 {
        self.guards.wrapping_sub((self.base / 64) * 4)
    }

    #[cfg(target_arch = "wasm32")]
    #[inline]
    pub(super) fn byte_store_room(self, absolute: u32) -> Option<u32> {
        let in_word = (absolute - self.base) & 31;
        // Both the source-watch word and its containing stripe must stay clear.
        if unsafe { load_control(self.stripe(absolute)) } != 0
            || self.watched(absolute - in_word, 32)
        {
            return None;
        }
        Some(32 - in_word)
    }

    #[cfg(target_arch = "wasm32")]
    #[inline]
    pub(super) fn begin_rmw(self, offsets: &[u32]) -> Option<SharedHeld> {
        if self.has_watched_byte(offsets) {
            return None;
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
            return None;
        }
        self.try_hold(stripes, true)
    }

    // True leaves the read to generated Wasm. Otherwise publish the loaded dword
    // while the stripe hold or exclusive grant is still live.
    #[inline]
    pub(super) fn read_contiguous_dword<const NATIVE_DWORD: bool>(
        self,
        absolute: u32,
        bytes: &mut [u8],
    ) -> bool {
        debug_assert!(bytes.len() == 4 && (absolute & 63) <= 60);
        let crosses_lane = (absolute & 7) > 4;
        let stripes = [self.stripe(absolute)];
        // Same one-attempt hold and exclusive promotion order as access().
        let held = self.try_hold(&stripes, crosses_lane);
        if NATIVE_DWORD && held.as_ref().is_some_and(|held| held.count == 0) {
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
