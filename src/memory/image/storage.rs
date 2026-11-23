use super::Backing;

pub type BackingOffset = u32;

impl Backing {
    #[inline]
    pub(in crate::memory) fn read_array<const N: usize>(&self, offset: BackingOffset) -> [u8; N] {
        let mut bytes = [0; N];
        self.read_bytes(offset, &mut bytes);
        bytes
    }

    #[inline]
    pub(in crate::memory) fn read_bytes(&self, offset: BackingOffset, output: &mut [u8]) {
        let start = offset as usize;
        copy_bytes(output, &self.bytes[start..start + output.len()]);
    }

    #[inline]
    pub(in crate::memory) fn write_bytes(&mut self, offset: BackingOffset, input: &[u8]) {
        let start = offset as usize;
        copy_bytes(&mut self.bytes[start..start + input.len()], input);
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) fn host_pointer(&self, offset: BackingOffset) -> Option<*const u8> {
        Some(self.bytes.as_ptr().wrapping_add(offset as usize))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(super) fn host_pointer_mut(&mut self, offset: BackingOffset) -> Option<*mut u8> {
        Some(self.bytes.as_mut_ptr().wrapping_add(offset as usize))
    }
}

#[inline]
fn copy_bytes(target: &mut [u8], source: &[u8]) {
    match target.len() {
        1 => target[0] = source[0],
        2 => target[..2].copy_from_slice(&source[..2]),
        4 => target[..4].copy_from_slice(&source[..4]),
        8 => target[..8].copy_from_slice(&source[..8]),
        10 => target[..10].copy_from_slice(&source[..10]),
        16 => target[..16].copy_from_slice(&source[..16]),
        _ => target.copy_from_slice(source),
    }
}
