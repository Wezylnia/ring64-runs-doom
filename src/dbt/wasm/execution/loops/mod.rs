#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) mod float_dyadic;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) mod float_loop;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(in crate::dbt::wasm) mod linear_recurrence;
pub(in crate::dbt::wasm) mod loop_cfg;
pub(in crate::dbt::wasm) mod structured_loop;
