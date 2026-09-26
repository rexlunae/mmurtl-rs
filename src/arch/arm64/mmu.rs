//! arm64 MMU: 4 KiB granule, 48-bit VA, one address space in TTBR0.
//!
//! The kernel runs identity-mapped (VA == PA), built from the device tree
//! so it works wherever RAM and devices are (RAM at 1 GiB on QEMU `virt`,
//! at 0 on a Raspberry Pi, with the Pi's peripherals right after RAM):
//!   - RAM ranges: Normal write-back, 2 MiB blocks (only whole blocks)
//!   - device regions (UART, interrupt controller, virtio, PCIe, ...):
//!     Device-nGnRnE, 2 MiB blocks covering each region
//! Both are EL1-only (AP=00) and never executable from EL0 (UXN); devices
//! are never executable at all (PXN). Level-2 tables for the kernel map
//! come from a static pool, since the map is built before memory
//! management exists (with the MMU still off). User pages (the
//! `memory::user` window) are 4 KiB pages in dynamically allocated tables.
//!
//! Descriptor permission bits used:
//!   AP[7:6]  00 = EL1 RW / EL0 none, 01 = EL1+EL0 RW, 11 = EL1+EL0 RO
//!   PXN(53)  no execution at EL1       UXN(54)  no execution at EL0

use core::arch::asm;

const ENTRIES: usize = 512;

#[repr(C, align(4096))]
struct Table([u64; ENTRIES]);

/// Level-0 table (512 GiB per entry)
static mut L0: Table = Table([0; ENTRIES]);
/// Level-1 table for VA [0, 512 GiB) (1 GiB per entry)
static mut L1: Table = Table([0; ENTRIES]);
/// Level-2 tables (2 MiB blocks), one per GiB the kernel maps anything in
const L2_POOL: usize = 24;
static mut L2: [Table; L2_POOL] = [const { Table([0; ENTRIES]) }; L2_POOL];
static mut L2_USED: usize = 0;

const VALID: u64 = 1 << 0;
const TABLE: u64 = 1 << 1; // table (L0-L2) or page (L3)
const ATTR_DEVICE: u64 = 0 << 2; // MAIR index 0
const ATTR_NORMAL: u64 = 1 << 2; // MAIR index 1
const AP_EL0_RW: u64 = 0b01 << 6;
const AP_RO_ALL: u64 = 0b11 << 6;
const AP_EL0_BIT: u64 = 1 << 6; // EL0 may access
const AP_RO_BIT: u64 = 1 << 7; // read-only
const SH_INNER: u64 = 0b11 << 8;
const AF: u64 = 1 << 10;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

const GIB: u64 = 1 << 30;
const MIB2: u64 = 2 << 20;

/// MAIR: 0 = Device-nGnRnE, 1 = Normal WB RA/WA, 2 = Normal non-cacheable
const MAIR: u64 = 0x00 | (0xFF << 8) | (0x44 << 16);

/// SCTLR_EL1: ARMv8.0 RES1 bits + M (MMU), C (D-cache), SA (SP alignment
/// check), I (I-cache). EL0 access to DAIF (UMA), cache maintenance, and
/// WFI/WFE all trap.
const SCTLR: u64 = 0x30D0_0800 | (1 << 0) | (1 << 2) | (1 << 3) | (1 << 12);

/// Register values every CPU loads to share the boot CPU's address space
#[derive(Clone, Copy)]
pub struct MmuRegs {
    pub mair: u64,
    pub tcr: u64,
    pub ttbr0: u64,
    pub sctlr: u64,
}

fn tcr_value() -> u64 {
    let parange: u64;
    unsafe { asm!("mrs {}, id_aa64mmfr0_el1", out(reg) parange) };
    let ips = (parange & 0xF).min(0b101); // up to 48-bit PA
    16 // T0SZ: 48-bit VA
        | (0b01 << 8) // IRGN0: WB RA/WA
        | (0b01 << 10) // ORGN0: WB RA/WA
        | (0b11 << 12) // SH0: inner shareable
        | (0b00 << 14) // TG0: 4 KiB
        | (16 << 16) // T1SZ
        | (1 << 23) // EPD1: no TTBR1 walks
        | (0b10 << 30) // TG1: 4 KiB
        | (ips << 32)
}

