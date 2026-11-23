// The Wasm helper copies between the private wasm32 heap and guest memory64.
// These addresses never identify bytes in Rust's own address space.
#[link(wasm_import_module = "guest64")]
unsafe extern "C" {
    fn allocate(length: u64) -> u64;
    fn release(base: u64);
    fn read(destination: *mut u8, source: u64, length: u32);
    fn write(destination: u64, source: *const u8, length: u32);
}

#[derive(Debug)]
pub(super) struct Extent {
    base: u64,
    pub(super) length: u64,
}

impl Extent {
    pub(super) fn new(length: u64, initial: &[u8]) -> Result<Self, super::MemoryError> {
        let base = unsafe { allocate(length) };
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
