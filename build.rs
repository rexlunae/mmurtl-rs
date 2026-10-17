//! Build script: selects the linker script for the arm64 port.
//!
//! amd64 retains its default layout with a small KCFI trap-table fragment.
fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if arch == "aarch64" {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg=-T{dir}/src/arch/arm64/linker.ld");
        println!("cargo:rerun-if-changed=src/arch/arm64/linker.ld");
    }
    if arch == "x86_64" {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg=-T{dir}/src/arch/amd64/cfi.ld");
        println!("cargo:rerun-if-changed=src/arch/amd64/cfi.ld");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
