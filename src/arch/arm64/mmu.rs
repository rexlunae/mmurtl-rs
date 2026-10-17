//! arm64 MMU: 4 KiB granule, 48-bit user TTBR0 and high-half kernel TTBR1.
//!
//! The image and physical window have separate randomized TTBR1 placements, built from the device tree
//! so it works wherever RAM and devices are (RAM at 1 GiB on QEMU `virt`,
//! at 0 on a Raspberry Pi, with the Pi's peripherals right after RAM):
//!   - RAM ranges: Normal write-back, 2 MiB blocks (only whole blocks)
//!   - device regions (UART, interrupt controller, virtio, PCIe, ...):
//!     Device-nGnRnE, 2 MiB blocks covering each region
//! Both are EL1-only (AP=00) and never executable from EL0 (UXN); devices
//! and RAM are non-executable (PXN); only read-only kernel text executes.
//! Level-2 tables for the kernel map
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
static mut EMPTY_L0: Table = Table([0; ENTRIES]);
static mut BOOT_L0: Table = Table([0; ENTRIES]);
static mut BOOT_L1: Table = Table([0; ENTRIES]);
static mut BOOT_L2: [Table; 2] = [const { Table([0; ENTRIES]) }; 2];
static mut BOOT_L3: [Table; 16] = [const { Table([0; ENTRIES]) }; 16];
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
const NG: u64 = 1 << 11;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

const GIB: u64 = 1 << 30;
const MIB2: u64 = 2 << 20;

/// MAIR: 0 = Device-nGnRnE, 1 = Normal WB RA/WA, 2 = Normal non-cacheable
const MAIR: u64 = 0x00 | (0xFF << 8) | (0x44 << 16);

/// SCTLR_EL1: ARMv8.0 RES1 bits + M (MMU), C (D-cache), SA (SP alignment
/// check), I (I-cache), WXN (writable implies execute-never). EL0 access to DAIF (UMA), cache maintenance, and
/// WFI/WFE all trap.
const SCTLR: u64 = (1 << 19) | 0x30D0_0800 | (1 << 0) | (1 << 2) | (1 << 3) | (1 << 12);

/// Register values every CPU loads to share the boot CPU's address space
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MmuRegs {
    pub mair: u64,
    pub tcr: u64,
    pub ttbr0: u64,
    pub ttbr1: u64,
    pub sctlr: u64,
    pub kernel_offset: u64,
    pub physical_offset: u64,
}

const _: () = assert!(core::mem::offset_of!(MmuRegs, kernel_offset) == 40
    && core::mem::offset_of!(MmuRegs, physical_offset) == 48);

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
        | (0b01 << 24) // IRGN1: WB RA/WA
        | (0b01 << 26) // ORGN1: WB RA/WA
        | (0b11 << 28) // SH1: inner shareable
        | (0b10 << 30) // TG1: 4 KiB
        | (ips << 32)
}

