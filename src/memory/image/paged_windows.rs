// Paging proofs carry 32-bit offsets. Each offset names bytes in an immutable
// window of the guest arena. Generated Wasm adds the window bias in i64.
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
}

pub(crate) fn biases_pointer() -> u32 {
    BIASES.with(|table| table.as_ptr() as u32)
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
    let token = window_token(ram - inside)?;
    Some(DirectSpan {
        base: address - before,
        length: (before + after) as u32,
        host: token + (inside - before) as u32,
    })
}
