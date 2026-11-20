use crate::x86::instruction::{Alu, Instruction, Location, Operation};

pub(super) fn select(instructions: &[Instruction]) -> Option<(u32, u32, u32)> {
    let mut best = None;
    for (tail, instruction) in instructions.iter().enumerate() {
        let target = match instruction.operation {
            Operation::Jump { target } | Operation::Jcc { target, .. } => target,
            _ => continue,
        };
        let Ok(head) = instructions[..tail].binary_search_by_key(&target, |i| i.pc) else {
            continue;
        };
        if let Some(max_lap_len) = iteration_bound(&instructions[head..=tail])
            && best.is_none_or(|(_, _, bound)| max_lap_len > bound)
        {
            best = Some((head as u32, tail as u32, max_lap_len));
        }
    }
    best
}

fn iteration_bound(instructions: &[Instruction]) -> Option<u32> {
    let head = instructions.first()?.pc;
    let tail = instructions.last()?.pc;
    if instructions
        .windows(2)
        .any(|pair| pair[0].next_pc != pair[1].pc || pair[0].pc >= pair[1].pc)
    {
        return None;
    }
    // Treat a branch to the head or outside the slice as the end of this lap.
    // All other edges move forward and can use already computed path lengths.
    let mut costs = vec![(0, false); instructions.len()];
    let mut forward_branch = false;
    for (index, instruction) in instructions.iter().enumerate().rev() {
        let (taken, fallthrough) = match instruction.operation {
            Operation::Jump { target } => (Some(target), None),
            Operation::Jcc { target, .. } => (Some(target), Some(instruction.next_pc)),
            operation if inside_body(operation) => (None, Some(instruction.next_pc)),
            _ => return None,
        };
        let mut cost = 0;
        let mut reaches_head = false;
        for target in taken.into_iter().chain(fallthrough) {
            if !(head..=tail).contains(&target) {
                continue;
            }
            let ordinal = instructions.binary_search_by_key(&target, |i| i.pc).ok()?;
            if ordinal == 0 {
                reaches_head = true;
            } else if ordinal > index {
                forward_branch |= taken == Some(target) && ordinal > index + 1;
                cost = cost.max(costs[ordinal].0);
                reaches_head |= costs[ordinal].1;
            } else {
                return None;
            }
        }
        costs[index] = (cost + 1, reaches_head);
    }
    let (cost, repeats) = costs[0];
    (forward_branch && repeats).then_some(cost)
}

fn inside_body(operation: Operation) -> bool {
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86::instruction::Condition;

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
        assert_eq!(iteration_bound(&body), Some(4));
        body[4].operation = Operation::Jump { target: 0x1004 };
        assert_eq!(iteration_bound(&body), None);
    }
}