pub fn regs() -> MmuRegs {
    MmuRegs {
        mair: MAIR,
        tcr: tcr_value(),
        ttbr0: boot_root() | (255 << 48), // reserved, non-global bootstrap ASID
        ttbr1: kernel_root(),
        sctlr: SCTLR,
        kernel_offset: super::memory::kernel_offset(),
        physical_offset: super::memory::physical_offset(),
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
        return if e & TABLE != 0 { Some(table_pointer(e & ADDR_MASK)) } else { None };
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
    core::ptr::write_volatile(&mut l1[gib as usize], super::memory::virt_to_phys(t as u64) | TABLE | VALID);
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

const RAM_ATTRS: u64 = ATTR_NORMAL | SH_INNER | AF | PXN | UXN | VALID;
const DEVICE_ATTRS: u64 = ATTR_DEVICE | AF | PXN | UXN | VALID;

// Boot-only pool: image pages require finer permissions than the RAM blocks.
const L3_POOL: usize = 16;
static mut L3: [Table; L3_POOL] = [const { Table([0; ENTRIES]) }; L3_POOL];
static mut L3_USED: usize = 0;

static mut IMAGE_L1: Table = Table([0; ENTRIES]);
static mut IMAGE_L2: [Table; 2] = [const { Table([0; ENTRIES]) }; 2];
static mut IMAGE_L3: [Table; 16] = [const { Table([0; ENTRIES]) }; 16];
static mut IMAGE_L2_USED: usize = 0;
static mut IMAGE_L3_USED: usize = 0;
unsafe fn map_image_page(pa: u64, attrs: u64) {
    let va = pa.checked_add(super::memory::kernel_offset()).expect("kernel VA overflow");
    let top = ((va >> 39) & 511) as usize;
    let l0 = &mut (*core::ptr::addr_of_mut!(L0)).0;
    let l1_pa = super::memory::virt_to_phys(core::ptr::addr_of!(IMAGE_L1) as u64);
    assert!(l0[top] == 0 || l0[top] == l1_pa | TABLE | VALID, "image leaves reserved slot");
    l0[top] = l1_pa | TABLE | VALID;
    let l1 = &mut (*core::ptr::addr_of_mut!(IMAGE_L1)).0;
    let i = ((va >> 30) & 511) as usize;
    if l1[i] == 0 {
        assert!(IMAGE_L2_USED < 2, "kernel image exceeds L2 pool");
        l1[i] = super::memory::virt_to_phys(core::ptr::addr_of!((*core::ptr::addr_of!(IMAGE_L2))[IMAGE_L2_USED]) as u64) | TABLE | VALID;
        IMAGE_L2_USED += 1;
    }
    let l2 = table_pointer(l1[i] & ADDR_MASK).add(((va >> 21) & 511) as usize);
    if l2.read() == 0 {
        assert!(IMAGE_L3_USED < 16, "kernel image exceeds L3 pool");
        l2.write(super::memory::virt_to_phys(core::ptr::addr_of!((*core::ptr::addr_of!(IMAGE_L3))[IMAGE_L3_USED]) as u64) | TABLE | VALID);
        IMAGE_L3_USED += 1;
    }
    table_pointer(l2.read() & ADDR_MASK).add(((va >> 12) & 511) as usize).write(pa | attrs);
}

unsafe fn protect_kernel_image() {
    extern "C" {
        static __kernel_start: u8; static __kernel_end: u8;
        static __text_start: u8; static __text_end: u8;
        static __rodata_start: u8; static __rodata_end: u8;
        static __relro_start: u8; static __relro_end: u8;
    }
    let start = core::ptr::addr_of!(__kernel_start) as u64;
    let end = core::ptr::addr_of!(__kernel_end) as u64;
    let text = core::ptr::addr_of!(__text_start) as u64..core::ptr::addr_of!(__text_end) as u64;
    let ro = core::ptr::addr_of!(__rodata_start) as u64..core::ptr::addr_of!(__rodata_end) as u64;
    let relro = core::ptr::addr_of!(__relro_start) as u64..core::ptr::addr_of!(__relro_end) as u64;
    for pa in (start..end).step_by(4096) {
        let l2 = l2_for(pa / GIB).expect("kernel outside mapped RAM");
        let slot = l2.add(((pa % GIB) / MIB2) as usize);
        let mut d = slot.read_volatile();
        if d & TABLE == 0 {
            assert!(d & !ADDR_MASK == RAM_ATTRS, "kernel outside Normal RAM");
            let used = core::ptr::addr_of_mut!(L3_USED);
            assert!(*used < L3_POOL, "kernel exceeds boot L3 pool");
            let table = core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(L3))[*used]) as *mut u64;
            *used += 1;
            let base = pa & !(MIB2 - 1);
            for i in 0..ENTRIES { table.add(i).write_volatile((base + i as u64 * 4096) | RAM_ATTRS | TABLE); }
            d = super::memory::virt_to_phys(table as u64) | VALID | TABLE;
            slot.write_volatile(d);
        }
        let mut attrs = RAM_ATTRS | TABLE;
        if text.contains(&pa) { attrs |= AP_RO_BIT; }
        else if ro.contains(&pa) || relro.contains(&pa) { attrs |= AP_RO_BIT; }
        table_pointer(d & ADDR_MASK).add(((pa >> 12) & 511) as usize).write_volatile(pa | attrs);
        // Image has a separate alias; the physical window always stays NX.
        let code_attrs = if text.contains(&pa) { attrs & !PXN } else { attrs };
        map_image_page(pa, code_attrs);
    }
}

