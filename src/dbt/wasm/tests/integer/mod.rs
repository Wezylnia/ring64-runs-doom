mod arithmetic_bits;
mod bits;
mod bmi;
#[cfg(target_arch = "x86_64")]
mod bmi_native;
mod counts;
mod data_widths;
mod exchange;
#[cfg(target_arch = "x86_64")]
mod exchange_native;
mod integer_memory;
mod logical;
mod multiply_divide;
