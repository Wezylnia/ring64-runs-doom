#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) mod data;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) mod stack;