/// Audit the shared kernel map before launching untrusted tasks.
pub fn audit_kernel_wx() {
    unsafe fn visit(table: u64, level: u32, read_only: bool, nx: bool) {
        for i in 0..ENTRIES {
            let d = table_pointer(table).add(i).read_volatile();
            if d & VALID == 0 { continue; }
            if level < 3 && d & TABLE != 0 {
                visit(d & ADDR_MASK, level + 1, read_only || d & (1 << 62) != 0, nx || d & (1 << 59) != 0);
            } else {
                assert!(d & AP_EL0_BIT == 0, "EL0 access to kernel map");
                assert!(read_only || d & AP_RO_BIT != 0 || nx || d & PXN != 0, "writable executable kernel mapping");
            }
        }
    }
    unsafe {
        visit(kernel_root(), 0, false, false);
        extern "C" { static __text_start: u8; }
        let image_va = core::ptr::addr_of!(__text_start) as u64;
        let pa = super::memory::virt_to_phys(image_va);
        unsafe fn leaf(va: u64) -> u64 {
            let mut table = kernel_root();
            for level in 0..4 {
                let d = table_pointer(table).add(((va >> (39 - level * 9)) & 511) as usize).read_volatile();
                assert!(d & VALID != 0, "missing image alias");
                if level == 3 || d & TABLE == 0 { return d; }
                table = d & ADDR_MASK;
            }
            unreachable!()
        }
        let image = leaf(image_va);
        let linear = leaf(super::memory::phys_to_virt(pa) as u64);
        assert_eq!(image & (AP_RO_BIT | PXN | UXN | AP_EL0_BIT), AP_RO_BIT | UXN, "image text permissions");
        assert_eq!(linear & (AP_RO_BIT | PXN | UXN | AP_EL0_BIT), AP_RO_BIT | PXN | UXN, "linear text alias permissions");
        assert_eq!(image & ADDR_MASK, pa, "image translation");
        assert_eq!(linear & ADDR_MASK, pa, "linear alias translation");
        let ttbr0: u64; let ttbr1: u64; let tcr: u64;
        asm!("mrs {}, ttbr0_el1", "mrs {}, ttbr1_el1", "mrs {}, tcr_el1", out(reg) ttbr0, out(reg) ttbr1, out(reg) tcr);
        assert_eq!(ttbr0 & ADDR_MASK, empty_root(), "kernel TTBR0 is not empty");
        assert_eq!(ttbr0 >> 48, 0, "kernel TTBR0 did not use reserved ASID 0");
        assert_eq!(ttbr1 & ADDR_MASK, kernel_root(), "kernel not in TTBR1");
        assert!(tcr & (1 << 37) == 0, "user top-byte ignore enabled");
        let sctlr: u64;
        asm!("mrs {}, sctlr_el1", out(reg) sctlr);
        assert!(sctlr & (1 << 19) != 0, "WXN disabled");
    }
    audit_bootstrap();
    crate::serial::write_line("[MMU] Kernel W^X audit passed (text RX, constants RO/NX, RAM RW/NX)");
}

/// Build TTBR1 and the private physical-text bootstrap map with the MMU off.
/// The assembly handoff enables translation and jumps high. Returns the RAM
/// actually mapped (whole 2 MiB blocks, within the table pool), which is
/// the RAM the kernel may use.
///
/// # Safety
/// Once, on the boot CPU, with the MMU off.
pub unsafe fn early_init(ram: &[(u64, u64)], devices: &[(u64, u64)]) -> heapless::Vec<(u64, u64), 8> {
    let l0 = &mut *core::ptr::addr_of_mut!(L0);
    l0.0[((super::memory::physical_offset() >> 39) & 511) as usize] = super::memory::virt_to_phys(core::ptr::addr_of!(L1) as u64) | TABLE | VALID;

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

    protect_kernel_image();
    build_bootstrap();
    mapped
}

