pub const PKRS_MSR: u32 = 0x6e1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct State {
    pub pkru: u32,
    pub pkrs: u32,
}

impl State {
    pub const ZERO: Self = Self { pkru: 0, pkrs: 0 };
}