pub fn regs() -> MmuRegs {
    MmuRegs {
        mair: MAIR,
        tcr: tcr_value(),
        ttbr0: core::ptr::addr_of!(L0) as u64,
        sctlr: SCTLR,
    }
}

/// The level-2 table for GiB `gib`, taking one from the pool if needed
unsafe fn l2_for(gib: u64) -> Option<*mut u64> {
    if gib >= ENTRIES as u64 {
        return None; // beyond 512 GiB: not mapped by the kernel
    }
    let l1 = &mut (*core::ptr::addr_of_mut!(L1)).0;
    let e = l1[gib as usize];
    if e & VALID != 0 {
        return if e & TABLE != 0 { Some((e & ADDR_MASK) as *mut u64) } else { None };
    }
    let used = &mut *core::ptr::addr_of_mut!(L2_USED);
    if *used >= L2_POOL {
        return None;
    }
    let t = core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(L2))[*used]) as *mut u64;
    *used += 1;
    for i in 0..ENTRIES {
        core::ptr::write_volatile(t.add(i), 0);
    }
    core::ptr::write_volatile(&mut l1[gib as usize], t as u64 | TABLE | VALID);
    Some(t)
}

/// Map the 2 MiB block at `pa` with `attrs`, unless something is already
/// mapped there. Returns whether the block is now mapped with `attrs`.
unsafe fn map_block(pa: u64, attrs: u64) -> bool {
    let t = match l2_for(pa / GIB) {
        Some(t) => t,
        None => return false,
    };
    let e = t.add(((pa % GIB) / MIB2) as usize);
    let d = core::ptr::read_volatile(e);
    if d & VALID != 0 {
        return d & !ADDR_MASK == attrs & !ADDR_MASK;
    }
    core::ptr::write_volatile(e, pa | attrs);
    true
}

const RAM_ATTRS: u64 = ATTR_NORMAL | SH_INNER | AF | UXN | VALID;
const DEVICE_ATTRS: u64 = ATTR_DEVICE | AF | PXN | UXN | VALID;

/// Build the kernel's identity map from the device tree's RAM and device
/// regions and turn the MMU on (boot CPU, MMU off). Returns the RAM
/// actually mapped (whole 2 MiB blocks, within the table pool), which is
/// the RAM the kernel may use.
///
/// # Safety
/// Once, on the boot CPU, with the MMU off.
pub unsafe fn early_init(ram: &[(u64, u64)], devices: &[(u64, u64)]) -> heapless::Vec<(u64, u64), 8> {
    let l0 = &mut *core::ptr::addr_of_mut!(L0);
    l0.0[0] = core::ptr::addr_of!(L1) as u64 | TABLE | VALID;

    // Devices first: every block a region touches. A RAM range that
    // overlaps a peripheral block (a firmware-reported size covering the
    // Pi's peripheral window, say) then simply skips that block.
    for &(base, size) in devices {
        let _ = map_device(base, size.max(1));
    }
    // RAM: only whole 2 MiB blocks, so nothing that isn't RAM is ever
    // mapped as Normal (cacheable, speculatively accessible) memory
    let mut mapped: heapless::Vec<(u64, u64), 8> = heapless::Vec::new();
    for &(base, size) in ram {
        let end = (base + size) & !(MIB2 - 1);
        let mut pa = (base + MIB2 - 1) & !(MIB2 - 1);
        let mut seg_start = None;
        while pa <= end {
            let ok = pa < end && map_block(pa, RAM_ATTRS);
            match (ok, seg_start) {
                (true, None) => seg_start = Some(pa),
                (false, Some(s)) => {
                    let _ = mapped.push((s, pa - s));
                    seg_start = None;
                }
                _ => {}
            }
            pa += MIB2;
        }
    }

    enable(&regs());
    mapped
}