/// Map `[pa, pa + size)` in the TTBR1 physical window as Device memory
/// (2 MiB blocks), e.g. a PCIe ECAM window. Every address space shares
/// the kernel's TTBR1 root, so it appears in all of them. Blocks that
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

/// Make instructions the kernel just wrote (through its high physical window)
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
    super::memory::virt_to_phys(core::ptr::addr_of!(L0) as u64)
}

/// Dedicated high TTBR1 stack region; FRAME_LOCK serializes table mutation.
pub fn map_stack_page(va: u64, pa: u64, fa: &mut crate::memory::frame_allocator::FrameAllocator) {
    assert!((0xffff_e000_0000_0000..0xffff_e080_0000_0000).contains(&va));
    unsafe {
        let mut table = table_pointer(kernel_root());
        for level in 0..3 {
            let entry = table.add(index(va, level));
            let mut descriptor = core::ptr::read_volatile(entry);
            if descriptor & VALID == 0 {
                let frame = fa.allocate_zeroed_frame().expect("stack page table");
                asm!("dsb ishst");
                descriptor = frame | TABLE | VALID;
                core::ptr::write_volatile(entry, descriptor);
            }
            assert_eq!(descriptor & (VALID | TABLE), VALID | TABLE);
            table = table_pointer(descriptor & ADDR_MASK);
        }
        let entry = table.add(index(va, 3));
        assert_eq!(core::ptr::read_volatile(entry) & VALID, 0);
        core::ptr::write_volatile(entry, pa | TABLE | VALID | ATTR_NORMAL | SH_INNER | AF | PXN | UXN);
        asm!("dsb ishst", "isb");
    }
}
pub fn stack_page_mapped(va: u64) -> bool {
    unsafe { walk(kernel_root(), va, false).is_some_and(|entry| core::ptr::read_volatile(entry) & VALID != 0) }
}
pub fn unmap_stack_page(va: u64) -> u64 {
    unsafe {
        let entry = walk(kernel_root(), va, false).expect("stack page table missing");
        let descriptor = core::ptr::read_volatile(entry);
        assert_ne!(descriptor & VALID, 0);
        core::ptr::write_volatile(entry, 0);
        descriptor & ADDR_MASK
    }
}
pub fn detach_stack_tables(va: u64) -> [u64; 2] {
    unsafe {
        let mut table = table_pointer(kernel_root());
        let mut parents = [core::ptr::null_mut(); 3];
        let mut descriptors = [0; 3];
        for level in 0..3 {
            let entry = table.add(index(va, level));
            let descriptor = core::ptr::read_volatile(entry);
            assert_eq!(descriptor & (VALID | TABLE), VALID | TABLE);
            parents[level as usize] = entry;
            descriptors[level as usize] = descriptor;
            table = table_pointer(descriptor & ADDR_MASK);
        }
        let mut retired = [0; 2];
        // Keep the L0 link permanently established for the stack region.
        for level in (1..=2).rev() {
            let pa = descriptors[level] & ADDR_MASK;
            let entries = core::slice::from_raw_parts(table_pointer(pa), ENTRIES);
            if entries.iter().any(|entry| core::ptr::read_volatile(entry) & VALID != 0) { break; }
            core::ptr::write_volatile(parents[level], 0);
            retired[2 - level] = pa;
        }
        retired
    }
}

fn alloc_zeroed_frame() -> Option<u64> {
    let pa = crate::memory::heap::with_frame_allocator(|fa| fa.allocate_zeroed_frame())??;
    Some(pa)
}

fn free_frame(pa: u64) {
    crate::memory::heap::with_frame_allocator(|fa| fa.deallocate_frame(pa));
}

