fn main() {
    println!("cargo::rustc-check-cfg=cfg(guest_memory64)");
    println!("cargo::rustc-check-cfg=cfg(parallel_memory64)");
    if std::env::var_os("CARGO_FEATURE_MEMORY64").is_some()
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32")
    {
        if std::env::var_os("CARGO_FEATURE_PARALLEL").is_some() {
            println!("cargo::rustc-cfg=parallel_memory64");
        } else {
            println!("cargo::rustc-cfg=guest_memory64");
        }
    }
}
