// Direct spans carry nonzero 32-bit tokens. Small arenas use offset + 1.
// Larger arenas use windows with a stable bias for the lifetime of a proof.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use super::{DirectSpan, Mapping};

pub(crate) const WINDOW_SHIFT: u32 = 20;
const SLOTS: usize = 1 << (32 - WINDOW_SHIFT);
const GUARD: u64 = 4096;
const PAYLOAD: u64 = (1 << WINDOW_SHIFT) - GUARD;

thread_local! {
    static DIRECT_OFFSETS: Cell<bool> = const { Cell::new(false) };
    static BIASES: [Cell<u64>; SLOTS] = const { [const { Cell::new(0) }; SLOTS] };
    static SLOTS_BY_BASE: RefCell<BTreeMap<u64, u32>> = const { RefCell::new(BTreeMap::new()) };
    static LAST: Cell<Option<(u64, u32)>> = const { Cell::new(None) };
    static EXHAUSTED: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn use_direct_offsets() -> bool {
    DIRECT_OFFSETS.get()
}

impl super::Image {
    pub(crate) fn configure_guest_windows(&self) {
        // First-fit skips at most one gap per live backing, each smaller than
        // the requested extent. This bounds the arena end even after churn.
        let bound = self
            .limits
            .max_backing_bytes
            .saturating_mul(u64::from(self.limits.max_backings));
        DIRECT_OFFSETS.set(bound <= u64::from(u32::MAX));
    }
}

pub(crate) fn biases_pointer() -> u32 {
    BIASES.with(|table| table.as_ptr() as u32)
}

// Called between generated entries. The caller retires cached spans before
// any slot can be assigned a different bias. Guest TLB entries stay live.
pub(crate) fn recycle_if_exhausted() -> bool {
    if !EXHAUSTED.replace(false) {
        return false;
    }
    SLOTS_BY_BASE.with_borrow_mut(BTreeMap::clear);
    LAST.set(None);
    true
}

fn window_token(base: u64) -> Option<u32> {
    if let Some((key, token)) = LAST.get()
        && key == base
    {
        return Some(token);
    }
    let token = SLOTS_BY_BASE.with_borrow_mut(|windows| {
        if let Some(&token) = windows.get(&base) {
            return Some(token);
        }
        let slot = windows.len();
        if slot == SLOTS {
            EXHAUSTED.set(true);
            return None;
        }
        let token = ((slot as u32) << WINDOW_SHIFT) + GUARD as u32;
        BIASES.with(|table| table[slot].set(base.wrapping_sub(u64::from(token))));
        windows.insert(base, token);
        Some(token)
    })?;
    LAST.set(Some((base, token)));
    Some(token)
}

pub(super) fn byte_token(ram: u64) -> Option<u32> {
    if use_direct_offsets() {
        return u32::try_from(ram.checked_add(1)?).ok();
    }
    let inside = ram % PAYLOAD;
    Some(window_token(ram - inside)? + inside as u32)
}

pub(super) fn span(
    mapping: &Mapping,
    ram_base: u64,
    address: u64,
    width: u32,
) -> Option<DirectSpan> {
    if use_direct_offsets() {
        let host = byte_token(ram_base)?;
        host.checked_add(mapping.length - 1)?;
        return Some(DirectSpan {
            base: mapping.base,
            length: mapping.length,
            host,
        });
    }
    let relative = address - mapping.base;
    let ram = ram_base + relative;
    let inside = ram % PAYLOAD;
    let before = relative.min(inside);
    let after = (u64::from(mapping.length) - relative).min(PAYLOAD - inside);
    if u64::from(width) > after {
        return None;
    }
    // A guard page between tokens prevents unrelated windows from looking
    // contiguous to split-page and overlapping-copy proofs.
    Some(DirectSpan {
        base: address - before,
        length: (before + after) as u32,
        host: byte_token(ram)? - before as u32,
    })
}
