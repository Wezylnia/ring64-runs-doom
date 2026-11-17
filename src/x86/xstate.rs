pub const SUPPORTED: u64 = (1 << 9) | (1 << 1) | 1;
pub const PKRU_OFFSET: usize = 576;
pub const MAX_BYTES: usize = PKRU_OFFSET + 8;
pub const RESET_XCR0: u64 = 1;
pub const XSS_MSR: u32 = 0xda0;

pub fn valid_xcr0(value: u64) -> bool {
    value & !SUPPORTED == 0 && value & 1 != 0
}

pub enum ControlFault {
    Disabled,
    GeneralProtection(super::protection::ProtectionPhase),
}

pub fn access_xcr0(
    xcr0: &mut u64,
    cr4: u64,
    context: &super::context::ExecutionContext,
    index: u32,
    write: Option<u64>,
) -> Result<u64, ControlFault> {
    use super::context::Mode;
    use super::protection::ProtectionPhase;
    if cr4 & super::paging::CR4_OSXSAVE == 0 {
        return Err(ControlFault::Disabled);
    }
    if write.is_some() && context.mode() != Mode::Real && context.cpl() != 0 {
        return Err(ControlFault::GeneralProtection(ProtectionPhase::Privilege));
    }
    if index != 0 || write.is_some_and(|value| !valid_xcr0(value)) {
        return Err(ControlFault::GeneralProtection(ProtectionPhase::Target));
    }
    if let Some(value) = write {
        *xcr0 = value;
    }
    Ok(*xcr0)
}