/// Create a private, empty user TTBR0 root. No kernel entries are copied.
/// Returns the table's physical address.
pub fn new_address_space() -> Result<u64, &'static str> {
    let mut root = alloc_zeroed_frame().ok_or("out of memory for a page table")?;
    if root == 0 { // zero is reserved by the portable API for kernel tasks
        root = match alloc_zeroed_frame() { Some(pa) => pa, None => { free_frame(0); return Err("out of memory for a nonzero root"); } };
        free_frame(0);
    }
    let registered = crate::arch::without_interrupts(|| ASIDS.lock().register(root));
    if !registered { free_frame(root); return Err("address-space registry full"); }
    Ok(root)
}

/// Walk `root` to the level-3 entry for `va`, allocating tables if
/// `create`
unsafe fn walk(root: u64, va: u64, create: bool) -> Option<*mut u64> {
    let mut table = table_pointer(root);
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
            table = table_pointer(pa);
        } else if d & TABLE == 0 {
            return None; // a block mapping: not a user page
        } else {
            table = table_pointer(d & ADDR_MASK);
        }
    }
    Some(table.add(index(va, 3)))
}

/// Map one 4 KiB user page `va` → `pa` in address space `root`
pub fn map_user_page(root: u64, va: u64, pa: u64, writable: bool, executable: bool) -> Result<(), &'static str> {
    if !(crate::memory::user::USER_BASE..crate::memory::user::USER_END).contains(&va)
        || va & 4095 != 0 || pa & 4095 != 0 || root == 0 { return Err("invalid user page mapping"); }
    let mut desc = (pa & ADDR_MASK) | ATTR_NORMAL | SH_INNER | AF | PXN | NG | TABLE | VALID;
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
/// Caller owns an offline, unpublished user root. This writes only NX data
/// through its physical backing; final RELRO permissions remain intact.
pub unsafe fn relocate_private_data(root:u64,va:u64,value:u64) -> Result<(), &'static str> {
    if root==0 || va&7!=0 || va<crate::memory::user::USER_BASE || va>=crate::memory::user::USER_END {
        return Err("invalid private relocation target");
    }
    let entry=walk(root,va,false).ok_or("missing private relocation table")?.read_volatile();
    let required=VALID|TABLE|AP_EL0_BIT|UXN;
    if entry&required!=required {return Err("relocation target is not NX user data");}
    super::memory::phys_to_virt((entry&ADDR_MASK)+(va&4095)).cast::<u64>().write_volatile(value);
    Ok(())
}

/// Whether `va` is mapped as a 4 KiB page in the *current* address space
/// (this CPU's TTBR0): None if not, else (user-accessible, writable)
pub fn query_page(va: u64) -> Option<(bool, bool)> {
    let root: u64;
    unsafe {
        let logical = ACTIVE_ROOT[super::cpu_index()].load(core::sync::atomic::Ordering::Acquire);
        root = if logical == 0 { empty_root() } else { logical };
        let mut table = table_pointer(root & ADDR_MASK);
        let mut user = true;
        let mut writable = true;
        for level in 0..4 {
            let desc = table.add(index(va, level)).read_volatile();
            if desc & VALID == 0 || desc & TABLE == 0 { return None; }
            if level == 3 {
                return Some((user && desc & AP_EL0_BIT != 0, writable && desc & AP_RO_BIT == 0));
            }
            user &= desc & (1 << 61) == 0; // APTable[0]: no EL0
            writable &= desc & (1 << 62) == 0; // APTable[1]: read-only
            table = table_pointer(desc & ADDR_MASK);
        }
        None
    }
}

