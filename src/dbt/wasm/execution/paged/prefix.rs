use crate::dbt::wasm::{Emitter, SHADOW, StructuredLoop, Unit, execution::loops::structured_loop};
use crate::x86::instruction::{Alu, Instruction, Location, Operation};
use wasm_encoder::{BlockType, Instruction as Op};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::dbt::wasm) enum PagedPrefixFetch {
    Checked,
    Retained,
    RetainedSse,
}

pub(in crate::dbt::wasm) fn paged_prefix_read(
    instruction: Instruction,
) -> Option<(
    crate::x86::instruction::RegisterView,
    crate::x86::instruction::Address,
    crate::x86::instruction::Width,
    crate::x86::instruction::Width,
)> {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{Location, Width};

    let (dst, address, source_width, width) = match instruction.operation {
        Operation::Extend {
            dst,
            src: Location::Memory(address),
            source_width: source_width @ (Width::W8 | Width::W16),
            width,
            signed: false,
        } => (dst, address, source_width, width),
        Operation::Load {
            dst,
            address,
            width: Width::W32,
        } => (dst, address, Width::W32, Width::W32),
        _ => return None,
    };
    (address.segment == Segment::Ds).then_some((dst, address, source_width, width))
}

pub(in crate::dbt::wasm) fn paged_prefix_narrow_read(instruction: Instruction) -> bool {
    use crate::x86::instruction::Width;
    paged_prefix_read(instruction)
        .is_some_and(|(_, _, width, _)| matches!(width, Width::W8 | Width::W16))
}

pub(in crate::dbt::wasm) fn paged_prefix_dword_read(instruction: Instruction) -> bool {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{Location, MemoryAluOperands, Width};

    let address = match instruction.operation {
        Operation::Load {
            address,
            width: Width::W32,
            ..
        }
        | Operation::Imul {
            src: Location::Memory(address),
            width: Width::W32,
            ..
        } => address,
        Operation::MemoryAlu {
            kind,
            address,
            operands,
            width: Width::W32,
        } if matches!(operands, MemoryAluOperands::RegisterFirst(_)) || !kind.writes_result() => {
            address
        }
        _ => return false,
    };
    address.segment == Segment::Ds
}

pub(in crate::dbt::wasm) fn paged_prefix_wide_read(instruction: Instruction) -> bool {
    use crate::x86::context::Segment;
    use crate::x86::instruction::{MemoryAluOperands, Width};
    matches!(
        instruction.operation,
        Operation::MemoryAlu {
            kind: Alu::Cmp,
            width: Width::W64,
            operands: MemoryAluOperands::RegisterFirst(_),
            address,
        } if matches!(address.segment, Segment::Ds | Segment::Ss)
    )
}

pub(in crate::dbt::wasm) fn paged_prefix_dword_store(instruction: Instruction) -> bool {
    use crate::x86::{
        context::Segment,
        instruction::{Address, Width},
    };

    matches!(
        instruction.operation,
        Operation::Store {
            address: Address {
                segment: Segment::Ds,
                ..
            },
            width: Width::W32,
            ..
        }
    )
}

pub(in crate::dbt::wasm) fn register_prefix_operation(operation: Operation) -> bool {
    structured_loop::inside_register_body(operation)
        || matches!(
            operation,
            Operation::Extend {
                src: Location::Register(_),
                ..
            }
        )
}

