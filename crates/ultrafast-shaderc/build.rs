// Bakes a Slang location into the binary when SLANG_DIR is set at build
// time (developer builds). Releases bundle Slang beside the executable
// instead, and without either the pinned release is fetched on first use;
// ULTRAFAST_SLANG_DIR overrides all of them at runtime.
fn main() {
    println!("cargo:rerun-if-env-changed=SLANG_DIR");
    let default = std::env::var("SLANG_DIR").unwrap_or_default();
    println!("cargo:rustc-env=ULTRAFAST_DEFAULT_SLANG_DIR={default}");
}
