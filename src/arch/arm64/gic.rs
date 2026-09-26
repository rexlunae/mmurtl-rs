//! Generic Interrupt Controller: GICv2 and GICv3.
//!
//! Interrupts used (the same on both versions):
//!   - SGI 1: reschedule IPI (like amd64's vector 0x30)
//!   - PPI (virtual timer, INTID 27 on QEMU virt): the per-CPU scheduler tick
//!   - SPI (PL011 UART RX): console input, routed to the boot CPU
//!
//! GICv2: a memory-mapped distributor and per-CPU interface; SGIs target
//! CPU interface numbers (at most 8 CPUs).
//!
//! GICv3: a memory-mapped distributor with affinity routing, one
//! redistributor per CPU (which owns that CPU's SGIs and PPIs), and the
//! CPU interface as system registers (ICC_*_EL1). SGIs target MPIDR
//! affinities, so there is no 8-CPU limit.
//!
//! Version and addresses come from the device tree.

use core::arch::asm;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

/// SGI used for reschedule IPIs
pub const RESCHED_SGI: u32 = 1;

static VERSION: AtomicU8 = AtomicU8::new(2);
static GICD: AtomicU64 = AtomicU64::new(0);
/// GICv2: CPU interface base. GICv3: redistributor region base.
static GICC_OR_GICR: AtomicU64 = AtomicU64::new(0);
static GICR_SIZE: AtomicU64 = AtomicU64::new(0);
static TIMER_IRQ: AtomicU32 = AtomicU32::new(27);
static UART_IRQ: AtomicU32 = AtomicU32::new(33);
/// GICv3: each CPU's redistributor (RD_base), indexed by scheduler CPU
static GICR_BY_CPU: [AtomicU64; crate::scheduler::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::scheduler::MAX_CPUS];

// Distributor registers (both versions)
const GICD_CTLR: u64 = 0x0000;
const GICD_IGROUPR: u64 = 0x0080;
const GICD_ISENABLER: u64 = 0x0100;
const GICD_IPRIORITYR: u64 = 0x0400;
const GICD_ITARGETSR: u64 = 0x0800; // v2
const GICD_ICFGR: u64 = 0x0C00;
const GICD_SGIR: u64 = 0x0F00; // v2
const GICD_IROUTER: u64 = 0x6000; // v3
const GICD_CTLR_RWP: u32 = 1 << 31;
// GICv2 CPU interface
const GICC_CTLR: u64 = 0x000;
const GICC_PMR: u64 = 0x004;
const GICC_BPR: u64 = 0x008;
const GICC_IAR: u64 = 0x00C;
const GICC_EOIR: u64 = 0x010;
// GICv3 redistributor: RD_base frame, then SGI_base frame at +64 KiB
const GICR_TYPER: u64 = 0x0008;
const GICR_WAKER: u64 = 0x0014;
const GICR_SGI_BASE: u64 = 0x1_0000;
const GICR_IGROUPR0: u64 = GICR_SGI_BASE + 0x0080;
const GICR_ISENABLER0: u64 = GICR_SGI_BASE + 0x0100;
const GICR_IPRIORITYR: u64 = GICR_SGI_BASE + 0x0400;
const WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
const WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;
const TYPER_VLPIS: u64 = 1 << 1;
const TYPER_LAST: u64 = 1 << 4;

fn rd32(addr: u64) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}
fn wr32(addr: u64, v: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
}
fn wr8(addr: u64, v: u8) {
    unsafe { core::ptr::write_volatile(addr as *mut u8, v) }
}
fn rd64(addr: u64) -> u64 {
    unsafe { core::ptr::read_volatile(addr as *const u64) }
}
fn wr64(addr: u64, v: u64) {
    unsafe { core::ptr::write_volatile(addr as *mut u64, v) }
}

fn gicd() -> u64 {
    GICD.load(Ordering::Relaxed)
}
fn v3() -> bool {
    VERSION.load(Ordering::Relaxed) == 3
}

/// MPIDR affinity packed as Aff3.Aff2.Aff1.Aff0 (the GICR_TYPER layout)
fn mpidr_affinity() -> u64 {
    let m: u64;
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) m) };
    ((m >> 8) & 0xFF00_0000) | (m & 0x00FF_FFFF)
}