// A clone can touch the executable leaf once when every admitted instruction
// stays on that leaf and can neither access memory nor change the controls.
pub(in crate::dbt::wasm) fn paged_register_prefixes(
    unit: &Unit,
    loops: [Option<StructuredLoop>; 2],
    short_indexed_reads: bool,
) -> Vec<Option<usize>> {
    let instructions = &unit.instructions;
    let long64 = unit.context.is_long64();
    let short_register_prefixes = long64 && unit.sampled;
    let interior_scalar_read = |instruction: Instruction| {
        short_register_prefixes && unit.context.cpl() == 0 && paged_prefix_narrow_read(instruction)
    };
    let maximum = if long64 { 12 } else { 8 };
    let mut targets = vec![false; instructions.len()];
    if long64 {
        for instruction in instructions {
            let target = match instruction.operation {
                Operation::Jump { target }
                | Operation::Jcc { target, .. }
                | Operation::CountBranch { target, .. } => target,
                _ => continue,
            };
            if let Ok(index) = instructions.binary_search_by_key(&target, |i| i.pc) {
                targets[index] = true;
            }
        }
    }
    let local_backedge = |instruction: &Instruction| {
        matches!(instruction.operation, Operation::Jump { target }
            if target < instruction.pc
                && target >> 12 == instruction.pc >> 12
                && instructions.binary_search_by_key(&target, |i| i.pc).is_ok())
    };
    let terminal_branch = |instruction: &Instruction, has_read: bool, wide_read: bool| {
        long64
            && match instruction.operation {
                Operation::Jcc { target, .. } => {
                    crate::memory::canonical48(target)
                        && ((short_register_prefixes
                            && (!has_read || target >> 12 == instruction.pc >> 12))
                            || wide_read
                            || instructions.binary_search_by_key(&target, |i| i.pc).is_ok())
                }
                Operation::Jump { target } => {
                    short_register_prefixes
                        && !has_read
                        && target >> 12 == instruction.pc >> 12
                        && instructions.binary_search_by_key(&target, |i| i.pc).is_ok()
                }
                _ => false,
            }
    };
    let mut prefixes = vec![None; instructions.len()];
    let mut first = 0;
    while first < instructions.len() {
        let pc = instructions[first].pc;
        let mut end = first;
        let mut has_read = false;
        let mut has_wide_read = false;
        let mut has_branch = false;
        while let Some(instruction) = instructions.get(end)
            && end - first < maximum
            && (end == first || !targets[end])
            && !loops
                .into_iter()
                .flatten()
                .any(|body| body.order().position(end as u32).is_some())
            && (register_prefix_operation(instruction.operation)
                || (if has_read {
                    has_wide_read || end - first >= 2
                } else {
                    short_register_prefixes && (end > first || local_backedge(instruction))
                } && terminal_branch(instruction, has_read, has_wide_read))
                || interior_scalar_read(*instruction)
                || (short_register_prefixes && !has_read && paged_prefix_wide_read(*instruction))
                || (long64
                    && !has_wide_read
                    && if end == first {
                        paged_prefix_read(*instruction).is_some()
                    } else {
                        paged_prefix_dword_read(*instruction)
                            || has_read && paged_prefix_dword_store(*instruction)
                    }))
            && instruction.next_pc > instruction.pc
            && instruction.next_pc.checked_sub(pc).is_some_and(|bytes| {
                bytes <= 4096 && unit.context.single_page_code_span(pc, bytes as u32)
            })
            && (end == first || instructions[end - 1].next_pc == instruction.pc)
        {
            // Keep an already useful register-only prefix before a checked store.
            has_wide_read |= paged_prefix_wide_read(*instruction);
            has_read |= has_wide_read
                || interior_scalar_read(*instruction)
                || if end == first {
                    paged_prefix_read(*instruction).is_some()
                } else {
                    paged_prefix_dword_read(*instruction)
                };
            end += 1;
            if terminal_branch(instruction, has_read, has_wide_read) {
                has_branch = true;
                let extend_fallthrough = short_register_prefixes
                    && matches!(instruction.operation, Operation::Jcc { target, .. }
                        if target >> 12 == instruction.pc >> 12)
                    && instructions.get(end).is_some_and(|next| {
                        register_prefix_operation(next.operation)
                            || interior_scalar_read(*next)
                            || paged_prefix_dword_read(*next)
                            || paged_prefix_wide_read(*next)
                    });
                if !extend_fallthrough {
                    break;
                }
            }
        }
        // Each successful operand accounts for the next fetch. Leave a following
        // in-prefix instruction so a stop or exhausted budget cannot omit it.
        while end > first + 1
            && (interior_scalar_read(instructions[end - 1])
                || paged_prefix_dword_read(instructions[end - 1])
                || paged_prefix_dword_store(instructions[end - 1])
                || paged_prefix_wide_read(instructions[end - 1]))
        {
            end -= 1;
        }
        let minimum = if has_branch && local_backedge(&instructions[first]) {
            1
        } else if short_register_prefixes && (!has_read || has_wide_read && has_branch) {
            2
        } else if has_branch
            || short_indexed_reads
                && instructions[first..end]
                    .iter()
                    .copied()
                    .any(paged_prefix_dword_read)
        {
            3
        } else {
            4
        };
        if end - first >= minimum {
            prefixes[first] = Some(end);
        }
        first = end.max(first + 1);
    }
    prefixes
}

