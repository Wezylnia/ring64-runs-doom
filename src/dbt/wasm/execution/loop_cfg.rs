use crate::memory::canonical48;
use crate::x86::context::ExecutionContext;
use crate::x86::instruction::{
    Alu, Condition, Count, Instruction, Location, Operation, Shift, Unary, Width,
};

#[derive(Clone, Copy)]
pub(super) struct LoopOrder {
    pub(super) earlier: u32,
    pub(super) head: u32,
    pub(super) tail: u32,
}

impl LoopOrder {
    pub(super) fn len(self) -> u32 {
        self.tail - self.earlier + 1
    }

    pub(super) fn at(self, position: u32) -> u32 {
        let suffix_len = self.tail - self.head + 1;
        if position < suffix_len {
            self.head + position
        } else {
            self.earlier + (position - suffix_len)
        }
    }

    pub(super) fn position(self, ordinal: u32) -> Option<u32> {
        if !(self.earlier..=self.tail).contains(&ordinal) {
            return None;
        }
        Some(if ordinal >= self.head {
            ordinal - self.head
        } else {
            self.tail - self.head + 1 + (ordinal - self.earlier)
        })
    }
}

pub(super) fn select(instructions: &[Instruction]) -> Option<(LoopOrder, u32)> {
    let limit = instructions.len();
    select_layout(instructions, false, limit).or_else(|| select_layout(instructions, true, limit))
}

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
pub(super) fn select_shared_rotated_reads(
    instructions: &[Instruction],
) -> Option<(LoopOrder, u32)> {
    // Shared compilation holds an exclusive grant. Bound the source window
    // before scanning branches or proving any candidate's execution paths.
    let (order, bound) = select_layout(instructions, true, 64)?;
    instructions[order.earlier as usize..=order.tail as usize]
        .iter()
        .all(|instruction| !matches!(instruction.operation, Operation::Store { .. }))
        .then_some((order, bound))
}

fn select_layout(
    instructions: &[Instruction],
    rotate: bool,
    max_body_len: usize,
) -> Option<(LoopOrder, u32)> {
    let mut best = None::<(LoopOrder, u32)>;
    for (tail, instruction) in instructions.iter().enumerate() {
        let Some(target) = branch_target(instruction.operation) else {
            continue;
        };
        let first = (tail + 1).saturating_sub(max_body_len);
        let Ok(head) = instructions[first..tail].binary_search_by_key(&target, |i| i.pc) else {
            continue;
        };
        let head = first + head;
        let mut consider = |earlier| {
            let order = LoopOrder {
                earlier: earlier as u32,
                head: head as u32,
                tail: tail as u32,
            };
            if let Some(bound) = iteration_bound(instructions, order)
                && best.is_none_or(|(_, previous)| bound > previous)
            {
                best = Some((order, bound));
            }
        };
        if rotate {
            let mut prefix_targets = instructions[head..tail]
                .iter()
                .filter_map(|i| branch_target(i.operation))
                .filter_map(|target| {
                    instructions[first..head]
                        .binary_search_by_key(&target, |i| i.pc)
                        .ok()
                        .map(|offset| first + offset)
                });
            if let Some(first) = prefix_targets.next() {
                // Try the widest and narrowest prefixes, with at most two
                // DAG proofs for each backedge.
                let (earliest, latest) = prefix_targets
                    .fold((first, first), |(earliest, latest), ordinal| {
                        (earliest.min(ordinal), latest.max(ordinal))
                    });
                consider(earliest);
                if latest != earliest {
                    consider(latest);
                }
            }
        } else {
            consider(head);
        }
    }
    best
}

fn branch_target(operation: Operation) -> Option<u64> {
    match operation {
        Operation::Jump { target } | Operation::Jcc { target, .. } => Some(target),
        _ => None,
    }
}

