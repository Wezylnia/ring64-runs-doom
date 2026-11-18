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
