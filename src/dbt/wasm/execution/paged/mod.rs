#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) mod copy;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) mod indexed_write;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) mod prefix;
