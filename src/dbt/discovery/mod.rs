mod fetch_proof;
use fetch_proof::FetchPage;
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) use fetch_proof::FetchProof;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::memory::{Access, Image, canonical48};
use crate::x86::{
    context::{AccessFailure, ExecutionContext, Segment},
    decode::DecodeError,
    instruction::{Operand, Operation},
};

use super::{CompileError, EntrySelection, Limits, SourceRange, unit::Unit};

#[cfg(test)]
pub(crate) fn discover(image: &Image, entry: u64, limits: Limits) -> Result<Unit, CompileError> {
    discover_with_context(image, &ExecutionContext::default(), entry, limits)
}

#[cfg(test)]
pub(crate) fn discover_with_context(
    image: &Image,
    context: &ExecutionContext,
    entry: u64,
    limits: Limits,
) -> Result<Unit, CompileError> {
    discover_with_profile(image, context, None, None, entry, limits)
}

#[cfg(test)]
pub(crate) fn discover_with_profile(
    image: &Image,
    context: &ExecutionContext,
    paging: Option<&crate::x86::paging::State>,
    cache: Option<&crate::x86::paging::cache::Cache>,
    entry: u64,
    limits: Limits,
) -> Result<Unit, CompileError> {
    discover_with_entries(
        image,
        context,
        paging,
        cache,
        entry,
        limits,
        (EntrySelection::Discover(&[]), false),
    )
}

