//! arm64 boot.
//!
//! The kernel is **relocatable**: it is linked as a position-independent
//! executable (base 0x4020_0000) and runs wherever the loader put it —
//! QEMU `virt` loads it at its link address, a Raspberry Pi's firmware
//! near the start of RAM at 0. `_start` applies its own
//! R_AARCH64_RELATIVE relocations before any code can read an absolute
//! address, then everything runs identity-mapped at the load address.
//!
//! CPU 0 enters `_start` with the MMU off, at EL3, EL2, or EL1 (we drop to
//! EL1). Secondary CPUs are started later, by PSCI `CPU_ON` at
//! `secondary_start` or by a spin-table release at `secondary_spin_entry`
//! (see `smp.rs`).
//!
//! Boot CPU sequence: relocate → device tree (MMU still off) → identity
//! map + MMU on → console → vectors → memory → interrupt controller +
//! timer → scheduler → secondary CPUs → devices → `crate::kernel_run()`.

use core::arch::global_asm;

use super::fdt;

global_asm!(
    r#"
    .equ LINK_BASE, 0x40200000      // must match linker.ld (asserted there)
    .equ R_AARCH64_RELATIVE, 1027

    .section .text.boot, "ax"
    .globl _start
_start:
    // arm64 Linux "Image" header (Documentation/arch/arm64/booting.rst),
    // so standard loaders (U-Boot booti, firmware, QEMU -kernel Image)
    // can boot us and pass the device tree in x0. The first word is
    // also the first instruction.
    b primary_entry                 // code0
    .long 0                         // code1
    .quad 0x200000                  // text_offset: RAM base + 2 MiB
    .quad __image_size              // image_size (includes .bss + stack)
    .quad 0b1010                    // flags: little-endian, 4 KiB pages,
                                    //   may be placed anywhere in RAM
    .quad 0, 0, 0                   // reserved
    .ascii "ARM\x64"                // magic
    .long 0                         // reserved (no PE/COFF header)

primary_entry:
    // x0 = device tree (if the loader passed one); keep it in x19
    mov x19, x0

    // Only the boot CPU runs this path (secondaries come in via PSCI or
    // a spin-table release)
    mrs x1, mpidr_el1
    and x1, x1, #0xFF
    cbnz x1, park

    bl drop_to_el1

    // ---- Self-relocation (MMU off; adrp/adr are PC-relative) ----
    adr x20, _start                 // where we actually are
    movz x2, #(LINK_BASE >> 16), lsl #16
    sub x20, x20, x2                // x20 = load delta
    adrp x3, __rela_start
    add x3, x3, :lo12:__rela_start
    adrp x4, __rela_end
    add x4, x4, :lo12:__rela_end
    mov x21, #0                     // relocations applied
1:  cmp x3, x4
    b.hs 2f
    ldp x5, x6, [x3], #16           // r_offset, r_info
    ldr x7, [x3], #8                // r_addend
    cmp w6, #R_AARCH64_RELATIVE
    b.ne 1b                         // (the linker emits nothing else)
    add x5, x5, x20
    add x7, x7, x20
    str x7, [x5]                    // *(offset + delta) = addend + delta
    add x21, x21, #1
    b 1b
2:
    adrp x1, __boot_stack_top
    add x1, x1, :lo12:__boot_stack_top
    mov sp, x1

    // Zero .bss
    adrp x1, __bss_start
    add x1, x1, :lo12:__bss_start
    adrp x2, __bss_end
    add x2, x2, :lo12:__bss_end
3:  cmp x1, x2
    b.hs 4f
    str xzr, [x1], #8
    b 3b
4:
    // The boot protocol hands us the image cleaned to the point of
    // coherency, but clean lines may still hold pre-boot contents of
    // addresses we just wrote with caches off (.bss, the stack, the
    // relocated data, and the page tables to come). Invalidate the whole
    // image range so nothing stale can be hit once the caches are on.
    adrp x1, __kernel_start
    add x1, x1, :lo12:__kernel_start
    adrp x2, __kernel_end
    add x2, x2, :lo12:__kernel_end
    mrs x3, ctr_el0
    ubfx x3, x3, #16, #4        // DminLine: log2(words) of the smallest D-line
    mov x4, #4
    lsl x4, x4, x3              // line size in bytes
    sub x5, x4, #1
    bic x1, x1, x5
5:  dc ivac, x1
    add x1, x1, x4
    cmp x1, x2
    b.lo 5b
    dsb sy

    msr tpidr_el1, xzr          // scheduler CPU index 0
    mov x0, x19                 // device tree
    adr x1, _start              // load address
    mov x2, x21                 // relocations applied
    bl arm64_boot_main
park:
    wfe
    b park

    // Drop to EL1h (interrupts masked) from EL3 or EL2, leaving EL1 set up
    // for an AArch64 kernel: timers and the GICv3 system registers usable
    // from EL1. Returns at EL1 on the same stack.
    .globl drop_to_el1
drop_to_el1:
    mrs x9, CurrentEL
    lsr x9, x9, #2
    cmp x9, #1
    b.eq 9f                     // already EL1
    mrs x10, id_aa64pfr0_el1
    ubfx x11, x10, #24, #4      // GIC system registers implemented?
    ubfx x12, x10, #8, #4       // EL2 implemented?
    cmp x9, #3
    b.ne 7f
    // ---- EL3 ----
    cbz x11, 6f
    mov x13, #0xF               // ICC_SRE_EL3 = Enable | DIB | DFB | SRE
    msr S3_6_C12_C12_5, x13
    isb
6:  mov x13, #0x431             // SCR_EL3: RW (EL2/EL1 AArch64), HCE,
    msr scr_el3, x13            //   RES1 bits, NS (non-secure below)
    cbz x12, 8f                 // no EL2: go straight to EL1
    // ---- EL2 registers (from EL2 itself, or from EL3 on the way down) ----
7:  mov x13, #(1 << 31)         // HCR_EL2.RW: EL1 is AArch64
    msr hcr_el2, x13
    mov x13, #3                 // CNTHCTL_EL2: EL1 physical timer/counter access
    msr cnthctl_el2, x13
    msr cntvoff_el2, xzr
    cbz x11, 8f
    mov x13, #0xF               // ICC_SRE_EL2 = Enable | DIB | DFB | SRE
    msr S3_4_C12_C9_5, x13
    isb
8:  mov x13, sp
    msr sp_el1, x13
    mov x13, #0x3C5             // EL1h, DAIF masked
    cmp x9, #3
    b.eq 10f
    msr spsr_el2, x13
    msr elr_el2, x30
    eret
10: msr spsr_el3, x13
    msr elr_el3, x30
    eret
9:  ret

    // Secondary CPU entry (PSCI CPU_ON): x0 = &ApBoot, MMU off
    .globl secondary_start
secondary_start:
    mov x19, x0
    bl drop_to_el1
    ldr x1, [x19, #0]           // stack_top
    mov sp, x1
    ldr x1, [x19, #8]           // cpu index
    msr tpidr_el1, x1
    ldr x2, [x19, #16]          // mair
    ldr x3, [x19, #24]          // tcr
    ldr x4, [x19, #32]          // ttbr0
    ldr x5, [x19, #40]          // sctlr
    msr mair_el1, x2
    msr tcr_el1, x3
    msr ttbr0_el1, x4
    isb
    tlbi vmalle1
    dsb ish
    isb
    msr sctlr_el1, x5
    isb
    ic iallu                    // drop any stale instructions
    dsb nsh
    isb
    mov x0, x1
    bl arm64_secondary_main
    b park

    // Secondary CPU entry (spin-table release): the firmware's holding
    // pen jumps here with no argument, so the ApBoot pointer comes from
    // AP_SPIN_BOOT (CPUs are released one at a time)
    .globl secondary_spin_entry
secondary_spin_entry:
    adrp x0, AP_SPIN_BOOT
    add x0, x0, :lo12:AP_SPIN_BOOT
    ldr x0, [x0]
    b secondary_start

    .text
    "#
);

extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// Link-time base address (see the LINK_BASE assembler constant)
pub const LINK_BASE: u64 = 0x4020_0000;

/// Where QEMU leaves the device tree for ELF kernels: the start of RAM
const QEMU_VIRT_DTB: u64 = 0x4000_0000;

/// Boot CPU entry from `_start` — MMU still off, relocations applied
#[no_mangle]
extern "C" fn arm64_boot_main(dtb_arg: u64, load_addr: u64, relocs: u64) -> ! {
    // Find and parse the device tree with the MMU off: it says where RAM
    // and the devices are, which the identity map needs
    let dtb = if fdt::is_valid(dtb_arg) {
        dtb_arg
    } else if fdt::is_valid(QEMU_VIRT_DTB) {
        QEMU_VIRT_DTB
    } else {
        loop {
            core::hint::spin_loop(); // no device tree, no console: nothing to report on
        }
    };
    let info = fdt::parse(dtb);

    // Everything the kernel touches before memory management exists
    let mut devices: heapless::Vec<(u64, u64), 48> = heapless::Vec::new();
    let _ = devices.push((info.uart, 0x1000));
    match info.irqchip {
        fdt::IrqChip::Gic(3) => {
            let _ = devices.push((info.gicd, 0x1_0000));
            let _ = devices.push((info.gicr, info.gicr_size));
        }
        fdt::IrqChip::Gic(_) => {
            let _ = devices.push((info.gicd, 0x1000));
            let _ = devices.push((info.gicc, 0x2000));
        }
        fdt::IrqChip::Bcm2836 => {
            let _ = devices.push((info.local_intc, 0x100));
            let _ = devices.push((info.armctrl, 0x200));
        }
    }
    for &v in &info.virtio[..info.virtio_count] {
        let _ = devices.push((v, 0x200));
    }
    if let Some(pci) = info.pci {
        if pci.io.2 != 0 {
            let _ = devices.push((pci.io.0, pci.io.2));
        }
        if pci.mem32.2 != 0 {
            let _ = devices.push((pci.mem32.0, pci.mem32.2));
        }
    }
    let ram = unsafe { super::mmu::early_init(&info.ram[..info.ram_count], &devices) };

    super::serial::set_base(info.uart);
    crate::serial::init();
    crate::print_banner();
    super::exceptions::init();

    {
        use core::fmt::Write;
        let mut line: heapless::String<200> = heapless::String::new();
        let _ = write!(
            line,
            "[BOOT] Loaded at 0x{:x} (linked at 0x{:x}), {} relocations applied\n",
            load_addr, LINK_BASE, relocs
        );
        let _ = write!(
            line,
            "[DTB] Device tree at 0x{:x}: {} CPU(s), {} virtio-mmio slots, SMP via {}\n",
            dtb,
            info.cpu_count,
            info.virtio_count,
            if info.cpu_release[..info.cpu_count].iter().any(|&r| r != 0) {
                "spin-table"
            } else {
                match info.psci {
                    fdt::PsciConduit::Hvc => "PSCI (HVC)",
                    fdt::PsciConduit::Smc => "PSCI (SMC)",
                    fdt::PsciConduit::None => "nothing (single CPU)",
                }
            }
        );
        crate::serial::write_str(&line);
    }

    // RAM
    let kernel_start = core::ptr::addr_of!(__kernel_start) as u64;
    let kernel_end = core::ptr::addr_of!(__kernel_end) as u64;
    for &(base, size) in &ram {
        crate::serial::write_str("[MEM] RAM 0x");
        crate::serial::write_hex(base);
        crate::serial::write_str(" - 0x");
        crate::serial::write_hex(base + size);
        crate::serial::write_str("\n");
    }
    if ram.is_empty() {
        panic!("the device tree describes no usable RAM");
    }
    super::memory::init(&ram, &info.reserved[..info.reserved_count], dtb, kernel_start, kernel_end);

    // Interrupts: controller + this CPU, UART RX, the generic timer
    super::irq::init(&info);
    super::serial::enable_rx_irq();

    crate::serial::write_str("[INIT] Scheduler...\n");
    crate::scheduler::init();

    crate::serial::write_str("[INIT] SMP...\n");
    super::smp::boot_secondaries(&info);

    crate::serial::write_str("[INIT] Virtio drivers...\n");
    super::virtio_mmio::probe(&info.virtio[..info.virtio_count]);

    // PCIe: map ECAM, assign BARs, then the same virtio-pci driver as amd64
    if let Some(host) = info.pci {
        if super::pcie::init(&host) {
            let devices = crate::pci::scan();
            crate::virtio::pci::init(&devices);
        }
    }

    crate::kernel_run()
}
