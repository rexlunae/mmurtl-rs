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

    let mut boot_config = bootloader::BootConfig::default();
    boot_config.log_level = bootloader_boot_config::LevelFilter::Warn;
    let mut builder = bootloader::DiskImageBuilder::new(kernel_path);
    builder.set_boot_config(&boot_config);
    if let Some(path) = std::env::var_os("MMURTL_MICROCODE") {
        let path = PathBuf::from(path);
        if !path.is_file() { return Err("MMURTL_MICROCODE must name a vendor container file".into()); }
        println!("  Microcode: {}", path.display());
        builder.set_ramdisk(path);
    }
    builder.create_bios_image(&bios_path)?;
    builder.create_uefi_image(&uefi_path)?;

    let bios_size = std::fs::metadata(&bios_path)?.len();
    let uefi_size = std::fs::metadata(&uefi_path)?.len();
    println!("  Done — BIOS: {:.1} MiB, UEFI: {:.1} MiB",
        bios_size as f64 / (1024.0 * 1024.0),
        uefi_size as f64 / (1024.0 * 1024.0));

    Ok(())
}
