use crate::x86::paging::walk::Translation;

const PAGE_MASK: u64 = !0xfff;

// Every supported paging leaf covers at least 4 KiB. Keep the walk whose
// entry bytes the compiler registered, including their order in the walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FetchPage {
    pub linear: u64,
    physical: u64,
    entries: [u64; 4],
    entry_count: u8,
    pub entry_width: u32,
}

impl FetchPage {
    pub fn record(pages: &mut Vec<Self>, linear: u64, translated: &Translation) {
        let linear = linear & PAGE_MASK;
        if translated.entry_width == 0 || pages.last().is_some_and(|p| p.linear == linear) {
            return;
        }
        let path = translated.entries();
        let mut entries = [0; 4];
        entries[..path.len()].copy_from_slice(path);
        pages.push(Self {
            linear,
            physical: translated.physical & PAGE_MASK,
            entries,
            entry_count: path.len() as u8,
            entry_width: translated.entry_width,
        });
    }

    pub fn entries(&self) -> &[u64] {
        &self.entries[..usize::from(self.entry_count)]
    }
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Default)]
pub(crate) struct FetchProof {
    pub(super) pages: Box<[FetchPage]>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl FetchProof {
    #[cfg(not(feature = "parallel"))]
    pub(crate) fn physical_span(&self, linear: u64, length: u32) -> Option<u64> {
        let offset = linear & !PAGE_MASK;
        if offset + u64::from(length) > 0x1000 {
            return None;
        }
        let index = self
            .pages
            .binary_search_by_key(&(linear & PAGE_MASK), |page| page.linear)
            .ok()?;
        Some(self.pages[index].physical + offset)
    }

    /// The caller separately verifies the full execution context, root and
    /// physical source owner. A speculative check must never perform a fetch.
    pub(crate) fn matches(
        &self,
        image: &crate::memory::Image,
        state: &crate::x86::paging::State,
        cache: &crate::x86::paging::cache::Cache,
        cpl: u8,
    ) -> bool {
        use crate::memory::Access;
        use crate::x86::{
            paging::walk::{AccessClass, Request},
            protection_keys,
        };

        state.cr0 & (1 << 31) != 0
            && !self.pages.is_empty()
            && self.pages.iter().all(|page| {
                let request = Request {
                    linear: page.linear,
                    access: Access::Execute,
                    class: AccessClass::operand(cpl, false),
                };
                cache
                    .peek(image, state, request, &protection_keys::State::ZERO)
                    .is_ok_and(|current| {
                        current.physical == page.physical
                            && current.entry_width == page.entry_width
                            && current.entries() == page.entries()
                    })
            })
    }
}

#[cfg(test)]
mod tests;
