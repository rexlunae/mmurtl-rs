/// MMURTL/RS Boot Image Builder
///
/// Creates BIOS + UEFI bootable disk images from the compiled kernel ELF.
/// Usage: image-builder <kernel-elf> <bios-img-out> <uefi-img-out>

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: {} <kernel-elf> <bios-img> <uefi-img>", args[0]);
        std::process::exit(1);
    }

    let kernel_path = PathBuf::from(&args[1]);
    let bios_path = PathBuf::from(&args[2]);
    let uefi_path = PathBuf::from(&args[3]);

    if !kernel_path.exists() {
        eprintln!("Error: kernel not found at {}", kernel_path.display());
        std::process::exit(1);
    }

    println!("Creating boot images...");
    println!("  Kernel: {}", kernel_path.display());
    println!("  BIOS:   {}", bios_path.display());
    println!("  UEFI:   {}", uefi_path.display());

    let builder = bootloader::DiskImageBuilder::new(kernel_path);
    builder.create_boot_image(&bios_path, &uefi_path)?;

    let bios_size = std::fs::metadata(&bios_path)?.len();
    let uefi_size = std::fs::metadata(&uefi_path)?.len();
    println!("  Done — BIOS: {:.1} MiB, UEFI: {:.1} MiB",
        bios_size as f64 / (1024.0 * 1024.0),
        uefi_size as f64 / (1024.0 * 1024.0));

    Ok(())
}
