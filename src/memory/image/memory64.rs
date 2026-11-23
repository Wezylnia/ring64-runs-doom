// The Wasm helper copies between the private wasm32 heap and guest memory64.
// These addresses never identify bytes in Rust's own address space.
#[link(wasm_import_module = "guest64")]
unsafe extern "C" {
    fn allocate(length: u64, minimum: u32, alignment: u32) -> u64;
    fn release(base: u64);
    fn read(destination: *mut u8, source: u64, length: u32);
    fn write(destination: u64, source: *const u8, length: u32);
}

#[derive(Debug)]
pub(super) struct Extent {
    pub(super) base: u64,
    pub(super) length: u64,
}

impl Extent {
    pub(super) fn new(
        length: u64,
        initial: &[u8],
        alignment: u32,
    ) -> Result<Self, super::MemoryError> {
        // Bounded arenas reserve offset zero for the null token.
        let base = unsafe { allocate(length, alignment, alignment.max(1)) };
        if base == u64::MAX {
            return Err(super::MemoryError::Capacity("memory64 allocation refused"));
        }
        let mut extent = Self { base, length };
        extent.write(0, initial);
        Ok(extent)
    }

    pub(super) fn read(&self, offset: u64, output: &mut [u8]) {
        debug_assert!(offset <= self.length - output.len() as u64);
        unsafe { read(output.as_mut_ptr(), self.base + offset, output.len() as u32) }
    }

    pub(super) fn write(&mut self, offset: u64, input: &[u8]) {
        debug_assert!(offset <= self.length - input.len() as u64);
        unsafe { write(self.base + offset, input.as_ptr(), input.len() as u32) }
    }
}

impl Drop for Extent {
    fn drop(&mut self) {
        unsafe { release(self.base) };
    }
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub(crate) struct GuestSpan {
    pub(crate) base: u64,
    pub(crate) length: u64,
    pub(crate) host: u64,
}

impl super::Image {
    pub(crate) fn guest_span(&self, address: u64, width: u32, write: bool) -> Option<GuestSpan> {
        if self.physical || width == 0 {
            return None;
        }
        let mapping = &self.mappings[self.locate_index(address)?];
        let relative = address - mapping.base;
        if write {
            if !self.direct_mapping_writable(mapping, relative, width) {
                return None;
            }
        } else if !mapping.permissions.read
            || u64::from(width) > u64::from(mapping.length) - relative
        {
            return None;
        }
        Some(GuestSpan {
            base: mapping.base,
            length: u64::from(mapping.length),
            host: self.backings[mapping.backing].extent.base + mapping.offset,
        })
    }
}
