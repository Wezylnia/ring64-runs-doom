use super::super::mmx;
use crate::x86::instruction::{
    Instruction, Operand, Operation, PagingInstruction, Repeat, StringKind, Width,
};

#[derive(Default)]
pub(super) struct OperationUses {
    pub(super) repeat: bool,
    pub(super) dynamic_targets: bool,
    pub(super) wide_product: bool,
    pub(super) wide_divide: bool,
    pub(super) wide_compare: bool,
    pub(super) float: bool,
    pub(super) load_mxcsr: bool,
    pub(super) store_mxcsr: bool,
    pub(super) x87: bool,
    pub(super) mmx: bool,
    pub(super) mmx_vector: bool,
    pub(super) numeric_image: bool,
    pub(super) masked_store: bool,
    pub(super) clflush: bool,
    pub(super) protection: bool,
    pub(super) far: bool,
    pub(super) compound_stack: bool,
    pub(super) debug_register: bool,
    pub(super) paging: bool,
    pub(super) invpcid: bool,
    pub(super) fast_system: bool,
    pub(super) processor: bool,
    pub(super) protection_key: bool,
    pub(super) system_flags: bool,
    pub(super) software_events: bool,
    pub(super) halt: bool,
    pub(super) write_segment_base: bool,
    pub(super) io: bool,
}

impl OperationUses {
    pub(super) fn for_instructions(instructions: &[Instruction]) -> Self {
        let mut uses = Self::default();
        for instruction in instructions {
            match instruction.operation {
                Operation::String { kind, repeat, .. } => {
                    uses.repeat |= repeat != Repeat::None;
                    uses.io |= matches!(kind, StringKind::Ins | StringKind::Outs);
                }
                Operation::Return { .. }
                | Operation::JumpRegister { .. }
                | Operation::JumpMemory { .. }
                | Operation::Call {
                    target: Operand::Register(_) | Operand::Memory(_),
                    ..
                } => uses.dynamic_targets = true,
                Operation::Multiply {
                    width: Width::W64, ..
                }
                | Operation::Imul {
                    width: Width::W64, ..
                } => uses.wide_product = true,
                Operation::Divide {
                    width: Width::W64, ..
                } => uses.wide_divide = true,
                Operation::CompareExchange16b { .. } => uses.wide_compare = true,
                Operation::Float(_) => uses.float = true,
                Operation::Mxcsr { store: false, .. } => uses.load_mxcsr = true,
                Operation::Mxcsr { store: true, .. } => uses.store_mxcsr = true,
                Operation::X87(_) => uses.x87 = true,
                Operation::Mmx { op, .. } => {
                    uses.mmx |= !mmx::direct(op);
                    uses.mmx_vector |= mmx::vector_temps(op);
                }
                Operation::NumericImage { .. } => uses.numeric_image = true,
                Operation::MaskedStore { .. } => uses.masked_store = true,
                Operation::Clflush { .. } => uses.clflush = true,
                Operation::Protection(_) | Operation::PopSs { .. } => uses.protection = true,
                Operation::Far(_) => uses.far = true,
                Operation::CompoundStack(_) => uses.compound_stack = true,
                Operation::DebugRegister { .. } => uses.debug_register = true,
                Operation::Paging(PagingInstruction::InvalidatePcid { .. }) => uses.invpcid = true,
                Operation::Paging(_) => uses.paging = true,
                Operation::FastSystem(_) => uses.fast_system = true,
                Operation::Processor(_) => uses.processor = true,
                Operation::ProtectionKey(_) => uses.protection_key = true,
                Operation::SystemFlags(_) => uses.system_flags = true,
                Operation::SoftwareEvent(_) => uses.software_events = true,
                Operation::Halt => uses.halt = true,
                Operation::SegmentBase { write: true, .. } => uses.write_segment_base = true,
                Operation::Io { .. } => uses.io = true,
                _ => {}
            }
        }
        uses
    }
}