/// Load the MMU registers and enable translation + caches.
///
/// # Safety
/// The tables in `r.ttbr0` must identity-map the code being executed.
pub unsafe fn enable(r: &MmuRegs) {
    asm!(
        "msr mair_el1, {mair}",
        "msr tcr_el1, {tcr}",
        "msr ttbr0_el1, {ttbr0}",
        "isb",
        "tlbi vmalle1",
        "dsb ish",
        "isb",
        "msr sctlr_el1, {sctlr}",
        "isb",
        "ic iallu",
        "dsb nsh",
        "isb",
        mair = in(reg) r.mair,
        tcr = in(reg) r.tcr,
        ttbr0 = in(reg) r.ttbr0,
        sctlr = in(reg) r.sctlr,
    );
}

/// Identity-map `[pa, pa + size)` as Device memory in the kernel's tables
/// (2 MiB blocks), e.g. a PCIe ECAM window. Every address space shares
/// the kernel's level-1 table, so it appears in all of them. Blocks that
/// are already Device-mapped are left alone; overlapping RAM is an error.
pub fn map_device(pa: u64, size: u64) -> Result<(), &'static str> {
    let mut block = pa & !(MIB2 - 1);
    let end = pa.checked_add(size).ok_or("device region wraps")?;
    let mmu_on = unsafe {
        let v: u64;
        asm!("mrs {}, sctlr_el1", out(reg) v);
        v & 1 != 0
    };
    let mut result = Ok(());
    while block < end {
        if !unsafe { map_block(block, DEVICE_ATTRS) } {
            result = Err("device region overlaps RAM or exceeds the page-table pool");
        }
        block += MIB2;
    }
    if mmu_on {
        unsafe { asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb") };
    }
    result
}

/// Make instructions the kernel just wrote (through its identity map)
/// visible to instruction fetch — e.g. user program code. The I-cache is
/// not coherent with data writes on ARM: clean the data to the point of
/// unification, then invalidate the instruction caches of every CPU.
pub fn sync_icache(kva: *const u8, len: usize) {
    let ctr: u64;
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr) };
    let line = 4u64 << ((ctr >> 16) & 0xF);
    let start = kva as u64 & !(line - 1);
    let end = kva as u64 + len as u64;
    let mut a = start;
    while a < end {
        unsafe { asm!("dc cvau, {}", in(reg) a) };
        a += line;
    }
    unsafe { asm!("dsb ish", "ic ialluis", "dsb ish", "isb") };
}


// ========================================================================
// 4 KiB user pages
// ========================================================================

fn index(va: u64, level: u32) -> usize {
    ((va >> (39 - 9 * level)) & 0x1FF) as usize
}

/// L0 slot holding the user window (memory::user::USER_BASE)
const USER_L0_SLOT: usize = ((crate::memory::user::USER_BASE >> 39) & 0x1FF) as usize;

/// Root table loaded in each CPU's TTBR0
static ACTIVE_ROOT: [core::sync::atomic::AtomicU64; crate::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::scheduler::MAX_CPUS];

fn kernel_root() -> u64 {
    core::ptr::addr_of!(L0) as u64
}

fn alloc_zeroed_frame() -> Option<u64> {
    let pa = crate::memory::heap::with_frame_allocator(|fa| fa.allocate_frame())??;
    unsafe { core::ptr::write_bytes(pa as *mut u8, 0, 4096) };
    Some(pa)
}

fn free_frame(pa: u64) {
    crate::memory::heap::with_frame_allocator(|fa| fa.deallocate_frame(pa));
}

/// Create a user address space: an L0 table sharing the kernel's identity
/// map (every slot but the user window's) with an empty user window.
/// Returns the table's physical address.
pub fn new_address_space() -> Result<u64, &'static str> {
    let root = alloc_zeroed_frame().ok_or("out of memory for a page table")?;
    unsafe {
        let kernel = kernel_root() as *const u64;
        let new = root as *mut u64;
        for i in 0..ENTRIES {
            if i != USER_L0_SLOT {
                *new.add(i) = *kernel.add(i);
            }
        }
        asm!("dsb ishst");
    }
    Ok(root)
}

