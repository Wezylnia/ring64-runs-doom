use std::ops::Range;
use wasm_encoder::{BranchHint, BranchHints, CodeSection, Module, Section};

pub(super) struct EncodedArtifact {
    pub(super) bytes: Vec<u8>,
    hints: Range<usize>,
}

impl EncodedArtifact {
    pub(super) fn new(
        module: Module,
        code: &CodeSection,
        functions: impl Iterator<Item = (u32, Vec<BranchHint>)>,
    ) -> Self {
        let mut bytes = module.finish();
        let start = bytes.len();
        let mut section = BranchHints::new();
        for (function, hints) in functions {
            if !hints.is_empty() {
                section.function_hints(function, hints);
            }
        }
        if !section.is_empty() {
            section.append_to(&mut bytes);
        }
        let hints = start..bytes.len();
        code.append_to(&mut bytes);
        Self { bytes, hints }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn code_fits(&self, limit: usize) -> bool {
        self.bytes.len() - self.hints.len() <= limit
    }

    pub(super) fn discard_hints(&mut self) {
        self.bytes.drain(self.hints.clone());
        self.hints.end = self.hints.start;
    }
}