/// Tear down a user address space: free every frame mapped in its user
/// window, the window's tables, and the root. Returns frames freed. The
/// space must not be loaded on any CPU.
pub fn free_address_space(root: u64) -> usize {
    crate::arch::without_interrupts(|| {
        let mut pool = ASIDS.lock();
        if let Some(tag) = pool.tag(root) { invalidate_tag(tag); }
        pool.release(root);
    });
    unsafe fn free_level(t: u64, level: u32, freed: &mut usize) {
        for i in 0..ENTRIES {
            let d = core::ptr::read_volatile(table_pointer(t).add(i));
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
        let top = core::ptr::read_volatile(table_pointer(root).add(USER_L0_SLOT));
        if top & VALID != 0 {
            free_level(top & ADDR_MASK, 1, &mut freed);
            free_frame(top & ADDR_MASK);
            freed += 1;
        }
    }
    free_frame(root);
    freed + 1
}

/// Load a user root with its ASID (0 means reserved empty TTBR0). Normal
/// switches retain translations; generation reuse broadcasts invalidation.
pub fn switch_address_space(cpu: usize, root: u64) {
    use core::sync::atomic::Ordering;
    crate::arch::without_interrupts(|| {
        apply_asid_width();
        super::pan::apply_current();
        if ACTIVE_ROOT[cpu].load(Ordering::Acquire) == root { return; }
        super::predictor::on_switch(cpu, root);
        let mut pool = ASIDS.lock();
        let (tag, flush) = pool.activate(cpu, root);
        if flush { flush_all(); }
        let tag = tag.expect("no ASIDs available for active CPUs");
        let target = (if root == 0 { empty_root() } else { root }) | ((tag as u64) << 48);
        SAVED_TTBR0[cpu].store(target, Ordering::Release);
        let actual = if super::pan::software() { empty_root() } else { target };
        unsafe { asm!("msr ttbr0_el1, {}", "isb", in(reg) actual); }
        ACTIVE_ROOT[cpu].store(root, Ordering::Release);
    });
}

/// Boot self-test only: caller owns this inactive-on-other-CPUs space.
/// Remove one user page while retaining ownership of its physical frame.
pub unsafe fn remove_test_page(root: u64, va: u64) -> u64 {
    let entry = walk(root, va, false).expect("missing test page tables");
    let desc = entry.read_volatile();
    assert!(desc & VALID != 0 && desc & AP_EL0_BIT != 0);
    entry.write_volatile(0);
    invalidate_space(root);
    desc & ADDR_MASK
}

/// Boot test only: caller owns the private user image.
pub unsafe fn test_user_physical(root: u64, va: u64) -> u64 {
    let d = walk(root, va, false).expect("test image page").read_volatile();
    assert!(d & VALID != 0);
    (d & ADDR_MASK) | (va & 4095)
}

fn table_pointer(pa: u64) -> *mut u64 { super::memory::boot_pointer(pa).cast() }
fn empty_root() -> u64 { super::memory::virt_to_phys(core::ptr::addr_of!(EMPTY_L0) as u64) }
/// Called first after each physical trampoline has branched high.
pub fn retire_identity() {
    let root = empty_root();
    SAVED_TTBR0[super::cpu_index()].store(root, core::sync::atomic::Ordering::Release);
    unsafe { asm!("msr ttbr0_el1, {}", "isb", "tlbi aside1is, {}", "dsb ish", "isb", in(reg) root, in(reg) 255u64 << 48); }
}

// ASID 0 always names an empty user translation regime for kernel tasks.
static ASIDS: spin::Mutex<super::asid_pool::Asids<128, { crate::scheduler::MAX_CPUS }>> =
    spin::Mutex::new(super::asid_pool::Asids::new(8));
static ASID_WIDTH: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(8);
static CPU_ASID_BITS: [core::sync::atomic::AtomicU8; crate::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU8::new(8) }; crate::scheduler::MAX_CPUS];

