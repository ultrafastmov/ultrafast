// Picks the GPU backend: Metal on macOS, Vulkan elsewhere, or Vulkan on
// macOS (through MoltenVK) with the `vulkan` feature.
fn main() {
    println!("cargo:rustc-check-cfg=cfg(backend_vulkan)");
    println!("cargo:rustc-check-cfg=cfg(backend_metal)");
    let macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let vulkan = std::env::var_os("CARGO_FEATURE_VULKAN").is_some() || !macos;
    println!(
        "cargo:rustc-cfg={}",
        if vulkan { "backend_vulkan" } else { "backend_metal" }
    );
}