#[derive(Clone, Copy, Default)]
pub(in crate::dbt::wasm) struct PrefixFetchEdge {
    pub(in crate::dbt::wasm) receives: bool,
    pub(in crate::dbt::wasm) destination: Option<usize>,
}

// The admitted read-only clone leaves its executable translation newest and
// cannot change code or controls. A same-page successor needs no new touch.
pub(in crate::dbt::wasm) fn paged_prefix_handoffs(
    unit: &Unit,
    prefixes: &[Option<usize>],
    earlier_clones: [Option<usize>; 3],
) -> Option<Vec<PrefixFetchEdge>> {
    if !unit.context.is_long64() {
        return None;
    }
    let instructions = &unit.instructions;
    let mut edges = None;
    for (start, &end) in prefixes.iter().enumerate() {
        let Some(end) = end else { continue };
        let Operation::Jcc { target, .. } = instructions[end - 1].operation else {
            continue;
        };
        if target & !4095 != instructions[start].pc & !4095
            || instructions[start..end]
                .iter()
                .copied()
                .any(paged_prefix_dword_store)
        {
            continue;
        }
        if let Ok(destination) = instructions.binary_search_by_key(&target, |i| i.pc)
            && prefixes[destination].is_some()
            && !earlier_clones.contains(&Some(destination))
        {
            let edges =
                edges.get_or_insert_with(|| vec![PrefixFetchEdge::default(); prefixes.len()]);
            edges[destination].receives = true;
            edges[start].destination = Some(destination);
        }
    }
    edges
}

