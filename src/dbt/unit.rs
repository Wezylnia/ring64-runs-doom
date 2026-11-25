use super::SourceRange;
use crate::x86::{context::ExecutionContext, instruction::Instruction};

pub(crate) struct Unit {
    pub context: ExecutionContext,
    #[cfg(parallel_memory64)]
    pub shared_ram_memory64: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub sampled: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub reached_instruction_limit: bool,
    pub instructions: Vec<Instruction>,
    pub block_count: usize,
    pub source_bytes: usize,
    pub source_ranges: Vec<SourceRange>,
    pub read_dependencies: Vec<SourceRange>,
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fetch_proof: crate::dbt::FetchProof,
}