pub(super) fn discover_with_entries(
    image: &Image,
    context: &ExecutionContext,
    paging: Option<&crate::x86::paging::State>,
    cache: Option<&crate::x86::paging::cache::Cache>,
    entry: u64,
    limits: Limits,
    (entries, _observed): (EntrySelection<'_>, bool),
) -> Result<Unit, CompileError> {
    let (seeds, sampled) = match entries {
        EntrySelection::Discover(seeds) => (seeds, false),
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        EntrySelection::Sampled(starts) => (starts, true),
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        EntrySelection::Traced(starts) => (starts, true),
    };
    let mut pending = VecDeque::from([entry]);
    // Linear traces record already executed paths. Start all those entries
    // before exploring less frequently taken successors under the same cap.
    let linear_region = paging.is_none() && !seeds.is_empty();
    if linear_region || sampled {
        pending.extend(seeds);
    }
    let mut seeds_to_visit = seeds.iter().copied().filter(|_| !linear_region && !sampled);
    let paged_region = paging.is_some() && !seeds.is_empty() && context.is_long64();
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    let prefer_local = !_observed
        && !image.has_mmio()
        && paging.is_none()
        && !sampled
        && seeds.is_empty()
        && context.mode() == crate::x86::context::Mode::Protected
        && context.nonwrapping_code32_fetch(0, 1) == Some(0);
    #[cfg(any(feature = "parallel", not(any(target_arch = "wasm32", test))))]
    let prefer_local = false;
    let deferred_distance = u64::from(limits.max_source_bytes.div_ceil(2));
    let enqueue_next =
        |pending: &mut VecDeque<u64>, visited: &BTreeSet<u64>, pc: u64, next: u64| {
            // Preserve nearby alternative arms. When queued work is far away,
            // complete the current path before spending its source budget there.
            if prefer_local
                && pc >> 12 == next >> 12
                && pending.iter().any(|queued| {
                    pc.abs_diff(*queued) >= deferred_distance
                        && pc >> 12 == queued >> 12
                        && !visited.contains(queued)
                })
            {
                pending.push_front(next);
            } else {
                pending.push_back(next);
            }
        };
    let mut loop_exits = VecDeque::new();
    let mut visited = BTreeSet::new();
    let mut instructions = BTreeMap::new();
    let mut source = BTreeSet::new();
    let mut linear_source = BTreeSet::new();
    let mut fetch_pages = BTreeMap::new();
    let mut instruction_pages = Vec::new();
    let identity_source = context.is_long64() && paging.is_none();
    while let Some(pc) = pending
        .pop_front()
        .or_else(|| seeds_to_visit.next())
        .or_else(|| loop_exits.pop_front())
    {
        if instructions.len() >= limits.max_instructions as usize {
            break;
        }
        if (linear_region || sampled) && pc >> 12 != entry >> 12 || !visited.insert(pc) {
            continue;
        }
        let mut fetched = Vec::new();
        instruction_pages.clear();
        let Ok(instruction) =
            crate::x86::decode::decode_for_translation(pc, context, paging.is_some(), |offset| {
                // Long64 CS offsets are linear; fetch already checks the full byte span.
                if identity_source {
                    return image.fetch(offset).map_err(DecodeError::Fetch);
                }
                let linear = context
                    .translate(offset, 1, Segment::Cs, Access::Execute, false)
                    .map_err(|failure| match failure {
                        AccessFailure::Segment(fault) => DecodeError::Segment(fault),
                        AccessFailure::Linear(fault) => DecodeError::Fetch(fault),
                        AccessFailure::Paging(fault) => DecodeError::Paging(fault),
                        AccessFailure::AlignmentCheck(_) => {
                            unreachable!("instruction fetch has no natural alignment checking")
                        }
                    })?;
                if let Some(paging) = paging {
                    let request = crate::x86::paging::walk::Request {
                        linear,
                        access: Access::Execute,
                        class: crate::x86::paging::walk::AccessClass::operand(context.cpl(), false),
                    };
                    let translated = match cache {
                        // Discovery observes instruction bytes; key rights never restrict fetch.
                        Some(cache) => cache.peek(
                            image,
                            paging,
                            request,
                            &crate::x86::protection_keys::State::ZERO,
                        ),
                        None => crate::x86::paging::walk::peek(
                            image,
                            paging,
                            request,
                            &crate::x86::protection_keys::State::ZERO,
                        ),
                    }
                    .map_err(DecodeError::Paging)?;
                    let byte = image
                        .physical_fetch(translated.physical)
                        .map_err(DecodeError::Fetch)?;
                    fetched.push(translated.physical);
                    FetchPage::record(&mut instruction_pages, linear, &translated);
                    return Ok(byte);
                }
                let byte = image.fetch(linear).map_err(DecodeError::Fetch)?;
                fetched.push(linear);
                Ok(byte)
            })
        else {
            // A future diagnostic is a frontier, never an early guest exit.
            continue;
        };
        let length = u64::from(instruction.encoded_len());
        if (linear_region || sampled)
            && pc
                .checked_add(length - 1)
                .is_none_or(|last| last >> 12 != entry >> 12)
        {
            continue;
        }
        let bytes: Vec<_> = (0..length).map(|offset| pc.wrapping_add(offset)).collect();
        let additional = bytes
            .iter()
            .filter(|address| !source.contains(*address))
            .count();
        if source.len() + additional > limits.max_source_bytes as usize {
            continue;
        }
        source.extend(bytes);
        linear_source.extend(fetched);
        for page in instruction_pages.drain(..) {
            // Both the image and the effective translation view are immutable during discovery.
            let previous = fetch_pages.entry(page.linear).or_insert(page);
            debug_assert_eq!(*previous, page);
        }
        if sampled {
            // The sampled instructions already compete for the same limits.
            // An uncovered successor remains a normal dispatcher exit.
            instructions.insert(pc, instruction);
            continue;
        }
        match instruction.operation {
            Operation::Jump { target } => {
                if !context.is_long64() || canonical48(target) {
                    enqueue_next(&mut pending, &visited, pc, target);
                }
            }
            Operation::Jcc { target, .. } | Operation::CountBranch { target, .. } => {
                if paged_region && target <= entry && entry <= pc && target >> 12 == pc >> 12 {
                    // Fit the observed targets before the fallthrough of their root loop.
                    loop_exits.push_back(instruction.next_pc);
                } else {
                    enqueue_next(&mut pending, &visited, pc, instruction.next_pc);
                }
                if !context.is_long64() || canonical48(target) {
                    pending.push_back(target);
                }
            }
            Operation::Call { target, .. } => {
                pending.push_back(instruction.next_pc);
                if let Operand::Immediate(target) = target
                    && (!context.is_long64() || canonical48(target))
                {
                    pending.push_back(target);
                }
            }
            Operation::JumpRegister { .. }
            | Operation::JumpMemory { .. }
            | Operation::Return { .. }
            | Operation::Far(_)
            | Operation::FastSystem(_)
            | Operation::SegmentBase { write: true, .. }
            | Operation::Halt => {}
            Operation::SoftwareEvent(event) => {
                if event.kind == crate::x86::instruction::SoftwareEventKind::Into {
                    pending.push_back(instruction.next_pc);
                }
            }
            _ => enqueue_next(&mut pending, &visited, pc, instruction.next_pc),
        }
        instructions.insert(pc, instruction);
    }
    if !instructions.contains_key(&entry)
        || (!seeds.is_empty() && !seeds.iter().any(|pc| instructions.contains_key(pc)))
    {
        return Err(CompileError::Resource);
    }
    let mut leaders = BTreeSet::from([entry]);
    leaders.extend(seeds.iter().filter(|pc| instructions.contains_key(pc)));
    for instruction in instructions.values() {
        match instruction.operation {
            Operation::Jump { target } => {
                leaders.insert(target);
            }
            Operation::Jcc { target, .. } | Operation::CountBranch { target, .. } => {
                leaders.insert(target);
                leaders.insert(instruction.next_pc);
            }
            Operation::Call { target, .. } => {
                leaders.insert(instruction.next_pc);
                if let Operand::Immediate(target) = target {
                    leaders.insert(target);
                }
            }
            Operation::SoftwareEvent(event)
                if event.kind == crate::x86::instruction::SoftwareEventKind::Into =>
            {
                leaders.insert(instruction.next_pc);
            }
            _ => {}
        }
    }
    let source_ranges = ranges(if identity_source {
        &source
    } else {
        &linear_source
    });
    let mut read_dependencies = BTreeSet::new();
    for page in fetch_pages.values() {
        for &entry in page.entries() {
            read_dependencies.extend(entry..entry + u64::from(page.entry_width));
        }
    }
    Ok(Unit {
        context: context.clone(),
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        sampled,
        block_count: leaders
            .iter()
            .filter(|pc| instructions.contains_key(pc))
            .count(),
        instructions: instructions.into_values().collect(),
        source_bytes: source.len(),
        source_ranges,
        read_dependencies: ranges(&read_dependencies),
        #[cfg(any(target_arch = "wasm32", test))]
        fetch_proof: FetchProof {
            pages: fetch_pages.into_values().collect(),
        },
    })
}

fn ranges(bytes: &BTreeSet<u64>) -> Vec<SourceRange> {
    let mut ranges: Vec<SourceRange> = Vec::new();
    for &address in bytes {
        if let Some(last) = ranges.last_mut()
            && u128::from(last.start) + u128::from(last.length) == u128::from(address)
        {
            last.length += 1;
        } else {
            ranges.push(SourceRange {
                start: address,
                length: 1,
            });
        }
    }
    ranges
}

#[cfg(test)]
mod tests;
