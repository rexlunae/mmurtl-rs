//! arm64 boot: QEMU `virt` loads the kernel ELF at 0x4020_0000 and enters
//! `_start` on CPU 0 at EL1 (or EL2, which we drop out of), MMU off.
//! Secondary CPUs stay powered off until PSCI `CPU_ON` starts them at
//! `secondary_start` (see `smp.rs`).
//!
//! Boot CPU sequence: MMU on → device tree → console → vectors → memory →
//! GIC + timer → scheduler → secondary CPUs → virtio-mmio devices →
//! `crate::kernel_run()`.

use core::arch::global_asm;

use super::fdt;

global_asm!(
    r#"
    .section .text.boot, "ax"
    .globl _start
_start:
    // x0 = device tree (if the loader passed one); keep it in x19
    mov x19, x0

    // Only the boot CPU runs this path (secondaries come in via PSCI)
    mrs x1, mpidr_el1
    and x1, x1, #0xFF
    cbnz x1, park

    bl drop_to_el1

    adrp x1, __boot_stack_top
    add x1, x1, :lo12:__boot_stack_top
    mov sp, x1

    // Zero .bss
    adrp x1, __bss_start
    add x1, x1, :lo12:__bss_start
    adrp x2, __bss_end
    add x2, x2, :lo12:__bss_end
1:  cmp x1, x2
    b.hs 2f
    str xzr, [x1], #8
    b 1b
2:
    msr tpidr_el1, xzr          // scheduler CPU index 0
    mov x0, x19
    bl arm64_boot_main
park:
    wfe
    b park

    // If entered at EL2 (e.g. -machine virtualization=on), configure EL1
    // as AArch64 with timer access and drop to EL1h, masked.
    .globl drop_to_el1
drop_to_el1:
    mrs x9, CurrentEL
    lsr x9, x9, #2
    cmp x9, #2
    b.ne 3f
    mov x9, #(1 << 31)          // HCR_EL2.RW: EL1 is AArch64
    msr hcr_el2, x9
    mov x9, #3                  // CNTHCTL_EL2: EL1 physical timer/counter access
    msr cnthctl_el2, x9
    msr cntvoff_el2, xzr
    // If the GICv3 system-register interface exists, let EL1 use it:
    // ICC_SRE_EL2 = Enable | DIB | DFB | SRE
    mrs x9, id_aa64pfr0_el1
    ubfx x9, x9, #24, #4
    cbz x9, 4f
    mov x9, #0xF
    msr S3_4_C12_C9_5, x9
    isb
4:
    mov x9, #0x3C5              // SPSR_EL2: EL1h, DAIF masked
    msr spsr_el2, x9
    mov x9, sp
    msr sp_el1, x9
    msr elr_el2, x30
    eret
3:  ret

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
    mov x0, x1
    bl arm64_secondary_main
    b park

    .text
    "#
);

extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// Where QEMU leaves the device tree for ELF kernels: the start of RAM
const QEMU_VIRT_DTB: u64 = 0x4000_0000;

/// Boot CPU entry from `_start`
#[no_mangle]
extern "C" fn arm64_boot_main(dtb_arg: u64) -> ! {
    // Caches and a sane memory model before any other Rust runs
    unsafe { super::mmu::early_init() };

    let dtb = if fdt::is_valid(dtb_arg) {
        dtb_arg
    } else if fdt::is_valid(QEMU_VIRT_DTB) {
        QEMU_VIRT_DTB
    } else {
        0
    };
    let info = if dtb != 0 {
        fdt::parse(dtb)
    } else {
        panic!("no device tree found")
    };

    super::serial::set_base(info.uart);
    crate::serial::init();
    crate::print_banner();
    super::exceptions::init();

    crate::serial::write_str("[DTB] Device tree at 0x");
    crate::serial::write_hex(dtb);
    crate::serial::write_str(": ");
    crate::serial::write_dec(info.cpu_count as u64);
    crate::serial::write_str(" CPU(s), ");
    crate::serial::write_dec(info.virtio_count as u64);
    crate::serial::write_str(" virtio-mmio slots, PSCI ");
    crate::serial::write_str(match info.psci {
        fdt::PsciConduit::Hvc => "via HVC",
        fdt::PsciConduit::Smc => "via SMC",
        fdt::PsciConduit::None => "absent",
    });
    crate::serial::write_str("\n");

    // RAM
    let ram = &info.ram[..info.ram_count];
    super::mmu::trim_ram(ram);
    let kernel_start = core::ptr::addr_of!(__kernel_start) as u64;
    let kernel_end = core::ptr::addr_of!(__kernel_end) as u64;
    let mut ram_bytes = 0;
    for &(base, size) in ram {
        ram_bytes += size;
        crate::serial::write_str("[MEM] RAM 0x");
        crate::serial::write_hex(base);
        crate::serial::write_str(" - 0x");
        crate::serial::write_hex(base + size);
        crate::serial::write_str("\n");
        if base + size > super::mmu::EARLY_RAM_END {
            crate::serial::write_line("[MEM] (RAM above the early identity map is ignored)");
        }
    }
    let _ = ram_bytes;
    super::memory::init(ram, dtb, kernel_start, kernel_end);

    // Interrupts: GIC distributor + this CPU, UART RX, the generic timer
    {
        use core::fmt::Write;
        let mut line: heapless::String<160> = heapless::String::new();
        if info.gic_version == 3 {
            let _ = write!(line, "[GIC] GICv3: distributor 0x{:x}, redistributors 0x{:x} (+0x{:x})",
                info.gicd, info.gicr, info.gicr_size);
        } else {
            let _ = write!(line, "[GIC] GICv2: distributor 0x{:x}, CPU interface 0x{:x}",
                info.gicd, info.gicc);
        }
        let _ = write!(line, "; timer INTID {} @ {} MHz\n",
            info.timer_irq, super::timer::frequency() / 1_000_000);
        crate::serial::write_str(&line);
    }
    super::gic::init(&super::gic::GicConfig {
        version: info.gic_version,
        gicd: info.gicd,
        gicc_or_gicr: if info.gic_version == 3 { info.gicr } else { info.gicc },
        gicr_size: info.gicr_size,
        timer_irq: info.timer_irq,
        uart_irq: info.uart_irq,
    });
    super::serial::enable_rx_irq();

    crate::serial::write_str("[INIT] Scheduler...\n");
    crate::scheduler::init();

    crate::serial::write_str("[INIT] SMP...\n");
    super::smp::boot_secondaries(&info);

    crate::serial::write_str("[INIT] Virtio drivers...\n");
    super::virtio_mmio::probe(&info.virtio[..info.virtio_count]);

    crate::kernel_run()
}