pub(super) fn read_prefix_len(instructions: &[Instruction], order: LoopOrder) -> u32 {
    let at = |position| instructions[order.at(position) as usize];
    for position in 0..order.len() {
        let instruction = at(position);
        match instruction.operation {
            Operation::Alu { .. }
            | Operation::Lea { .. }
            | Operation::Mov { .. }
            | Operation::Nop
            | Operation::Shift {
                dst: Location::Register(_),
                ..
            } if position + 1 < order.len() && instruction.next_pc == at(position + 1).pc => {}
            // Only the head can enter the straight prefix. Its comparisons
            // perform one ordinary read before changing architectural state.
            Operation::MemoryAlu {
                kind: Alu::Cmp | Alu::Test,
                ..
            } if position >= 2
                && !instructions[order.earlier as usize..=order.tail as usize]
                    .iter()
                    .filter_map(|instruction| branch_target(instruction.operation))
                    .any(|target| at(0).pc < target && target <= instruction.pc) =>
            {
                return position;
            }
            _ => return 0,
        }
    }
    0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConditionSource {
    Operands(Width),
    OperandsWithTakenFlags(Width),
    ResultSign(Width),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum CompareAction {
    #[default]
    Ordinary,
    SkipStatus,
    Condition(ConditionSource),
    RestoreOnLoadFault(ReadFaultFlags),
    SkipStatusAndRestoreOnLoadFault(ReadFaultFlags),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReadFaultFlags {
    Arithmetic(Alu, Width),
    Shift(Shift, Width),
}

pub(super) fn compare_predicates(
    instructions: &[Instruction],
    order: LoopOrder,
    context: &ExecutionContext,
) -> Vec<CompareAction> {
    let source = &instructions[order.earlier as usize..=order.tail as usize];
    let position_at = |pc| {
        let offset = source.binary_search_by_key(&pc, |i| i.pc).ok()?;
        Some(order.position(order.earlier + offset as u32)? as usize)
    };
    let len = order.len() as usize;
    let mut explicit_entry = vec![false; len];
    for position in source
        .iter()
        .filter_map(|i| branch_target(i.operation))
        .filter_map(position_at)
    {
        explicit_entry[position] = true;
    }

    // Any fault, FLAGS read, or edge out of this lap keeps preceding flags live.
    let mut replaces = vec![false; len];
    let mut predicates = vec![CompareAction::Ordinary; len];
    for position in (1..len).rev() {
        let index = order.at(position as u32) as usize;
        let instruction = instructions[index];
        let next_overwrites = |pc| {
            position_at(pc).is_some_and(|successor| successor > position && replaces[successor])
        };
        if let Operation::Cmov {
            condition,
            src: Location::Register(_),
            ..
        } = instruction.operation
        {
            let previous_position = position - 1;
            let previous = instructions[order.at(previous_position as u32) as usize];
            // A unique scalar load keeps the arithmetic locals live. Restore
            // their status only on its fault path, before leaving the lap.
            let across_load = position > 1
                && !explicit_entry[previous_position]
                && previous.next_pc == instruction.pc
                && matches!(
                    previous.operation,
                    Operation::Load { .. }
                        | Operation::Extend {
                            src: Location::Memory(_),
                            ..
                        }
                );
            let producer_position = previous_position - usize::from(across_load);
            let producer = instructions[order.at(producer_position as u32) as usize];
            let consumer_pc = if across_load {
                previous.pc
            } else {
                instruction.pc
            };
            if !explicit_entry[position]
                && next_overwrites(instruction.next_pc)
                && producer.next_pc == consumer_pc
            {
                let source = match producer.operation {
                    Operation::Alu {
                        kind: kind @ (Alu::Sub | Alu::Cmp),
                        width: width @ (Width::W32 | Width::W64),
                        ..
                    } if matches!(condition, Condition::Less | Condition::GreaterOrEqual) => {
                        Some((ConditionSource::Operands(width), kind, width))
                    }
                    Operation::Unary {
                        kind: Unary::Neg,
                        dst: Location::Register(_),
                        width: width @ (Width::W32 | Width::W64),
                    }
                    | Operation::Alu {
                        kind: Alu::Sub,
                        width: width @ (Width::W32 | Width::W64),
                        ..
                    } if matches!(condition, Condition::Sign | Condition::NotSign) => {
                        Some((ConditionSource::ResultSign(width), Alu::Sub, width))
                    }
                    _ => None,
                };
                if let Some((source, kind, width)) = source {
                    predicates[producer_position] = CompareAction::SkipStatus;
                    if across_load {
                        predicates[previous_position] = CompareAction::RestoreOnLoadFault(
                            ReadFaultFlags::Arithmetic(kind, width),
                        );
                    }
                    predicates[position] = CompareAction::Condition(source);
                }
            }
        }
        let Operation::Jcc { condition, target } = instruction.operation else {
            if let Some((producer, flags)) =
                shift_before_read(instructions, order, position, &explicit_entry)
            {
                predicates[producer] = CompareAction::SkipStatus;
                predicates[position] = if predicates[position] == CompareAction::SkipStatus {
                    CompareAction::SkipStatusAndRestoreOnLoadFault(flags)
                } else {
                    CompareAction::RestoreOnLoadFault(flags)
                };
            }
            replaces[position] = match instruction.operation {
                Operation::Alu { kind, .. } => !matches!(kind, Alu::Adc | Alu::Sbb),
                Operation::Mov { .. }
                | Operation::Lea { .. }
                | Operation::Extend {
                    src: Location::Register(_),
                    ..
                }
                | Operation::Nop => next_overwrites(instruction.next_pc),
                _ => false,
            };
            continue;
        };
        if explicit_entry[position]
            || !matches!(
                condition,
                Condition::Below
                    | Condition::AboveOrEqual
                    | Condition::Equal
                    | Condition::NotEqual
                    | Condition::BelowOrEqual
                    | Condition::Above
            )
            || !(if context.is_long64() {
                canonical48(target)
            } else {
                target <= context.code_limit()
            })
        {
            continue;
        }
        let producer = instructions[order.at(position as u32 - 1) as usize];
        if producer.next_pc != instruction.pc {
            continue;
        }
        let width = match producer.operation {
            Operation::Alu {
                kind: Alu::Cmp,
                width: width @ (Width::W32 | Width::W64),
                ..
            }
            | Operation::MemoryAlu {
                kind: Alu::Cmp,
                width: width @ (Width::W32 | Width::W64),
                ..
            } => width,
            _ => continue,
        };

        let head = instructions[order.head as usize].pc;
        // Keeping both forms of the comparison slowed rotated loops.
        if order.earlier == order.head
            && matches!(producer.operation, Operation::Alu { .. })
            && (target == head || instruction.next_pc == head)
        {
            // The backedge can use the operands even when later instructions
            // or loop exits still need the full comparison flags.
            predicates[position] = CompareAction::Condition(ConditionSource::Operands(width));
        }
        let fallthrough_overwrites = next_overwrites(instruction.next_pc);
        let restore_taken = !next_overwrites(target);
        if restore_taken && (position_at(target).is_some() || !fallthrough_overwrites) {
            continue;
        }
        if !fallthrough_overwrites {
            let Some(load_position) = position_at(instruction.next_pc)
                .filter(|&i| i == position + 1 && !explicit_entry[i])
            else {
                continue;
            };
            let load = instructions[order.at(load_position as u32) as usize];
            if !matches!(load.operation, Operation::Load { .. })
                || !position_at(load.next_pc).is_some_and(|i| i > load_position && replaces[i])
            {
                continue;
            }
            // This load owns its fallthrough entry, and the load path leaves
            // the compare operands in LHS and RHS until any access fault.
            predicates[load_position] =
                CompareAction::RestoreOnLoadFault(ReadFaultFlags::Arithmetic(Alu::Cmp, width));
        }
        predicates[position - 1] = CompareAction::SkipStatus;
        predicates[position] = CompareAction::Condition(if restore_taken {
            // The fallthrough replaces flags before exposing them. The
            // external edge publishes the actual CMP state before routing.
            ConditionSource::OperandsWithTakenFlags(width)
        } else {
            ConditionSource::Operands(width)
        });
    }
    predicates
}

fn shift_before_read(
    instructions: &[Instruction],
    order: LoopOrder,
    position: usize,
    explicit_entry: &[bool],
) -> Option<(usize, ReadFaultFlags)> {
    let at = |i| instructions[order.at(i as u32) as usize];
    let mut next = at(position);
    if explicit_entry[position]
        || !matches!(
            next.operation,
            Operation::MemoryAlu {
                kind: Alu::Cmp | Alu::Test,
                ..
            }
        )
    {
        return None;
    }
    let mut producer = position - 1;
    if matches!(at(producer).operation, Operation::Lea { .. }) {
        // LEA and the comparison's memory helpers leave the shift's input,
        // count and result in their locals through a possible read fault.
        if explicit_entry[producer] || at(producer).next_pc != next.pc {
            return None;
        }
        next = at(producer);
        producer = producer.checked_sub(1)?;
    }
    let shift = at(producer);
    let Operation::Shift {
        kind: kind @ (Shift::Shl | Shift::Shr | Shift::Sar),
        dst: Location::Register(_),
        count: Count::Immediate(count),
        width: width @ (Width::W32 | Width::W64),
    } = shift.operation
    else {
        return None;
    };
    let mask = if width == Width::W64 { 63 } else { 31 };
    (shift.next_pc == next.pc && count & mask != 0)
        .then_some((producer, ReadFaultFlags::Shift(kind, width)))
}

fn iteration_bound(instructions: &[Instruction], order: LoopOrder) -> Option<u32> {
    let source = &instructions[order.earlier as usize..=order.tail as usize];
    let first_pc = source.first()?.pc;
    let last_pc = source.last()?.pc;
    let rotated = order.earlier != order.head;
    if source
        .windows(2)
        .any(|pair| pair[0].next_pc != pair[1].pc || pair[0].pc >= pair[1].pc)
        || rotated && !source.iter().any(|i| reads_memory(i.operation))
    {
        return None;
    }
    // Treat a branch to the head or outside the slice as the end of this lap.
    // All other edges move forward and can use already computed path lengths.
    let mut costs = vec![(0, false, false); order.len() as usize];
    let mut forward_branch = false;
    for position in (0..order.len()).rev() {
        let ordinal = order.at(position);
        let instruction = instructions[ordinal as usize];
        let (taken, fallthrough) = match instruction.operation {
            Operation::Jump { target } => (Some(target), None),
            Operation::Jcc { target, .. } => (Some(target), Some(instruction.next_pc)),
            operation if inside_body(operation, rotated) => (None, Some(instruction.next_pc)),
            _ => return None,
        };
        let mut cost = 0;
        let mut reaches_head = false;
        let mut head_via_prefix = false;
        for target in taken.into_iter().chain(fallthrough) {
            if !(first_pc..=last_pc).contains(&target) {
                continue;
            }
            let index = source.binary_search_by_key(&target, |i| i.pc).ok()?;
            let target_position = order.position(order.earlier + index as u32)?;
            if target_position == 0 {
                reaches_head = true;
            } else if target_position > position {
                forward_branch |= taken == Some(target) && target_position > position + 1;
                let (next_cost, next_head, next_via_prefix) = costs[target_position as usize];
                cost = cost.max(next_cost);
                reaches_head |= next_head;
                head_via_prefix |= next_via_prefix;
            } else {
                return None;
            }
        }
        head_via_prefix |= ordinal < order.head && reaches_head;
        costs[position as usize] = (cost + 1, reaches_head, head_via_prefix);
    }
    let (cost, repeats, head_via_prefix) = costs[0];
    (repeats
        && if rotated {
            head_via_prefix
        } else {
            forward_branch
        })
    .then_some(cost)
}

fn reads_memory(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::Load { .. }
            | Operation::MemoryAlu {
                kind: Alu::Cmp | Alu::Test,
                ..
            }
            | Operation::Extend {
                src: Location::Memory(_),
                ..
            }
    )
}

fn inside_body(operation: Operation, rotated: bool) -> bool {
    matches!(
        operation,
        Operation::Alu { .. }
            | Operation::Cmov { .. }
            | Operation::Extend { .. }
            | Operation::Lea { .. }
            | Operation::Load { .. }
            | Operation::MemoryAlu {
                kind: Alu::Cmp | Alu::Test,
                ..
            }
            | Operation::Mov { .. }
            | Operation::Nop
            | Operation::Store { .. }
            | Operation::Unary {
                dst: Location::Register(_),
                ..
            }
    ) || rotated
        && matches!(
            operation,
            Operation::Shift {
                dst: Location::Register(_),
                ..
            }
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86::instruction::{Address, Condition, RegisterView, Width};

    #[test]
    fn longest_lap_counts_reachable_instructions_and_rejects_inner_cycles() {
        let mut body: Vec<_> = [
            Operation::Jcc {
                condition: Condition::Equal,
                target: 0x100c,
            },
            Operation::Jump { target: 0x1010 },
            Operation::Nop,
            Operation::Jump { target: 0x1014 },
            Operation::Nop,
            Operation::Jump { target: 0x1000 },
        ]
        .into_iter()
        .enumerate()
        .map(|(i, operation)| Instruction {
            pc: 0x1000 + i as u64 * 4,
            next_pc: 0x1004 + i as u64 * 4,
            operation,
        })
        .collect();
        let order = LoopOrder {
            earlier: 0,
            head: 0,
            tail: 5,
        };
        assert_eq!(iteration_bound(&body, order), Some(4));
        body[4].operation = Operation::Jump { target: 0x1004 };
        assert_eq!(iteration_bound(&body, order), None);
    }

    #[test]
    fn rotated_candidates_require_a_path_through_the_prefix_back_to_the_head() {
        let prefix = 32;
        let pc = |ordinal| 0x1000 + ordinal as u64 * 4;
        let jcc = |ordinal| Operation::Jcc {
            condition: Condition::Equal,
            target: pc(ordinal),
        };
        let mut body: Vec<_> = (0..prefix)
            .map(|_| Operation::Nop)
            .chain([Operation::Load {
                dst: RegisterView::low(0),
                address: Address::default(),
                width: Width::W32,
            }])
            .chain((0..prefix).map(jcc))
            .chain((0..prefix).map(|_| jcc(prefix)))
            .enumerate()
            .map(|(ordinal, operation)| Instruction {
                pc: pc(ordinal),
                next_pc: pc(ordinal + 1),
                operation,
            })
            .collect();
        let (order, max_lap) = select_layout(&body, true, body.len()).unwrap();
        assert_eq!((order.earlier, order.head, order.tail), (0, 32, 96));
        assert_eq!(max_lap, 65);

        // The widest prefix would contain a cycle with the wrong head.
        // The narrowest can only leave the region instead of returning.
        body[prefix - 1].operation = Operation::Jump {
            target: pc(prefix - 2),
        };
        assert!(iteration_bound(&body, order).is_none());
        assert!(
            iteration_bound(
                &body,
                LoopOrder {
                    earlier: prefix as u32 - 1,
                    ..order
                },
            )
            .is_none()
        );
    }

    #[test]
    fn rotated_scan_limits_source_window_even_when_lap_is_short() {
        let pc = |ordinal| 0x1000 + ordinal as u64 * 4;
        for length in [64, 65, 66] {
            let tail = length - 1;
            let body: Vec<_> = (0..length)
                .map(|ordinal| Instruction {
                    pc: pc(ordinal),
                    next_pc: pc(ordinal + 1),
                    operation: match ordinal {
                        1 => Operation::Load {
                            dst: RegisterView::low(0),
                            address: Address::default(),
                            width: Width::W32,
                        },
                        2 => Operation::Jcc {
                            condition: Condition::Equal,
                            target: pc(0),
                        },
                        3 => Operation::Jump { target: pc(tail) },
                        n if n == tail => Operation::Jump { target: pc(1) },
                        _ => Operation::Nop,
                    },
                })
                .collect();
            let (order, bound) = select_layout(&body, true, body.len()).unwrap();
            assert_eq!(
                (order.earlier, order.head, order.tail, bound),
                (0, 1, tail as u32, 4)
            );
            assert_eq!(select_layout(&body, true, 64).is_some(), length == 64);
        }
    }
}
