pub(super) mod boundary;
mod compound_stack;
pub(super) mod control;
pub(super) mod entry_lookup;
mod far;
mod instruction;
pub(super) mod loops;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) mod paged;
mod stack;
pub(super) mod state;