impl Emitter {
    pub(in crate::dbt::wasm) fn emit_guarded_register_prefix(
        &mut self,
        instructions: &[Instruction],
        loop_depth: u32,
        guard: u32,
        read_import: Option<u32>,
        write_import: Option<u32>,
        fetch_handoff: Option<(u32, bool, Option<usize>)>,
    ) {
        use crate::dbt::wasm::execution::control::InstructionRetirement;

        let first = instructions.first().unwrap();
        let last = instructions.last().unwrap();
        let leading_wide_read = paged_prefix_wide_read(*first);
        let incoming = fetch_handoff.and_then(|(local, incoming, _)| incoming.then_some(local));
        let read = paged_prefix_read(*first)
            .map(|operation| (read_import.expect("leading read prefix import"), operation));
        let indexed_dword = self.indexed_dword_prefixes
            && instructions.iter().copied().any(paged_prefix_dword_read);
        let indexed_wide = self.indexed_scalar_stamp.is_some()
            && instructions.iter().copied().any(paged_prefix_wide_read);
        let indexed_narrow = self.sampled_region
            && self.indexed_paged_read.is_some()
            && instructions
                .iter()
                .skip(1)
                .copied()
                .any(paged_prefix_narrow_read);
        let indexed_store = self.retouch_prefix_fetch
            && self.indexed_paged_read.is_some()
            && write_import.is_some()
            && instructions.iter().copied().any(paged_prefix_dword_store);
        let indexed_leading = read.is_some_and(|(_, (_, _, width, _))| {
            if width == crate::x86::instruction::Width::W32 {
                indexed_dword
            } else {
                self.sampled_region && incoming.is_none() && self.indexed_paged_read.is_some()
            }
        });
        // A handoff can bypass its guard only directly from a read-only
        // prefix in this unit. Wide reads must admit their index at the
        // checked entry because other prefixes can hand off fetch alone.
        let indexed = indexed_leading
            || self.indexed_dword_prefixes
            || indexed_wide
            || indexed_narrow
            || indexed_store;
        // A first operand can refuse before any guest effect. After it,
        // register-only work can retire together at each possible branch exit.
        let batch_retirement = self.context.is_long64()
            && instructions.len() > 1
            && instructions.iter().enumerate().all(|(index, instruction)| {
                register_prefix_operation(instruction.operation)
                    || index == 0 && (read.is_some() || leading_wide_read)
                    || matches!(instruction.operation, Operation::Jcc { target, .. }
                        if crate::memory::canonical48(target))
                    || index + 1 == instructions.len()
                        && matches!(instruction.operation,
                        Operation::Jump { target } if crate::memory::canonical48(target))
            });
        let keep_quiet = self.sampled_region
            && self.quiet_fetch_page.is_some()
            && !instructions.iter().copied().any(paged_prefix_dword_store);
        self.guard_straight_paged_prefix(
            instructions,
            guard,
            incoming.filter(|_| !indexed_wide && !indexed_narrow && !indexed_store),
            indexed,
            if self.retouch_prefix_fetch
                || indexed_wide
                || indexed_narrow
                || !indexed
                || self.sampled_region && indexed_leading
            {
                PagedPrefixFetch::Retained
            } else {
                PagedPrefixFetch::Checked
            },
            keep_quiet,
        );
        if let Some((read, (register, address, source_width, width))) = read {
            use crate::dbt::wasm::{ADDRESS, STORE_STATUS};
            use crate::x86::instruction::Width;
            use wasm_encoder::{MemArg, ValType};
            let mem = |offset| MemArg {
                offset,
                align: 0,
                memory_index: 0,
            };

            self.address(address, first.next_pc);
            if indexed_leading {
                self.indexed_prefix_operand_host(read, source_width.bytes());
                self.op(Op::LocalTee(STORE_STATUS))
                    .op(Op::If(BlockType::Empty))
                    .op(Op::LocalGet(STORE_STATUS));
            } else {
                let check_alignment = source_width != Width::W8 && self.context.cpl() == 3;
                if check_alignment {
                    // A checked CPL3 load owns any naturally-unaligned #AC decision.
                    self.op(Op::LocalTee(ADDRESS))
                        .op(Op::I64Const(i64::from(source_width.bytes() - 1)))
                        .op(Op::I64And)
                        .op(Op::I64Eqz)
                        .op(Op::If(BlockType::Result(ValType::I32)))
                        .op(Op::LocalGet(ADDRESS));
                }
                let segment = if source_width == Width::W32 {
                    crate::embedding::layout::paging::PREFIX_DWORD_READ_SEGMENT
                } else {
                    address.segment as u32
                };
                self.op(Op::I32Const(i32::from(source_width.bytes())))
                    .op(Op::I32Const(segment as i32))
                    .op(Op::Call(read));
                if check_alignment {
                    self.op(Op::Else).op(Op::I32Const(0)).op(Op::End);
                }
                self.op(Op::LocalTee(STORE_STATUS))
                    .op(Op::If(BlockType::Empty))
                    .op(Op::LocalGet(STORE_STATUS))
                    .op(Op::I32Load(mem(
                        std::mem::offset_of!(crate::memory::DirectSpan, host) as u64,
                    )));
            }
            self.ram_load(
                match source_width {
                    Width::W8 => Op::I64Load8U,
                    Width::W16 => Op::I64Load16U,
                    Width::W32 => Op::I64Load32U,
                    Width::W64 => unreachable!("unsupported prefix read width"),
                },
                mem(0),
            );
            self.write_register(register, width);
            if !batch_retirement {
                self.commit(first.next_pc);
            }
        }
        if leading_wide_read {
            // A first-operand refusal drops directly to the checked entry.
            self.op(Op::Block(BlockType::Empty));
        }
        let loop_depth = loop_depth + 2 + u32::from(read.is_some() || leading_wide_read);
        self.guarded_paged_lap = true;
        self.loop_gates_prechecked = true;
        let outgoing =
            fetch_handoff.and_then(|(local, _, outgoing)| outgoing.map(|target| (local, target)));
        let mut deferred = u32::from(read.is_some());
        let guarded = &instructions[usize::from(read.is_some())..];
        let mut retired_read = false;
        for (index, instruction) in guarded.iter().enumerate() {
            let wide = paged_prefix_wide_read(*instruction);
            let fuse_branch = wide && guarded.get(index + 1).is_some_and(|next| {
                matches!(next.operation, Operation::Jcc { target, .. } if crate::memory::canonical48(target))
            });
            let terminal = index + 1 == guarded.len();
            self.paged_prefix_fetch_out = if terminal { outgoing } else { None };
            self.instruction_retirement = if batch_retirement {
                if terminal || matches!(instruction.operation, Operation::Jcc { .. }) {
                    InstructionRetirement::AfterPrefix(std::mem::take(&mut deferred))
                } else {
                    deferred += 1;
                    InstructionRetirement::DeferredCommit
                }
            } else if fuse_branch {
                InstructionRetirement::DeferredCommit
            } else if retired_read {
                InstructionRetirement::AfterPrefix(1)
            } else {
                InstructionRetirement::One
            };
            retired_read = fuse_branch;
            self.paged_prefix_read = read_import
                .filter(|_| {
                    wide || paged_prefix_dword_read(*instruction)
                        || paged_prefix_narrow_read(*instruction)
                })
                .map(|read| {
                    (
                        read,
                        if wide {
                            indexed_wide
                        } else if paged_prefix_narrow_read(*instruction) {
                            indexed_narrow
                        } else {
                            indexed_dword
                        },
                        leading_wide_read && index == 0,
                    )
                });
            self.paged_prefix_store = write_import
                .filter(|_| paged_prefix_dword_store(*instruction))
                .map(|write| (write, indexed_store));
            self.emit_instruction(*instruction, None, loop_depth, None, None, false);
        }
        self.instruction_retirement = InstructionRetirement::One;
        self.paged_prefix_read = None;
        self.paged_prefix_store = None;
        self.paged_prefix_fetch_out = None;
        self.loop_gates_prechecked = false;
        self.route(last.next_pc, loop_depth, loop_depth + 1);
        self.guarded_paged_lap = false;
        if read.is_some() || leading_wide_read {
            self.op(Op::End);
        }
        self.op(Op::End).op(Op::End);
        if keep_quiet {
            // An operand refusal can fall through to the original entry.
            self.clear_quiet_fetch();
        }
        if let Some(local) = incoming {
            // A stop or short budget can reject the group before it consumes
            // the handoff. The checked copy cannot carry that proof forward.
            self.op(Op::I32Const(0)).op(Op::LocalSet(local));
        }
    }

