mod packets;
mod paired_conversions;
mod reference;
mod scalar;
mod sse3;
#[cfg(target_arch = "x86_64")]
mod sse3_native;
