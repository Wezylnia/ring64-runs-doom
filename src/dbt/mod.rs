mod discovery;
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) use discovery::FetchProof;
#[cfg(any(target_arch = "wasm32", test))]
mod ownership;
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) use ownership::Owners;
mod unit;
mod wasm;

use crate::memory::Image;
use crate::x86::context::ExecutionContext;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_instructions: u32,
    pub max_source_bytes: u32,
    pub max_artifact_bytes: u32,
}

pub(crate) struct CompileProfile<'a> {
    pub observed: bool,
    pub paged_vector_copy: bool,
    pub paged_copy_spans: bool,
    pub entries: EntrySelection<'a>,
}

pub(crate) enum EntrySelection<'a> {
    Discover(&'a [u64]),
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    Sampled(&'a [u64]),
}

#[derive(Debug)]
pub struct Artifact {
    pub bytes: Vec<u8>,
    /// The index supplied to `run` must identify the current RIP in this list.
    pub entries: Vec<u64>,
    pub instruction_count: usize,
    pub block_count: usize,
    pub source_bytes: usize,
    pub source_ranges: Vec<SourceRange>,
    pub read_dependencies: Vec<SourceRange>,
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fetch_proof: crate::dbt::FetchProof,
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) instructions: Vec<crate::x86::instruction::Instruction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRange {
    pub start: u64,
    pub length: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompileError {
    Resource,
}

pub fn compile(image: &Image, entry: u64, limits: Limits) -> Result<Artifact, CompileError> {
    compile_with_context(image, &ExecutionContext::default(), entry, limits)
}

pub fn compile_with_context(
    image: &Image,
    context: &ExecutionContext,
    entry: u64,
    limits: Limits,
) -> Result<Artifact, CompileError> {
    compile_with_context_observed(image, context, entry, limits, false)
}

pub fn compile_with_context_observed(
    image: &Image,
    context: &ExecutionContext,
    entry: u64,
    limits: Limits,
    observed: bool,
) -> Result<Artifact, CompileError> {
    compile_with_paging(image, context, None, entry, limits, observed)
}

pub fn compile_with_paging(
    image: &Image,
    context: &ExecutionContext,
    paging: Option<&crate::x86::paging::State>,
    entry: u64,
    limits: Limits,
    observed: bool,
) -> Result<Artifact, CompileError> {
    compile_profile(
        image,
        context,
        paging,
        None,
        entry,
        limits,
        CompileProfile {
            observed,
            paged_vector_copy: false,
            paged_copy_spans: false,
            entries: EntrySelection::Discover(&[]),
        },
    )
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn compile_with_paging_cache(
    image: &Image,
    context: &ExecutionContext,
    paging: Option<&crate::x86::paging::State>,
    cache: &crate::x86::paging::cache::Cache,
    entry: u64,
    limits: Limits,
    profile: CompileProfile<'_>,
) -> Result<Artifact, CompileError> {
    compile_profile(image, context, paging, Some(cache), entry, limits, profile)
}

fn compile_profile(
    image: &Image,
    context: &ExecutionContext,
    paging: Option<&crate::x86::paging::State>,
    cache: Option<&crate::x86::paging::cache::Cache>,
    entry: u64,
    limits: Limits,
    profile: CompileProfile<'_>,
) -> Result<Artifact, CompileError> {
    let CompileProfile {
        observed,
        paged_vector_copy: _paged_vector_copy,
        paged_copy_spans: _paged_copy_spans,
        entries,
    } = profile;
    let unit = discovery::discover_with_entries(
        image,
        context,
        paging,
        cache,
        entry,
        limits,
        (entries, observed),
    )?;
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    let emit = if image.has_mmio() {
        wasm::emit_mmio_profile
    } else {
        wasm::emit_profile
    };
    #[cfg(not(all(not(feature = "parallel"), any(target_arch = "wasm32", test))))]
    let emit = wasm::emit_profile;
    let max_artifact_bytes = limits.max_artifact_bytes as usize;
    let checked = || emit(&unit, observed, paging.is_some(), max_artifact_bytes);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let bytes = if _paged_vector_copy && paging.is_some() && !image.has_mmio() {
        wasm::emit_paged_vector_copy(&unit, max_artifact_bytes, _paged_copy_spans)
            .unwrap_or_else(checked)
    } else {
        checked()
    };
    #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
    let bytes = checked();
    if bytes.len() > limits.max_artifact_bytes as usize {
        return Err(CompileError::Resource);
    }
    Ok(Artifact {
        entries: unit
            .instructions
            .iter()
            .map(|instruction| instruction.pc)
            .collect(),
        instruction_count: unit.instructions.len(),
        block_count: unit.block_count,
        source_bytes: unit.source_bytes,
        source_ranges: unit.source_ranges,
        read_dependencies: unit.read_dependencies,
        #[cfg(any(target_arch = "wasm32", test))]
        fetch_proof: unit.fetch_proof,
        #[cfg(any(target_arch = "wasm32", test))]
        instructions: unit.instructions,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_artifact_limit_is_checked_before_returning_an_artifact() {
        let image = Image::new(0x1000, vec![0xb8, 1, 0, 0, 0], 0x8000, vec![0; 8]).unwrap();
        let limits = Limits {
            max_instructions: 8,
            max_source_bytes: 32,
            max_artifact_bytes: 1,
        };
        assert!(matches!(
            compile(&image, 0x1000, limits),
            Err(CompileError::Resource)
        ));
        let artifact = compile(
            &image,
            0x1000,
            Limits {
                max_artifact_bytes: 65536,
                ..limits
            },
        )
        .unwrap();
        assert_eq!(artifact.entries, [0x1000]);
        assert_eq!(
            (
                artifact.instruction_count,
                artifact.block_count,
                artifact.source_bytes
            ),
            (1, 1, 5)
        );
        wasmparser::Validator::new()
            .validate_all(&artifact.bytes)
            .unwrap();
    }
}