    pub(in crate::dbt::wasm) fn newest_prefix_fetch(
        &mut self,
        pc: u64,
        indexed_read: bool,
        sse: bool,
    ) {
        use crate::dbt::wasm::STORE_STATUS;
        use crate::embedding::layout::debug::{Continuation, DebugState};
        use crate::embedding::layout::paging::{
            MXCSR_EXCEPTION_MASKS, NEWEST_FETCH_INDEXED, NEWEST_FETCH_SSE, NewestFetch,
        };
        use crate::embedding::layout::{RawEventState, RawInterruptState, RunState, StateStorage};
        use crate::x86::paging::cache::IndexedReadTable;
        use std::mem::offset_of;
        use wasm_encoder::{MemArg, ValType};

        let mem = |offset, align| MemArg {
            offset: offset as u64,
            align,
            memory_index: 0,
        };
        let field = |offset, align| mem(offset_of!(StateStorage, newest_fetch) + offset, align);
        let table = field(offset_of!(NewestFetch, table), 2);
        let slot = field(offset_of!(NewestFetch, slot), 2);
        let clock = mem(offset_of!(IndexedReadTable, clock), 3);
        let hint = mem(offset_of!(IndexedReadTable, hint), 2);
        let ages = mem(offset_of!(IndexedReadTable, ages), 3);
        let debug = offset_of!(RunState, debug);
        // A single-page activation can reuse the checked slot until it calls
        // Core. Generated operands still touch the LRU, so recheck its age.
        let retained = self.retained_fetch_slot.filter(|_| indexed_read && !sse);
        let failed = u32::from(retained.is_some());
        self.op(Op::Block(BlockType::Result(ValType::I32)))
            .op(Op::I32Const(0))
            .op(Op::Block(BlockType::Empty));
        if let Some(slot) = retained {
            self.guard_quiet_fetch_controls();
            self.op(Op::Block(BlockType::Empty))
                .op(Op::LocalGet(slot))
                .op(Op::LocalTee(STORE_STATUS))
                .indexed_miss(Op::BrIf(0));
        }
        self.op(Op::LocalGet(0))
            .op(Op::I64Load(field(offset_of!(NewestFetch, tag), 3)));
        let indexed = if indexed_read {
            NEWEST_FETCH_INDEXED
        } else {
            0
        };
        let numeric = if sse { NEWEST_FETCH_SSE } else { 0 };
        let ignored = (NEWEST_FETCH_INDEXED | NEWEST_FETCH_SSE) & !(indexed | numeric);
        if ignored != 0 {
            self.op(Op::I64Const(!ignored as i64)).op(Op::I64And);
        }
        let tag = (pc & !4095) | 1 | indexed | numeric;
        self.op(Op::I64Const(tag as i64))
            .op(Op::I64Ne)
            .indexed_miss(Op::BrIf(failed));
        if sse {
            // Keep the original masked-SSE admission when MXCSR changes.
            self.op(Op::LocalGet(0))
                .op(Op::I32Load(mem(offset_of!(RunState, mxcsr), 2)))
                .op(Op::I32Const(MXCSR_EXCEPTION_MASKS as i32))
                .op(Op::I32And)
                .op(Op::I32Const(MXCSR_EXCEPTION_MASKS as i32))
                .op(Op::I32Ne)
                .indexed_miss(Op::BrIf(failed));
        }
        if indexed_read && (self.retouch_prefix_fetch || self.sampled_region) {
            self.op(Op::LocalGet(0))
                .op(Op::I32Load(mem(
                    offset_of!(StateStorage, indexed_paged_reads),
                    2,
                )))
                .op(Op::LocalGet(0))
                .op(Op::I32Load(table))
                .op(Op::I32Ne)
                .indexed_miss(Op::BrIf(failed));
        }
        if retained.is_none() {
            self.guard_quiet_fetch_controls();
        }
        self.op(Op::LocalGet(0))
            .op(Op::I32Load(field(
                offset_of!(NewestFetch, source_invalid),
                2,
            )))
            .op(Op::I32Load8U(mem(0, 0)))
            .indexed_miss(Op::BrIf(failed))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(field(
                offset_of!(NewestFetch, cache_changed),
                2,
            )))
            .op(Op::I32Load8U(mem(0, 0)))
            .indexed_miss(Op::BrIf(failed))
            .op(Op::LocalGet(0))
            .op(Op::I32Load8U(mem(
                debug
                    + offset_of!(DebugState, registers)
                    + offset_of!(crate::x86::debug::State, dr7),
                0,
            )))
            .indexed_miss(Op::BrIf(failed))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(mem(
                debug + offset_of!(DebugState, continuation) + offset_of!(Continuation, slots),
                2,
            )))
            .indexed_miss(Op::BrIf(failed))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(mem(
                offset_of!(RunState, event) + offset_of!(RawEventState, kind),
                2,
            )))
            .indexed_miss(Op::BrIf(failed))
            .op(Op::LocalGet(0))
            .op(Op::I64Load(mem(offset_of!(RunState, interrupts), 2)))
            .op(Op::I64Eqz)
            .op(Op::I32Eqz)
            .indexed_miss(Op::BrIf(failed))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(mem(
                offset_of!(RunState, interrupts) + offset_of!(RawInterruptState, nmi_blocked),
                2,
            )))
            .indexed_miss(Op::BrIf(failed));
        if !self.retouch_prefix_fetch {
            self.op(Op::LocalGet(0))
                .op(Op::I32Load(table))
                .op(Op::I32Load(hint))
                .op(Op::LocalGet(0))
                .op(Op::I32Load(slot))
                .op(Op::I32Ne)
                .indexed_miss(Op::BrIf(failed));
        }
        self.op(Op::LocalGet(0))
            .op(Op::I32Load(table))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(slot))
            .op(Op::I32Const(3))
            .op(Op::I32Shl)
            .op(Op::I32Add)
            .op(Op::LocalTee(STORE_STATUS))
            .op(Op::I64Load(mem(offset_of!(IndexedReadTable, live_ids), 3)))
            .op(Op::LocalGet(0))
            .op(Op::I64Load(field(offset_of!(NewestFetch, id), 3)))
            .op(Op::I64Ne)
            .indexed_miss(Op::BrIf(failed));
        if let Some(slot) = retained {
            self.op(Op::LocalGet(STORE_STATUS))
                .op(Op::LocalSet(slot))
                .op(Op::End);
        }
        self.op(Op::LocalGet(0))
            .op(Op::I32Load(table))
            .op(Op::I64Load(clock))
            .op(Op::I64Const(-1))
            .op(Op::I64Eq)
            .indexed_miss(Op::BrIf(0))
            .op(Op::LocalGet(0))
            .op(Op::I32Load(table))
            .op(Op::I64Load(clock))
            .op(Op::LocalGet(STORE_STATUS))
            .op(Op::I64Load(ages))
            .op(Op::I64Ne);
        if self.retouch_prefix_fetch {
            // A CALL or RET can leave a data slot newest without revoking the
            // checked code identity. Mirror touch_current_fetch before the lap.
            self.op(Op::LocalGet(0))
                .op(Op::I32Load(table))
                .op(Op::I32Load(hint))
                .op(Op::LocalGet(0))
                .op(Op::I32Load(slot))
                .op(Op::I32Ne)
                .op(Op::I32Or)
                .op(Op::If(BlockType::Empty))
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::LocalGet(0))
                .op(Op::I32Load(table))
                .op(Op::I64Load(clock))
                .op(Op::I64Const(1))
                .op(Op::I64Add)
                .op(Op::I64Store(ages))
                .op(Op::LocalGet(0))
                .op(Op::I32Load(table))
                .op(Op::LocalGet(STORE_STATUS))
                .op(Op::I64Load(ages))
                .op(Op::I64Store(clock))
                .op(Op::LocalGet(0))
                .op(Op::I32Load(table))
                .op(Op::LocalGet(0))
                .op(Op::I32Load(slot))
                .op(Op::I32Store(hint));
            if let Some((_, _, rights)) = self.ordered_operand {
                self.op(Op::I32Const(0)).op(Op::LocalSet(rights));
            }
            self.op(Op::End);
        } else {
            self.indexed_miss(Op::BrIf(0));
        }
        self.op(Op::LocalGet(0))
            .op(Op::I32Const(crate::x86::context::Segment::Cs as i32))
            .op(Op::I32Store(mem(offset_of!(RunState, fault_segment), 2)))
            .op(Op::I32Const(1))
            .op(Op::Br(1))
            .op(Op::End)
            .op(Op::End);
    }

    fn guard_quiet_fetch_controls(&mut self) {
        self.system_bits();
        self.op(Op::I64Const(0x10100))
            .op(Op::I64And)
            .op(Op::I64Eqz)
            .op(Op::I32Eqz)
            .op(Op::LocalGet(SHADOW))
            .op(Op::I32Or)
            .indexed_miss(Op::BrIf(0));
    }

    pub(in crate::dbt::wasm) fn guard_straight_paged_prefix(
        &mut self,
        instructions: &[Instruction],
        guard: u32,
        handoff: Option<u32>,
        indexed_read: bool,
        fetch: PagedPrefixFetch,
        keep_quiet: bool,
    ) {
        use crate::dbt::wasm::SHADOW;

        let first = &instructions[0];
        let last = instructions.last().unwrap();
        self.guard_straight_prefix(first, last, instructions.len() as u32);
        let retained = self
            .quiet_fetch_page
            .filter(|_| keep_quiet && !indexed_read && fetch == PagedPrefixFetch::Retained);
        if let Some(page) = retained {
            self.op(Op::LocalGet(page))
                .op(Op::I64Const(((first.pc & !4095) | 1) as i64))
                .op(Op::I64Eq)
                .op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                .op(Op::I32Const(1))
                .op(Op::Else);
        }
        self.clear_quiet_fetch();
        if let Some(local) = handoff {
            let expected = self.current_index as i32 + 1;
            self.op(Op::LocalGet(local))
                .op(Op::I32Const(expected))
                .op(Op::I32Eq)
                .op(Op::I32Const(0))
                .op(Op::LocalSet(local))
                .op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)))
                .op(Op::I32Const(1))
                .op(Op::Else);
        }
        let newest = fetch != PagedPrefixFetch::Checked
            && (self.sampled_region || self.retouch_prefix_fetch)
            && self.context.is_long64()
            && self.context.cpl() == 0
            && last.next_pc > first.pc
            && (last.next_pc - 1) & !4095 == first.pc & !4095;
        if newest {
            self.newest_prefix_fetch(
                first.pc,
                indexed_read,
                fetch == PagedPrefixFetch::RetainedSse,
            );
            self.op(Op::If(BlockType::Result(wasm_encoder::ValType::I32)));
            if keep_quiet {
                self.remember_quiet_fetch(first.pc);
            }
            self.op(Op::I32Const(1)).op(Op::Else);
        }
        self.op(Op::I64Const(first.pc as i64))
            .op(Op::I32Const((last.next_pc - first.pc) as i32));
        self.system_bits();
        self.op(Op::LocalGet(SHADOW));
        if indexed_read {
            self.op(Op::I32Const(
                crate::embedding::layout::paging::INDEXED_PAGED_READ_GUARD as i32,
            ))
            .op(Op::I32Or);
        }
        if newest {
            self.op(Op::I32Const(
                crate::embedding::layout::paging::NEWEST_FETCH_GUARD as i32,
            ))
            .op(Op::I32Or);
        }
        self.op(Op::Call(guard));
        if newest {
            self.op(Op::End);
        }
        if handoff.is_some() {
            self.op(Op::End);
        }
        if retained.is_some() {
            self.op(Op::End);
        }
        self.op(Op::I32Const(1))
            .op(Op::I32Eq)
            .op(Op::If(BlockType::Empty));
    }
}
