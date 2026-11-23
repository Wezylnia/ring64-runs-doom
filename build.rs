fn main() {
    println!("cargo::rustc-check-cfg=cfg(guest_memory64)");
    if std::env::var_os("CARGO_FEATURE_MEMORY64").is_some()
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32")
        && std::env::var_os("CARGO_FEATURE_PARALLEL").is_none()
    {
        println!("cargo::rustc-cfg=guest_memory64");
    }
}