// GICv3 CPU interface system registers, by raw encoding (so no assembler
// feature flags are needed)
macro_rules! sysreg_write {
    ($enc:literal, $v:expr) => {
        unsafe { asm!(concat!("msr ", $enc, ", {}"), "isb", in(reg) ($v as u64)) }
    };
}
macro_rules! sysreg_read {
    ($enc:literal) => {{
        let v: u64;
        unsafe { asm!(concat!("mrs {}, ", $enc), out(reg) v) };
        v
    }};
}
// ICC_SRE_EL1     S3_0_C12_C12_5    system-register enable
// ICC_PMR_EL1     S3_0_C4_C6_0      priority mask
// ICC_BPR1_EL1    S3_0_C12_C12_3    binary point (group 1)
// ICC_IGRPEN1_EL1 S3_0_C12_C12_7    group 1 enable
// ICC_IAR1_EL1    S3_0_C12_C12_0    acknowledge (group 1)
// ICC_EOIR1_EL1   S3_0_C12_C12_1    end of interrupt (group 1)
// ICC_SGI1R_EL1   S3_0_C12_C11_5    generate group 1 SGI

// ========================================================================
// Initialization
// ========================================================================

/// Which controller, and where (from the device tree)
pub struct GicConfig {
    pub version: u8,
    pub gicd: u64,
    /// GICv2 CPU interface, or GICv3 redistributor region
    pub gicc_or_gicr: u64,
    pub gicr_size: u64,
    pub timer_irq: u32,
    pub uart_irq: u32,
}

/// Distributor setup (boot CPU), then this CPU's interface
pub fn init(cfg: &GicConfig) {
    VERSION.store(cfg.version, Ordering::Relaxed);
    GICD.store(cfg.gicd, Ordering::Relaxed);
    GICC_OR_GICR.store(cfg.gicc_or_gicr, Ordering::Relaxed);
    GICR_SIZE.store(cfg.gicr_size, Ordering::Relaxed);
    TIMER_IRQ.store(cfg.timer_irq, Ordering::Relaxed);
    UART_IRQ.store(cfg.uart_irq, Ordering::Relaxed);

    let d = gicd();
    wr32(d + GICD_CTLR, 0);
    wait_rwp();

    // UART RX: level-triggered SPI, group 1, delivered to this CPU
    let u = cfg.uart_irq as u64;
    let cfg_reg = d + GICD_ICFGR + 4 * (u / 16);
    wr32(cfg_reg, rd32(cfg_reg) & !(0b10 << (2 * (u % 16))));
    wr8(d + GICD_IPRIORITYR + u, 0xA0);
    if v3() {
        // Group 1 (IRQ via ICC_IAR1); GICv2 keeps it in group 0
        let grp = d + GICD_IGROUPR + 4 * (u / 32);
        wr32(grp, rd32(grp) | 1 << (u % 32));
        let a = mpidr_affinity(); // Aff3.Aff2.Aff1.Aff0
        wr64(d + GICD_IROUTER + 8 * u, ((a & 0xFF00_0000) << 8) | (a & 0x00FF_FFFF));
    } else {
        wr8(d + GICD_ITARGETSR + u, 1 << v2_interface_id());
    }
    wr32(d + GICD_ISENABLER + 4 * (u / 32), 1 << (u % 32));

    // v2: enable forwarding. v3: affinity routing (ARE) + both groups.
    // Bits 0/1/4 mean EnableGrp0/EnableGrp1/ARE with a single security
    // state, and EnableGrp1NS/EnableGrp1A/ARE_NS in the non-secure view —
    // what we want either way.
    wr32(d + GICD_CTLR, if v3() { (1 << 4) | (1 << 1) | 1 } else { 1 });
    wait_rwp();

    init_cpu();
}

fn wait_rwp() {
    if v3() {
        while rd32(gicd() + GICD_CTLR) & GICD_CTLR_RWP != 0 {
            core::hint::spin_loop();
        }
    }
}

/// Per-CPU setup: the CPU interface, and this CPU's SGI + timer PPI
pub fn init_cpu() {
    let timer = TIMER_IRQ.load(Ordering::Relaxed);
    if v3() {
        let rd = find_redistributor().expect("no GICv3 redistributor for this CPU");
        GICR_BY_CPU[super::cpu_index()].store(rd, Ordering::Relaxed);

        // Wake the redistributor
        wr32(rd + GICR_WAKER, rd32(rd + GICR_WAKER) & !WAKER_PROCESSOR_SLEEP);
        while rd32(rd + GICR_WAKER) & WAKER_CHILDREN_ASLEEP != 0 {
            core::hint::spin_loop();
        }
        // SGI + timer PPI: group 1, priority, enabled
        let bits = (1 << RESCHED_SGI) | (1 << timer);
        wr32(rd + GICR_IGROUPR0, rd32(rd + GICR_IGROUPR0) | bits);
        wr8(rd + GICR_IPRIORITYR + RESCHED_SGI as u64, 0x80);
        wr8(rd + GICR_IPRIORITYR + timer as u64, 0x80);
        wr32(rd + GICR_ISENABLER0, bits);

        // System-register CPU interface
        sysreg_write!("S3_0_C12_C12_5", sysreg_read!("S3_0_C12_C12_5") | 1); // ICC_SRE_EL1.SRE
        if sysreg_read!("S3_0_C12_C12_5") & 1 == 0 {
            panic!("GICv3 system-register interface unavailable");
        }
        sysreg_write!("S3_0_C4_C6_0", 0xF0u64); // ICC_PMR_EL1
        sysreg_write!("S3_0_C12_C12_3", 0u64); // ICC_BPR1_EL1
        sysreg_write!("S3_0_C12_C12_7", 1u64); // ICC_IGRPEN1_EL1
    } else {
        let d = gicd();
        let c = GICC_OR_GICR.load(Ordering::Relaxed);
        wr8(d + GICD_IPRIORITYR + RESCHED_SGI as u64, 0x80);
        wr8(d + GICD_IPRIORITYR + timer as u64, 0x80);
        wr32(d + GICD_ISENABLER, (1 << RESCHED_SGI) | (1 << timer));
        wr32(c + GICC_PMR, 0xF0);
        wr32(c + GICC_BPR, 0);
        wr32(c + GICC_CTLR, 1);
    }
}

