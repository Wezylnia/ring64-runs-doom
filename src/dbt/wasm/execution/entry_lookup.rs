use std::collections::BTreeMap;
use wasm_encoder::{BlockType, Function, Instruction as Op, ValType};

pub(in crate::dbt::wasm) fn emit(entries: &BTreeMap<u64, u32>, dense: bool) -> Function {
    let entries: Vec<_> = entries.iter().map(|(&pc, &index)| (pc, index)).collect();
    if dense
        && let [first, .., last] = entries.as_slice()
        && last.0 - first.0 < (entries.len() as u64 * 6).min(1024)
    {
        return lookup_table(&entries, first.0, last.0 - first.0 + 1);
    }
    let mut body = Function::new([]);
    lookup_tree(&mut body, &entries);
    body.instruction(&Op::End);
    body
}

fn lookup_table(entries: &[(u64, u32)], base: u64, span: u64) -> Function {
    let out = entries.len() as u32;
    let mut labels = vec![out; span as usize];
    for (slot, &(pc, _)) in entries.iter().enumerate() {
        labels[(pc - base) as usize] = slot as u32;
    }
    let mut body = Function::new([(1, ValType::I64)]);
    body.instruction(&Op::Block(BlockType::Empty));
    body.instruction(&Op::LocalGet(0));
    body.instruction(&Op::I64Const(base as i64));
    body.instruction(&Op::I64Sub);
    body.instruction(&Op::LocalTee(1));
    body.instruction(&Op::I64Const(span as i64));
    body.instruction(&Op::I64GeU);
    body.instruction(&Op::BrIf(0));
    for _ in entries {
        body.instruction(&Op::Block(BlockType::Empty));
    }
    body.instruction(&Op::LocalGet(1));
    body.instruction(&Op::I32WrapI64);
    body.instruction(&Op::BrTable(labels.into(), out));
    for &(_, index) in entries {
        body.instruction(&Op::End);
        body.instruction(&Op::I32Const(index as i32));
        body.instruction(&Op::Return);
    }
    body.instruction(&Op::End);
    body.instruction(&Op::I32Const(-1));
    body.instruction(&Op::End);
    body
}

fn lookup_tree(body: &mut Function, entries: &[(u64, u32)]) {
    if entries.is_empty() {
        body.instruction(&Op::I32Const(-1));
        return;
    }
    let middle = entries.len() / 2;
    let (pc, index) = entries[middle];
    body.instruction(&Op::LocalGet(0));
    body.instruction(&Op::I64Const(pc as i64));
    body.instruction(&Op::I64Eq);
    body.instruction(&Op::If(BlockType::Result(ValType::I32)));
    body.instruction(&Op::I32Const(index as i32));
    body.instruction(&Op::Else);
    if entries.len() == 1 {
        body.instruction(&Op::I32Const(-1));
    } else {
        body.instruction(&Op::LocalGet(0));
        body.instruction(&Op::I64Const(pc as i64));
        body.instruction(&Op::I64LtU);
        body.instruction(&Op::If(BlockType::Result(ValType::I32)));
        lookup_tree(body, &entries[..middle]);
        body.instruction(&Op::Else);
        lookup_tree(body, &entries[middle + 1..]);
        body.instruction(&Op::End);
    }
    body.instruction(&Op::End);
}