/// Walk `root` to the level-3 entry for `va`, allocating tables if
/// `create`
unsafe fn walk(root: u64, va: u64, create: bool) -> Option<*mut u64> {
    let mut table = root as *mut u64;
    for level in 0..3 {
        let e = table.add(index(va, level));
        let d = core::ptr::read_volatile(e);
        if d & VALID == 0 {
            if !create {
                return None;
            }
            let pa = alloc_zeroed_frame()?;
            asm!("dsb ishst");
            core::ptr::write_volatile(e, pa | TABLE | VALID);
            table = pa as *mut u64;
        } else if d & TABLE == 0 {
            return None; // a block mapping: not a user page
        } else {
            table = (d & ADDR_MASK) as *mut u64;
        }
    }
    Some(table.add(index(va, 3)))
}

/// Map one 4 KiB user page `va` → `pa` in address space `root`
pub fn map_user_page(root: u64, va: u64, pa: u64, writable: bool, executable: bool) -> Result<(), &'static str> {
    let mut desc = (pa & ADDR_MASK) | ATTR_NORMAL | SH_INNER | AF | PXN | TABLE | VALID;
    desc |= if writable { AP_EL0_RW } else { AP_RO_ALL };
    if !executable {
        desc |= UXN;
    }
    unsafe {
        let e = walk(root, va, true).ok_or("out of memory for page tables")?;
        if core::ptr::read_volatile(e) & VALID != 0 {
            return Err("page already mapped");
        }
        core::ptr::write_volatile(e, desc);
        asm!("dsb ishst", "isb");
    }
    Ok(())
}

/// Whether `va` is mapped as a 4 KiB page in the *current* address space
/// (this CPU's TTBR0): None if not, else (user-accessible, writable)
pub fn query_page(va: u64) -> Option<(bool, bool)> {
    let root: u64;
    unsafe {
        asm!("mrs {}, ttbr0_el1", out(reg) root);
        let e = walk(root & ADDR_MASK, va, false)?;
        let d = core::ptr::read_volatile(e);
        if d & VALID == 0 {
            return None;
        }
        Some((d & AP_EL0_BIT != 0, d & AP_RO_BIT == 0))
    }
}

/// Tear down a user address space: free every frame mapped in its user
/// window, the window's tables, and the root. Returns frames freed. The
/// space must not be loaded on any CPU.
pub fn free_address_space(root: u64) -> usize {
    unsafe fn free_level(t: u64, level: u32, freed: &mut usize) {
        for i in 0..ENTRIES {
            let d = core::ptr::read_volatile((t as *const u64).add(i));
            if d & VALID == 0 {
                continue;
            }
            if level < 3 {
                free_level(d & ADDR_MASK, level + 1, freed);
            }
            free_frame(d & ADDR_MASK);
            *freed += 1;
        }
    }
    let mut freed = 0;
    unsafe {
        let top = core::ptr::read_volatile((root as *const u64).add(USER_L0_SLOT));
        if top & VALID != 0 {
            free_level(top & ADDR_MASK, 1, &mut freed);
            free_frame(top & ADDR_MASK);
            freed += 1;
        }
    }
    free_frame(root);
    freed + 1
}

/// Load `root` (0 = the kernel's own tables) into this CPU's TTBR0 if it
/// isn't already, discarding this CPU's stale translations (no ASIDs yet)
pub fn switch_address_space(cpu: usize, root: u64) {
    use core::sync::atomic::Ordering;
    let target = if root == 0 { kernel_root() } else { root };
    if ACTIVE_ROOT[cpu].load(Ordering::Relaxed) == target {
        return;
    }
    unsafe {
        asm!(
            "msr ttbr0_el1, {}",
            "isb",
            "tlbi vmalle1",
            "dsb nsh",
            "isb",
            in(reg) target,
        );
    }
    ACTIVE_ROOT[cpu].store(target, Ordering::Relaxed);
}
