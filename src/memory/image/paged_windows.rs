// Paging proofs carry 32-bit offsets. Each offset names bytes in a window of
// the guest arena. Its bias stays fixed while generated code can use the proof.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use super::{DirectSpan, Mapping};

pub(crate) const WINDOW_SHIFT: u32 = 20;
const SLOTS: usize = 1 << (32 - WINDOW_SHIFT);
const GUARD: u64 = 4096;
const PAYLOAD: u64 = (1 << WINDOW_SHIFT) - GUARD;

thread_local! {
    static BIASES: [Cell<u64>; SLOTS] = const { [const { Cell::new(0) }; SLOTS] };
    static SLOTS_BY_BASE: RefCell<BTreeMap<u64, u32>> = const { RefCell::new(BTreeMap::new()) };
    static LAST: Cell<Option<(u64, u32)>> = const { Cell::new(None) };
    static EXHAUSTED: Cell<bool> = const { Cell::new(false) };
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
    let inside = ram % PAYLOAD;
    Some(window_token(ram - inside)? + inside as u32)
}

pub(super) fn span(
    mapping: &Mapping,
    ram_base: u64,
    address: u64,
    width: u32,
) -> Option<DirectSpan> {
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