pub fn record_asid_bits(cpu: usize) {
    let mmfr0: u64;
    unsafe { asm!("mrs {}, id_aa64mmfr0_el1", out(reg) mmfr0, options(nomem, nostack)); }
    CPU_ASID_BITS[cpu].store(if (mmfr0 >> 4) & 15 == 2 { 16 } else { 8 }, core::sync::atomic::Ordering::Release);
}
fn apply_asid_width() {
    let bits = ASID_WIDTH.load(core::sync::atomic::Ordering::Acquire);
    let old: u64;
    unsafe { asm!("mrs {}, tcr_el1", out(reg) old); }
    let new = if bits == 16 { old | (1 << 36) } else { old & !(1 << 36) };
    if old != new {
        unsafe { asm!("dsb ish", "msr tcr_el1, {}", "isb", "tlbi vmalle1", "dsb ish", "isb", in(reg) new); }
    }
}
fn flush_all() { unsafe { asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb"); } }
fn invalidate_tag(tag: u16) {
    unsafe { asm!("dsb ishst", "tlbi aside1is, {}", "dsb ish", "isb", in(reg) (tag as u64) << 48); }
}
fn invalidate_space(root: u64) {
    crate::arch::without_interrupts(|| {
        if let Some(tag) = ASIDS.lock().tag(root) { invalidate_tag(tag); }
    });
}
pub fn finalize_asids() {
    use core::sync::atomic::Ordering;
    let admitted = crate::security::freeze();
    let bits = (0..crate::scheduler::MAX_CPUS).filter(|cpu| admitted & (1u64 << cpu) != 0)
        .map(|cpu| CPU_ASID_BITS[cpu].load(Ordering::Acquire)).min().unwrap_or(8);
    crate::arch::without_interrupts(|| {
        ASIDS.lock().configure(bits);
        ASID_WIDTH.store(bits, Ordering::Release);
        apply_asid_width(); flush_all();
    });
    crate::serial::write_str("[MMU] kernel at ");
    crate::kptr::write(core::ptr::addr_of!(L0) as u64);
    crate::serial::write_str(" (TTBR1), user-only TTBR0 with "); crate::serial::write_dec(bits as u64);
    crate::serial::write_line("-bit ASIDs; user nG, TBI0 off");
}

/// Boot regression: force 8-bit tag rollover even on a 16-bit CPU, before
/// any user tasks exist. Real TTBR0/ASID changes must not retain another
/// private image's translations, including when returning to empty TTBR0.
pub fn asid_self_test() {
    let restore_bits = ASID_WIDTH.load(core::sync::atomic::Ordering::Acquire);
    crate::arch::without_interrupts(|| { ASIDS.lock().configure(8); flush_all(); });
    let mut complete = true;
    for _ in 0..300 {
        let code = 0xd65f03c0u32.to_le_bytes();
        let a = match crate::memory::user::load_program(&code) { Ok(a) => a, Err(_) => { complete = false; break; } };
        let b = match crate::memory::user::load_program(&code) {
            Ok(b) => b,
            Err(_) => { crate::memory::user::release(a.space, a.slot); complete = false; break; }
        };
        crate::arch::without_interrupts(|| {
            let mut byte = 0u8;
            switch_address_space(0, a.space);
            assert_eq!(unsafe { super::uaccess::copy_from_user(&mut byte, a.entry, 1) }, 0);
            assert_eq!(byte, code[0]);
            switch_address_space(0, b.space);
            assert_eq!(unsafe { super::uaccess::copy_from_user(&mut byte, a.entry, 1) }, 1, "A's translation leaked into B");
            assert_eq!(unsafe { super::uaccess::copy_from_user(&mut byte, b.entry, 1) }, 0);
            switch_address_space(0, a.space);
            assert_eq!(unsafe { super::uaccess::copy_from_user(&mut byte, b.entry, 1) }, 1, "B's translation leaked into A");
            switch_address_space(0, 0);
            assert_eq!(unsafe { super::uaccess::copy_from_user(&mut byte, a.entry, 1) }, 1, "user translation leaked into empty TTBR0");
        });
        crate::memory::user::release(a.space, a.slot);
        crate::memory::user::release(b.space, b.slot);
    }
    let generation = crate::arch::without_interrupts(|| {
        let mut pool = ASIDS.lock(); let generation = pool.generation();
        pool.configure(restore_bits); flush_all(); generation
    });
    if complete {
        assert!(generation > 1, "ASID rollover was not exercised");
        crate::serial::write_line("[MMU] ASID isolation/rollover passed (600 private images, empty TTBR0, broadcast reuse)");
    } else { crate::serial::write_line("[MMU] ASID probes skipped: out of memory"); }
}

fn boot_root() -> u64 { super::memory::virt_to_phys(core::ptr::addr_of!(BOOT_L0) as u64) }
/// Only physical text is needed between enabling translation and branching
/// high. Its private non-global leaves use ASID 255, which user allocation
/// always reserves even when a late secondary starts after userspace.
unsafe fn build_bootstrap() {
    extern "C" { static __text_start: u8; static __text_end: u8; }
    let start = core::ptr::addr_of!(__text_start) as u64;
    let end = core::ptr::addr_of!(__text_end) as u64;
    let l0 = core::ptr::addr_of_mut!(BOOT_L0).cast::<u64>();
    let l1 = core::ptr::addr_of_mut!(BOOT_L1).cast::<u64>();
    l0.write(l1 as u64 | TABLE | VALID);
    let first_gib = start / GIB;
    let mut l3_used = 0;
    for block in ((start & !(MIB2 - 1))..end).step_by(MIB2 as usize) {
        let gib_slot = (block / GIB - first_gib) as usize;
        assert!(gib_slot < 2 && l3_used < 16, "bootstrap text exceeds table pool");
        let l2 = core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(BOOT_L2))[gib_slot]).cast::<u64>();
        l1.add((block / GIB) as usize).write(l2 as u64 | TABLE | VALID);
        let l3 = core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(BOOT_L3))[l3_used]).cast::<u64>();
        l3_used += 1;
        l2.add(((block % GIB) / MIB2) as usize).write(l3 as u64 | TABLE | VALID);
        for page in (block.max(start)..(block + MIB2).min(end)).step_by(4096) {
            l3.add(((page >> 12) & 511) as usize).write(page | ATTR_NORMAL | SH_INNER | AF | AP_RO_BIT | UXN | NG | TABLE | VALID);
        }
    }
}

