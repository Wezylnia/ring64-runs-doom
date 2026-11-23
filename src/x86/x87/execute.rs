use super::number::{self, Class, INDEFINITE, SIGN};
use super::*;
use std::cmp::Ordering;
fn status(state: &mut State, r: number::Result) {
    state.status_word = (state.status_word & !0x200) | if r.up { 0x200 } else { 0 };
    state.status_word |= r.pre | r.post;
    state.refresh();
}
fn stack_fault(state: &mut State, overflow: bool) -> bool {
    state.status_word = (state.status_word & !0x200) | 0x41 | if overflow { 0x200 } else { 0 };
    state.refresh();
    state.control_word & 1 == 0
}
#[inline(always)]
fn push(state: &mut State, value: u128) {
    let p = (state.top() + 7) & 7;
    state.top_to(p);
    state.put(p, value)
}
#[inline(never)]
fn sincos(state: &mut State, top: usize, raw: u128) {
    state.status_word &= !0x400;
    let empty = state.tag(top) == 3;
    if empty || state.tag((top + 7) & 7) != 3 {
        if !stack_fault(state, !empty) {
            state.put(top, INDEFINITE);
            push(state, INDEFINITE);
        }
        return;
    }
    if number::sincos_out_of_range(raw) {
        state.status_word = (state.status_word & !0x200) | 0x400;
        return;
    }
    let (sine, cosine) = number::sincos(raw, state.control_word);
    status(state, sine);
    if sine.pre & !state.control_word & 63 == 0 {
        state.put(top, sine.value);
        push(state, cosine);
    }
}

