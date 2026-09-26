//! Interrupt-controller front end: the GIC (v2/v3) on most machines, the
//! BCM2836 local controller on the Raspberry Pi 2/3. The device tree says
//! which; everything else calls through here.

use core::sync::atomic::{AtomicBool, Ordering};

use super::fdt::{IrqChip, MachineInfo};

static BCM: AtomicBool = AtomicBool::new(false);

fn bcm() -> bool {
    BCM.load(Ordering::Relaxed)
}

/// Boot CPU: set up the controller and this CPU's interface
pub fn init(info: &MachineInfo) {
    use core::fmt::Write;
    let mut line: heapless::String<200> = heapless::String::new();
    match info.irqchip {
        IrqChip::Gic(v) => {
            if v == 3 {
                let _ = write!(line, "[IRQ] GICv3: distributor 0x{:x}, redistributors 0x{:x} (+0x{:x})",
                    info.gicd, info.gicr, info.gicr_size);
            } else {
                let _ = write!(line, "[IRQ] GICv2: distributor 0x{:x}, CPU interface 0x{:x}",
                    info.gicd, info.gicc);
            }
            let _ = write!(line, "; timer INTID {}", info.timer_irq);
        }
        IrqChip::Bcm2836 => {
            let _ = write!(line, "[IRQ] BCM2836 local controller 0x{:x}, armctrl 0x{:x}; UART IRQ {}",
                info.local_intc, info.armctrl, info.uart_irq);
        }
    }
    let _ = write!(line, "; counter {} MHz\n", super::timer::frequency() / 1_000_000);
    crate::serial::write_str(&line);

    match info.irqchip {
        IrqChip::Gic(v) => super::gic::init(&super::gic::GicConfig {
            version: v,
            gicd: info.gicd,
            gicc_or_gicr: if v == 3 { info.gicr } else { info.gicc },
            gicr_size: info.gicr_size,
            timer_irq: info.timer_irq,
            uart_irq: info.uart_irq,
        }),
        IrqChip::Bcm2836 => {
            BCM.store(true, Ordering::Relaxed);
            super::bcm2836::init(info.local_intc, info.armctrl, info.uart_irq);
        }
    }
}

/// Secondary CPU: this CPU's interface
pub fn init_cpu() {
    if bcm() {
        super::bcm2836::init_cpu()
    } else {
        super::gic::init_cpu()
    }
}

/// The calling CPU's IPI target
pub fn cpu_target_id() -> u32 {
    if bcm() {
        super::bcm2836::cpu_target_id()
    } else {
        super::gic::cpu_target_id()
    }
}

pub fn send_resched(target: u32) {
    if bcm() {
        super::bcm2836::send_resched(target)
    } else {
        super::gic::send_resched(target)
    }
}

/// IRQ dispatch from the vector table. Returns the frame to resume.
///
/// # Safety
/// Only from `arm64_exception`, with `sp` the saved frame.
pub unsafe fn handle_irq(sp: u64) -> u64 {
    if bcm() {
        super::bcm2836::handle_irq(sp)
    } else {
        super::gic::handle_irq(sp)
    }
}
