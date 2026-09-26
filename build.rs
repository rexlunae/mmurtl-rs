//! Build script: selects the linker script for the arm64 port.
//!
//! amd64 needs none (the `bootloader` crate supplies the layout; image
//! creation is handled by the Makefile).
fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if arch == "aarch64" {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg=-T{dir}/src/arch/arm64/linker.ld");
        println!("cargo:rerun-if-changed=src/arch/arm64/linker.ld");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
