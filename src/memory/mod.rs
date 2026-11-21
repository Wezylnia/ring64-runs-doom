mod access;
mod address;
mod image;
#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
mod mmio;
mod validity;

pub(crate) use access::{PreparedPhysicalUpdate, PreparedPhysicalWrite};
pub(crate) use address::check_span;

pub use address::{Access, Cause, Fault, PHYSICAL_ADDRESS_END, canonical48};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) use image::DirectSpan;
pub use image::{Image, MappingInput, MemoryError, MemoryLimits, Permissions};
#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) use mmio::{MmioOperand, MmioRange};

#[cfg(any(all(feature = "parallel", target_arch = "wasm32"), test))]
mod parallel;

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
pub(crate) use parallel::{CAS_ATTEMPTS as SHARED_CAS_ATTEMPTS, SharedDword};

#[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
pub(crate) use parallel::SharedExclusive;
