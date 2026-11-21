mod exceptions;
mod execution;
mod fisttp;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod fpatan_native;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod log_exp_native;
mod reference;
mod special;
