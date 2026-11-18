pub mod access;
pub mod cache;
mod controls;
pub mod walk;

pub use controls::{
    CR4_FSGSBASE, CR4_OSXSAVE, CR4_PCIDE, CR4_PGE, CR4_PKE, CR4_PKS, CR4_SMAP, CR4_SMEP, CR4_TSD,
    ControlFailure, EFER_MSR, PdpteFailure, State,
};

#[cfg(test)]
mod key_tests;
#[cfg(test)]
mod transition_tests;