fn predicate(c: u8, flags: u64) -> bool {
    let cf = flags & 1 != 0;
    let zf = flags & 64 != 0;
    let pf = flags & 4 != 0;
    match c {
        0 => cf,
        1 => zf,
        2 => cf || zf,
        3 => pf,
        4 => !cf,
        5 => !zf,
        6 => !cf && !zf,
        _ => !pf,
    }
}
fn pops(state: &mut State, count: u8) {
    for _ in 0..count {
        state.pop()
    }
}
#[inline(always)]
fn finish_numeric(state: &mut State, destination: usize, r: number::Result, pop: u8) {
    status(state, r);
    if !r.suppress && r.pre & !state.control_word & 63 == 0 {
        state.put(destination, r.value);
        pops(state, pop);
    }
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[inline(always)]
pub(crate) fn subtract_register(state: &mut State, index: u8) -> bool {
    let top = state.top();
    let other = state.physical(index);
    if state.tag(top) == 3 || state.tag(other) == 3 {
        return false;
    }
    let result = number::binary(
        Kind::Sub,
        state.raw(top),
        state.raw(other),
        state.control_word,
        0,
    );
    finish_numeric(state, top, result, 0);
    true
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) fn admitted_exchange_compare(
    state: &mut State,
    exchange: u8,
    compare: Op,
    flags: u64,
) -> Option<u64> {
    let top = state.top();
    let exchange = state.physical(exchange);
    let compare_slot = state.physical(compare.index);
    let old_top = state.raw(top);
    let lhs = state.raw(exchange);
    let rhs = if compare_slot == top {
        lhs
    } else if compare_slot == exchange {
        old_top
    } else {
        if state.tag(compare_slot) == 3 {
            return None;
        }
        state.raw(compare_slot)
    };
    let key = |raw: u128| {
        let magnitude = raw & (SIGN - 1);
        if magnitude == 0 {
            return Some(SIGN);
        }
        let exponent = (magnitude >> 64) as u16;
        if exponent.wrapping_sub(1) >= 0x7ffe || (raw as u64) >> 63 == 0 {
            return None;
        }
        // Normal binary80 magnitudes are lexicographic (exponent, significand).
        Some(if raw & SIGN == 0 {
            SIGN | magnitude
        } else {
            (SIGN - 1) ^ magnitude
        })
    };
    let code = match key(lhs)?.cmp(&key(rhs)?) {
        Ordering::Less => 1,
        Ordering::Equal => 0x40,
        Ordering::Greater => 0,
    };
    state.status_word &= !(0x8080 | 0x200);
    state.put(top, lhs);
    state.put(exchange, old_top);
    pops(state, compare.pop);
    Some((flags & !0x8d5) | code)
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(super) fn load_memory_arithmetic(state: &mut State, index: u8, op: Op, source: u128) {
    let (destination, lhs) = state.load_admitted_register(index);
    let rhs = number::widen(source, op.format);
    let extra = u16::from(number::memory_denormal(source, op.format)) << 1;
    let result = number::binary(op.kind, lhs, rhs, state.control_word, extra);
    finish_numeric(state, destination, result, 0);
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(super) fn load_memory_register(
    state: &mut State,
    format: Format,
    source: u128,
    index: u8,
) -> bool {
    let top = state.top();
    let first = (top + 7) & 7;
    let second = (top + 6) & 7;
    let from = (first + usize::from(index)) & 7;
    if state.tag(first) != 3 || state.tag(second) != 3 || from != first && state.tag(from) == 3 {
        return false;
    }
    let result = number::load(source, format);
    if (result.pre | result.post) & !state.control_word & 63 != 0 {
        return false;
    }
    status(state, result);
    push(state, result.value);
    state.load_admitted_register(index);
    true
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(super) fn memory_arithmetic_store(
    state: &mut State,
    first: Op,
    source: u128,
    second: Op,
) -> Option<Option<u64>> {
    let top = state.top();
    if state.tag(top) == 3 {
        return None;
    }
    let cw = state.control_word;
    let arithmetic =
        number::paired_memory_binary(first.kind, state.raw(top), source, first.format, cw);
    if arithmetic.suppress || (arithmetic.pre | arithmetic.post) & !cw & 63 != 0 {
        return None;
    }
    let store = number::store(arithmetic.value, second.format, cw);
    // Even a suppressed store observes the first instruction's rounded register.
    finish_numeric(state, top, arithmetic, 0);
    status(state, store);
    if store.suppress || store.pre & !cw & 63 != 0 {
        return Some(None);
    }
    pops(state, second.pop);
    Some(Some(store.value as u64))
}

fn examined(state: &mut State) {
    let p = state.top();
    let raw = state.raw(p);
    let code = if state.tag(p) == 3 {
        0x4100
    } else {
        match number::class(raw) {
            Class::Zero => 0x4000,
            Class::Finite => {
                if raw & 0x7fff0000000000000000 == 0 {
                    0x4400
                } else {
                    0x400
                }
            }
            Class::Infinity => 0x500,
            Class::QuietNan | Class::SignalingNan => 0x100,
            Class::Unsupported => 0,
        }
    };
    state.status_word =
        (state.status_word & !0x4700) | code | if raw & SIGN != 0 { 0x200 } else { 0 };
}
pub(super) fn eval(state: &mut State, op: Op, source: u128, flags: u64) -> Effect {
    use Kind::*;
    let mut out = Effect {
        value: 0,
        flags,
        store: false,
        ax: false,
    };
    let top = state.top();
    let other = state.physical(op.index);
    let a = state.raw(top);
    let cw = state.control_word;
    match op.kind {
        Init => {
            let registers = state.registers;
            *state = State {
                registers,
                ..State::default()
            };
            return out;
        }
        Clear => {
            state.status_word &= !0x80ff;
            return out;
        }
        LoadControl => {
            state.control_word = source as u16;
            state.refresh();
            return out;
        }
        StoreControl => {
            out.value = u128::from(state.control_word);
            out.store = true;
            return out;
        }
        StoreStatus => {
            out.value = u128::from(state.status_word);
            out.ax = op.format == Format::Ax;
            out.store = !out.ax;
            return out;
        }
        Wait | Nop => return out,
        Free => {
            state.set_tag(other, 3);
            return out;
        }
        DecTop | IncTop => {
            state.top_to((top + if op.kind == DecTop { 7 } else { 1 }) & 7);
            state.status_word &= !0x200;
            return out;
        }
        Examine => {
            examined(state);
            return out;
        }
        Load | Constant => {
            let source_empty =
                op.kind == Load && op.format == Format::Register && state.tag(other) == 3;
            let occupied = state.tag((top + 7) & 7) != 3;
            if source_empty || occupied {
                if stack_fault(state, !source_empty) {
                    return out;
                }
                push(state, INDEFINITE);
                return out;
            }
            let r = if op.kind == Constant {
                number::Result::raw(number::constant(op.index, cw))
            } else if op.format == Format::Register {
                number::Result::raw(state.raw(other))
            } else {
                number::load(source, op.format)
            };
            status(state, r);
            if r.pre & !cw & 1 != 0 {
                return out;
            }
            push(state, r.value);
            return out;
        }
        Exchange => {
            let ea = state.tag(top) == 3;
            let eb = state.tag(other) == 3;
            if (ea || eb) && stack_fault(state, false) {
                return out;
            }
            if !ea && !eb {
                state.status_word &= !0x200
            }
            let va = if ea { INDEFINITE } else { a };
            let vb = if eb { INDEFINITE } else { state.raw(other) };
            state.put(top, vb);
            state.put(other, va);
            return out;
        }
        ConditionalMove => {
            if state.tag(top) == 3 || state.tag(other) == 3 {
                if !stack_fault(state, false) {
                    state.put(top, INDEFINITE)
                }
                return out;
            }
            if predicate(op.condition, flags) {
                state.put(top, state.raw(other))
            }
            return out;
        }
        SinCos => {
            sincos(state, top, a);
            return out;
        }
        _ => {}
    }
    if op.kind == Extract {
        let empty = state.tag(top) == 3;
        if empty || state.tag((top + 7) & 7) != 3 {
            if !stack_fault(state, !empty) {
                state.put(top, INDEFINITE);
                push(state, INDEFINITE);
            }
            return out;
        }
        let (result, exponent) = number::extract(a);
        status(state, result);
        if result.pre & !cw & 63 == 0 {
            state.put(top, exponent);
            push(state, result.value);
        }
        return out;
    }
    if matches!(op.kind, Remainder | RemainderNearest | Scale) {
        let second = state.physical(1);
        if state.tag(top) == 3 || state.tag(second) == 3 {
            if !stack_fault(state, false) {
                state.put(top, INDEFINITE);
                if op.kind != Scale {
                    state.status_word &= !0x400;
                }
            }
            return out;
        }
        let (result, condition) = if op.kind == Scale {
            (number::scale(a, state.raw(second), cw), 0)
        } else {
            number::remainder(a, state.raw(second), op.kind == RemainderNearest, cw)
        };
        status(state, result);
        if !result.suppress && result.pre & !cw & 63 == 0 {
            state.put(top, result.value);
            if op.kind != Scale {
                state.status_word = (state.status_word & !0x4700) | condition;
            }
        }
        return out;
    }
    let compares = matches!(
        op.kind,
        Compare | CompareUnordered | CompareFlags | CompareUnorderedFlags | Test
    );
    let store = matches!(op.kind, Store | StoreTruncated);
    let empty =
        state.tag(top) == 3 || (op.format == Format::Register && !store && state.tag(other) == 3);
    if empty {
        let trapped = stack_fault(state, false);
        if op.writes_flags() {
            out.flags &= !0x890
        }
        if trapped {
            return out;
        }
        if compares {
            if op.writes_flags() {
                out.flags |= 0x45
            } else {
                state.status_word = (state.status_word & !0x4500) | 0x4500
            }
        } else if store {
            let r = number::store(INDEFINITE, op.format, cw | 1);
            out.value = r.value;
            if op.format == Format::Register {
                state.put(other, INDEFINITE)
            } else {
                out.store = true
            }
        } else {
            let p = if op.dest_index { other } else { top };
            state.put(p, INDEFINITE)
        }
        pops(state, op.pop);
        return out;
    }
    if store {
        let conversion_control = if op.kind == StoreTruncated {
            cw | 0x0c00
        } else {
            cw
        };
        let r = number::store(a, op.format, conversion_control);
        status(state, r);
        if r.suppress || r.pre & !cw & 63 != 0 {
            return out;
        }
        if op.format == Format::Register {
            state.put(other, r.value)
        } else {
            out.value = r.value;
            out.store = true
        }
        pops(state, op.pop);
        return out;
    }
    if op.kind == ChangeSign || op.kind == Abs {
        state.status_word &= !0x200;
        state.put(top, if op.kind == Abs { a & !SIGN } else { a ^ SIGN });
        return out;
    }
    let b = if op.kind == Test {
        0
    } else if op.format == Format::Register {
        state.raw(other)
    } else {
        number::widen(source, op.format)
    };
    if compares {
        let ca = number::class(a);
        let cb = number::class(b);
        let unordered = matches!(op.kind, CompareUnordered | CompareUnorderedFlags);
        let invalid = matches!(ca, Class::Unsupported | Class::SignalingNan)
            || matches!(cb, Class::Unsupported | Class::SignalingNan)
            || !unordered && (ca == Class::QuietNan || cb == Class::QuietNan);
        let order = number::compare(a, b);
        let pre = if invalid {
            1
        } else if order.is_none() {
            0
        } else if !op.writes_flags()
            && (number::denormal(a)
                || number::denormal(b)
                || number::memory_denormal(source, op.format))
        {
            2
        } else {
            0
        };
        state.status_word &= !0x200;
        state.status_word |= pre;
        state.refresh();
        if op.writes_flags() {
            out.flags &= !0x890
        }
        if pre & !cw & 63 != 0 {
            return out;
        }
        let code = match order {
            None => 0x45,
            Some(Ordering::Less) => 1,
            Some(Ordering::Equal) => 0x40,
            Some(Ordering::Greater) => 0,
        };
        if op.writes_flags() {
            out.flags = (out.flags & !0x45) | code
        } else {
            let c = if code == 0x45 {
                0x4500
            } else if code == 1 {
                0x100
            } else if code == 0x40 {
                0x4000
            } else {
                0
            };
            state.status_word = (state.status_word & !0x4500) | c
        }
        pops(state, op.pop);
        return out;
    }
    let r = match op.kind {
        Exp2MinusOne | MulLog2 | MulLog2PlusOne => number::transcendental(op.kind, a, b, cw),
        Atan2 => number::atan2(a, b, cw),
        Sqrt | Round => number::unary(op.kind, a, cw),
        _ => {
            let (lhs, rhs) = if op.dest_index { (b, a) } else { (a, b) };
            let extra = if number::memory_denormal(source, op.format) {
                2
            } else {
                0
            };
            number::binary(op.kind, lhs, rhs, cw, extra)
        }
    };
    finish_numeric(state, if op.dest_index { other } else { top }, r, op.pop);
    out
}
