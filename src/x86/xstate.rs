pub const SUPPORTED: u64 = (1 << 9) | (1 << 1) | 1;
pub const PKRU_OFFSET: usize = 576;
pub const MAX_BYTES: usize = PKRU_OFFSET + 8;
pub const RESET_XCR0: u64 = 1;
pub const XSS_MSR: u32 = 0xda0;

pub fn valid_xcr0(value: u64) -> bool {
    value & !SUPPORTED == 0 && value & 1 != 0
}