fn audit_bootstrap() {
    unsafe fn walk(table: u64, level: u32, text: &core::ops::Range<u64>) {
        for i in 0..ENTRIES {
            let d = table_pointer(table).add(i).read_volatile();
            if d & VALID == 0 { continue; }
            assert!(d & TABLE != 0, "bootstrap block mapping");
            if level < 3 { walk(d & ADDR_MASK, level + 1, text); }
            else {
                assert!(text.contains(&(d & ADDR_MASK)), "bootstrap mapped non-text RAM");
                assert!(d & (AP_RO_BIT | UXN | NG) == AP_RO_BIT | UXN | NG);
                assert!(d & (AP_EL0_BIT | PXN) == 0);
            }
        }
    }
    extern "C" { static __text_start: u8; static __text_end: u8; }
    let text = super::memory::virt_to_phys(core::ptr::addr_of!(__text_start) as u64)
        ..super::memory::virt_to_phys(core::ptr::addr_of!(__text_end) as u64);
    unsafe { walk(boot_root(), 0, &text); }
}

static SAVED_TTBR0: [core::sync::atomic::AtomicU64; crate::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::scheduler::MAX_CPUS];
pub fn saved_user_ttbr0() -> u64 { SAVED_TTBR0[super::cpu_index()].load(core::sync::atomic::Ordering::Acquire) }
pub fn empty_user_root() -> u64 { empty_root() }

/// Boot test only: a private inactive-on-other-CPUs code page becomes XOM.
pub unsafe fn make_test_execute_only(root: u64, va: u64) {
    let entry = walk(root, va, false).expect("execute-only test page");
    let d = entry.read_volatile();
    assert!(d & (VALID | AP_RO_BIT | PXN) == VALID | AP_RO_BIT | PXN && d & UXN == 0);
    entry.write_volatile(d & !AP_EL0_BIT);
    invalidate_space(root);
}