/// Scan the redistributor region for the frame whose GICR_TYPER affinity
/// matches this CPU's MPIDR
fn find_redistributor() -> Option<u64> {
    let base = GICC_OR_GICR.load(Ordering::Relaxed);
    let end = base + GICR_SIZE.load(Ordering::Relaxed);
    let me = mpidr_affinity();
    let mut rd = base;
    while rd < end {
        let typer = rd64(rd + GICR_TYPER);
        if typer >> 32 == me {
            return Some(rd);
        }
        if typer & TYPER_LAST != 0 {
            break;
        }
        rd += if typer & TYPER_VLPIS != 0 { 0x4_0000 } else { 0x2_0000 };
    }
    None
}

/// GICv2: this CPU's interface number. The banked GICD_ITARGETSR0 reads
/// back the calling CPU's own mask.
fn v2_interface_id() -> u32 {
    let mask = rd32(gicd() + GICD_ITARGETSR) & 0xFF;
    if mask == 0 { 0 } else { mask.trailing_zeros() }
}

/// The calling CPU's IPI target: its GICv2 interface number, or (GICv3)
/// its MPIDR affinity packed as Aff2.Aff1.Aff0
pub fn cpu_target_id() -> u32 {
    if v3() {
        (mpidr_affinity() & 0x00FF_FFFF) as u32
    } else {
        v2_interface_id()
    }
}

/// Send the reschedule SGI to the CPU identified by `target`
/// (see `cpu_target_id`)
pub fn send_resched(target: u32) {
    unsafe { asm!("dsb ish") }; // publish scheduler state before the IPI
    if v3() {
        let aff0 = (target & 0xFF) as u64;
        let aff1 = ((target >> 8) & 0xFF) as u64;
        let aff2 = ((target >> 16) & 0xFF) as u64;
        // Target list covers Aff0 values 16*RS .. 16*RS+15
        let v = (aff2 << 32)
            | ((aff0 >> 4) << 44)
            | ((RESCHED_SGI as u64) << 24)
            | (aff1 << 16)
            | (1 << (aff0 & 0xF));
        sysreg_write!("S3_0_C12_C11_5", v); // ICC_SGI1R_EL1
    } else {
        wr32(gicd() + GICD_SGIR, (1 << (16 + target)) | RESCHED_SGI);
    }
}

// ========================================================================
// IRQ dispatch
// ========================================================================

fn acknowledge() -> u32 {
    if v3() {
        sysreg_read!("S3_0_C12_C12_0") as u32 // ICC_IAR1_EL1
    } else {
        rd32(GICC_OR_GICR.load(Ordering::Relaxed) + GICC_IAR)
    }
}

fn end_of_interrupt(iar: u32) {
    if v3() {
        sysreg_write!("S3_0_C12_C12_1", iar); // ICC_EOIR1_EL1
    } else {
        wr32(GICC_OR_GICR.load(Ordering::Relaxed) + GICC_EOIR, iar);
    }
}

/// IRQ dispatch from the vector table. Returns the frame to resume.
///
/// # Safety
/// Only from `arm64_exception`, with `sp` the saved frame.
pub unsafe fn handle_irq(sp: u64) -> u64 {
    let iar = acknowledge();
    let intid = iar & 0x00FF_FFFF;
    if (1020..1024).contains(&intid) {
        return sp; // spurious
    }
    if intid == TIMER_IRQ.load(Ordering::Relaxed) {
        super::timer::rearm();
        end_of_interrupt(iar);
        return crate::scheduler::switch_from_interrupt(sp, true);
    }
    if intid == RESCHED_SGI {
        end_of_interrupt(iar);
        return crate::scheduler::switch_from_interrupt(sp, false);
    }
    if intid == UART_IRQ.load(Ordering::Relaxed) {
        super::serial::handle_rx_irq();
    }
    end_of_interrupt(iar);
    sp
}
