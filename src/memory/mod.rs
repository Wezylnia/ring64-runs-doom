mod access;
mod address;
mod image;
mod validity;

pub(crate) use access::{PreparedPhysicalUpdate, PreparedPhysicalWrite};
pub(crate) use address::check_span;

pub use address::{Access, Cause, Fault, PHYSICAL_ADDRESS_END, canonical48};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) use image::ReadSpan;
pub use image::{Image, MappingInput, MemoryError, MemoryLimits, Permissions};

#[cfg(test)]
mod field_tests;

#[cfg(test)]
mod mapped_tests;

#[cfg(test)]
mod physical_tests;

#[cfg(test)]
mod dependency_tests;

#[cfg(test)]
mod vector_tests;

#[cfg(test)]
mod vector_read_tests;

#[cfg(any(all(feature = "parallel", target_arch = "wasm32"), test))]
mod parallel;

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
pub(crate) use parallel::CAS_ATTEMPTS as SHARED_CAS_ATTEMPTS;

#[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
pub(crate) use parallel::SharedExclusive;
