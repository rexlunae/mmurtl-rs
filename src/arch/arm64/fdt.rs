//! Flattened device tree (DTB) parser — just enough to discover the
//! machine: RAM and reserved memory, CPUs and how to start them (PSCI or
//! spin-table), the interrupt controller (GICv2/v3 or the BCM2836 local
//! controller of Raspberry Pi 2/3), the PL011 UART, the generic timer's
//! interrupt, the PCIe host bridge, and the virtio-mmio transports.
//!
//! It runs with the MMU off (before the kernel knows where RAM is), so it
//! allocates nothing and reads only aligned big-endian words; the
//! `+strict-align` target keeps the compiler from emitting unaligned
//! accesses, which Device memory would fault on.
//!
//! Device addresses are translated to CPU addresses through each
//! ancestor's `ranges` (e.g. the Pi's `soc` bus maps 0x7e000000 to
//! 0x3f000000).

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// CPUs we track (GICv2 can target only 8; GICv3 routes by affinity)
pub const MAX_CPUS: usize = 64;
pub const MAX_RAM: usize = 4;
pub const MAX_VIRTIO: usize = 32;
pub const MAX_RESERVED: usize = 8;

/// PSCI calling convention
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PsciConduit {
    None,
    Hvc,
    Smc,
}

/// Interrupt controller
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IrqChip {
    /// ARM GIC, architecture version 2 or 3
    Gic(u8),
    /// Broadcom BCM2836 per-core controller + BCM2835 "armctrl" (Pi 2/3)
    Bcm2836,
}

/// What the kernel learns from the device tree
pub struct MachineInfo {
    pub ram: [(u64, u64); MAX_RAM], // (base, size)
    pub ram_count: usize,
    /// /memreserve/ entries: firmware structures, spin tables, ...
    pub reserved: [(u64, u64); MAX_RESERVED],
    pub reserved_count: usize,
    pub cpus: [u64; MAX_CPUS], // MPIDR affinity values
    /// Per CPU: spin-table release address (0 = not spin-table)
    pub cpu_release: [u64; MAX_CPUS],
    pub cpu_count: usize,
    pub psci: PsciConduit,
    pub irqchip: IrqChip,
    pub gicd: u64,
    /// GICv2 CPU interface base
    pub gicc: u64,
    /// GICv3 redistributor region (first region)
    pub gicr: u64,
    pub gicr_size: u64,
    /// BCM2836 per-core controller and BCM2835 armctrl bases
    pub local_intc: u64,
    pub armctrl: u64,
    pub uart: u64,
    /// A PL011 was found (the first enabled one wins)
    pub uart_found: bool,
    /// UART interrupt: GIC INTID, or armctrl number (bank * 32 + bit)
    pub uart_irq: u32,
    /// Virtual timer interrupt: GIC INTID, or BCM2836 local source number
    pub timer_irq: u32,
    pub virtio: [u64; MAX_VIRTIO],
    pub virtio_count: usize,
    /// PCIe host bridge (generic ECAM), if present
    pub pci: Option<PciHost>,
}

/// A generic ECAM PCIe host bridge
#[derive(Clone, Copy, Default)]
pub struct PciHost {
    pub ecam: u64,
    pub ecam_size: u64,
    pub bus_start: u8,
    pub bus_end: u8,
    /// I/O space window: CPU address, PCI address, size
    pub io: (u64, u64, u64),
    /// 32-bit memory window: CPU address, PCI address, size
    pub mem32: (u64, u64, u64),
}

impl MachineInfo {
    /// QEMU `virt` defaults, used for anything the tree doesn't say
    const fn qemu_virt_defaults() -> Self {
        Self {
            ram: [(0, 0); MAX_RAM],
            ram_count: 0,
            reserved: [(0, 0); MAX_RESERVED],
            reserved_count: 0,
            cpus: [0; MAX_CPUS],
            cpu_release: [0; MAX_CPUS],
            cpu_count: 0,
            psci: PsciConduit::None,
            irqchip: IrqChip::Gic(2),
            gicd: 0x0800_0000,
            gicc: 0x0801_0000,
            gicr: 0,
            gicr_size: 0,
            local_intc: 0,
            armctrl: 0,
            uart: 0x0900_0000,
            uart_found: false,
            uart_irq: 33,
            timer_irq: 27,
            virtio: [0; MAX_VIRTIO],
            virtio_count: 0,
            pci: None,
        }
    }
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Read a `cells`-cell big-endian number
fn read_cells(b: &[u8], off: usize, cells: u32) -> u64 {
    let mut v = 0u64;
    for i in 0..cells as usize {
        v = (v << 32) | be32(b, off + 4 * i) as u64;
    }
    v
}

fn cstr(b: &[u8], off: usize) -> &[u8] {
    let mut end = off;
    while end < b.len() && b[end] != 0 {
        end += 1;
    }
    &b[off..end]
}

/// Whether a NUL-separated string list contains `want`
fn strlist_contains(list: &[u8], want: &[u8]) -> bool {
    list.split(|&c| c == 0).any(|s| s == want)
}

/// Whether `addr` holds a device tree header
pub fn is_valid(addr: u64) -> bool {
    if addr == 0 || addr % 4 != 0 {
        return false;
    }
    let hdr = unsafe { core::slice::from_raw_parts(addr as *const u8, 8) };
    be32(hdr, 0) == FDT_MAGIC
}

/// Total size of the blob at `addr`
pub fn total_size(addr: u64) -> u64 {
    let hdr = unsafe { core::slice::from_raw_parts(addr as *const u8, 8) };
    be32(hdr, 4) as u64
}

/// Per-node state collected while walking
#[derive(Clone, Copy)]
struct Node {
    /// #address-cells / #size-cells this node declares for its children
    child_addr_cells: u32,
    child_size_cells: u32,
    is_cpu: bool,
    is_memory: bool,
    is_gic: bool,
    is_gicv3: bool,
    is_pcie: bool,
    is_uart: bool,
    is_virtio: bool,
    is_timer: bool,
    is_psci: bool,
    is_local_intc: bool,
    is_armctrl: bool,
    spin_table: bool,
    /// `ranges` present (possibly empty = identity) / its location
    has_ranges: bool,
    ranges_off: usize,
    ranges_len: usize,
    busrange_off: usize,
    busrange_len: usize,
    /// PSCI "method" (resolved when the node closes: property order varies)
    method: PsciConduit,
    release_addr: u64,
    reg_off: usize,
    reg_len: usize,
    irq_off: usize,
    irq_len: usize,
    status_ok: bool,
}

impl Node {
    const fn new() -> Self {
        Self {
            child_addr_cells: 2,
            child_size_cells: 1,
            is_cpu: false,
            is_memory: false,
            is_gic: false,
            is_gicv3: false,
            is_pcie: false,
            is_uart: false,
            is_virtio: false,
            is_timer: false,
            is_psci: false,
            is_local_intc: false,
            is_armctrl: false,
            spin_table: false,
            has_ranges: false,
            ranges_off: 0,
            ranges_len: 0,
            busrange_off: 0,
            busrange_len: 0,
            method: PsciConduit::None,
            release_addr: 0,
            reg_off: 0,
            reg_len: 0,
            irq_off: 0,
            irq_len: 0,
            status_ok: true,
        }
    }
}

/// Decode one interrupt specifier of `cells` cells at `off`:
/// GIC (3 cells: type, number, flags) → INTID; Broadcom (2 cells: bank or
/// source, number) → bank * 32 + number.
fn decode_irq(b: &[u8], off: usize, cells: usize) -> u32 {
    match cells {
        3 => {
            let (kind, num) = (be32(b, off), be32(b, off + 4));
            if kind == 1 { num + 16 } else { num + 32 } // PPI : SPI
        }
        2 => be32(b, off) * 32 + be32(b, off + 4),
        _ => be32(b, off),
    }
}

/// Translate a bus address in the child space of `stack[..depth]`'s last
/// node up to a CPU address, through each ancestor's `ranges`
fn translate(b: &[u8], stack: &[Node], depth: usize, mut addr: u64) -> u64 {
    // stack[depth-1] is the node owning the address space `addr` is in
    let mut k = depth;
    while k > 1 {
        let bus = &stack[k - 1];
        if !bus.has_ranges {
            break; // no translation defined: treat as identity
        }
        if bus.ranges_len > 0 {
            let (cac, csc) = (bus.child_addr_cells, bus.child_size_cells);
            let pac = stack[k - 2].child_addr_cells;
            let entry = 4 * (cac + pac + csc) as usize;
            let mut o = bus.ranges_off;
            while o + entry <= bus.ranges_off + bus.ranges_len {
                let child = read_cells(b, o, cac);
                let parent = read_cells(b, o + 4 * cac as usize, pac);
                let size = read_cells(b, o + 4 * (cac + pac) as usize, csc);
                if addr >= child && addr - child < size {
                    addr = parent + (addr - child);
                    break;
                }
                o += entry;
            }
        }
        k -= 1;
    }
    addr
}

/// Parse the blob at `addr`
pub fn parse(addr: u64) -> MachineInfo {
    let mut info = MachineInfo::qemu_virt_defaults();
    let size = total_size(addr) as usize;
    let b = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    let off_struct = be32(b, 8) as usize;
    let off_strings = be32(b, 12) as usize;
    let off_rsvmap = be32(b, 16) as usize;

    // Memory reservation block: (address, size) pairs until (0, 0)
    let mut o = off_rsvmap;
    while o + 16 <= size && info.reserved_count < MAX_RESERVED {
        let (base, len) = (read_cells(b, o, 2), read_cells(b, o + 8, 2));
        if base == 0 && len == 0 {
            break;
        }
        info.reserved[info.reserved_count] = (base, len);
        info.reserved_count += 1;
        o += 16;
    }

    const DEPTH: usize = 16;
    let mut stack = [Node::new(); DEPTH];
    let mut depth = 0usize; // stack[depth-1] is the current node
    let mut p = off_struct;
    let mut have_gic = false;

    loop {
        let token = be32(b, p);
        p += 4;
        match token {
            FDT_BEGIN_NODE => {
                let name = cstr(b, p);
                p = (p + name.len() + 1 + 3) & !3;
                if depth < DEPTH {
                    let mut n = Node::new();
                    n.is_memory = name.starts_with(b"memory");
                    n.is_cpu = name.starts_with(b"cpu@");
                    stack[depth] = n;
                }
                depth += 1;
            }
            FDT_END_NODE => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                if depth >= DEPTH {
                    continue;
                }
                let n = stack[depth];
                let parent = if depth > 0 { stack[depth - 1] } else { Node::new() };
                let (ac, sc) = (parent.child_addr_cells, parent.child_size_cells);
                let entry = 4 * (ac + sc) as usize;
                // reg[i] of this node, as a CPU address
                let reg_addr = |i: usize| translate(b, &stack, depth, read_cells(b, n.reg_off + i * entry, ac));
                let reg_size = |i: usize| read_cells(b, n.reg_off + i * entry + 4 * ac as usize, sc);
                if !n.status_ok {
                    continue;
                }
                if n.is_psci && n.method != PsciConduit::None {
                    info.psci = n.method;
                }
                if n.is_memory && n.reg_len >= entry {
                    let mut i = 0;
                    while (i + 1) * entry <= n.reg_len && info.ram_count < MAX_RAM {
                        let (base, len) = (reg_addr(i), reg_size(i));
                        if len > 0 {
                            info.ram[info.ram_count] = (base, len);
                            info.ram_count += 1;
                        }
                        i += 1;
                    }
                } else if n.is_cpu && n.reg_len >= 4 * ac as usize {
                    if info.cpu_count < MAX_CPUS {
                        info.cpus[info.cpu_count] = read_cells(b, n.reg_off, ac);
                        info.cpu_release[info.cpu_count] = if n.spin_table { n.release_addr } else { 0 };
                        info.cpu_count += 1;
                    }
                } else if n.is_pcie && n.reg_len >= entry {
                    let mut host = PciHost {
                        ecam: reg_addr(0),
                        ecam_size: reg_size(0),
                        bus_start: 0,
                        bus_end: 255,
                        ..PciHost::default()
                    };
                    if n.busrange_len >= 8 {
                        host.bus_start = be32(b, n.busrange_off) as u8;
                        host.bus_end = be32(b, n.busrange_off + 4) as u8;
                    }
                    // ranges: <pci-addr (3 cells)> <cpu-addr (parent cells)> <size>
                    let (cac, csc) = (n.child_addr_cells, n.child_size_cells);
                    let rentry = 4 * (cac + ac + csc) as usize;
                    let mut o = n.ranges_off;
                    while cac == 3 && o + rentry <= n.ranges_off + n.ranges_len {
                        let space = (be32(b, o) >> 24) & 0x3;
                        let pci = read_cells(b, o + 4, 2);
                        let cpu = read_cells(b, o + 12, ac);
                        let size = read_cells(b, o + 12 + 4 * ac as usize, csc);
                        match space {
                            1 => host.io = (cpu, pci, size),
                            2 => host.mem32 = (cpu, pci, size),
                            _ => {}
                        }
                        o += rentry;
                    }
                    info.pci = Some(host);
                } else if n.is_gicv3 && n.reg_len >= 2 * entry {
                    // reg = <GICD>, <GICR region>, [GICC, GICH, GICV]
                    info.irqchip = IrqChip::Gic(3);
                    have_gic = true;
                    info.gicd = reg_addr(0);
                    info.gicr = reg_addr(1);
                    info.gicr_size = reg_size(1);
                } else if n.is_gic && n.reg_len >= 2 * entry {
                    info.irqchip = IrqChip::Gic(2);
                    have_gic = true;
                    info.gicd = reg_addr(0);
                    info.gicc = reg_addr(1);
                } else if n.is_local_intc && n.reg_len >= entry {
                    info.local_intc = reg_addr(0);
                    if !have_gic {
                        info.irqchip = IrqChip::Bcm2836;
                    }
                } else if n.is_armctrl && n.reg_len >= entry {
                    info.armctrl = reg_addr(0);
                } else if n.is_uart && n.reg_len >= entry && !info.uart_found {
                    info.uart = reg_addr(0);
                    if n.irq_len >= 8 {
                        info.uart_irq = decode_irq(b, n.irq_off, n.irq_len / 4);
                    }
                    info.uart_found = true;
                } else if n.is_virtio && n.reg_len >= entry && info.virtio_count < MAX_VIRTIO {
                    info.virtio[info.virtio_count] = reg_addr(0);
                    info.virtio_count += 1;
                } else if n.is_timer && n.irq_len >= 16 && n.irq_len % 16 == 0 {
                    // interrupts = <secure-phys>, <non-secure-phys>, <virtual>, <hyp>
                    let cells = n.irq_len / 16;
                    info.timer_irq = decode_irq(b, n.irq_off + 2 * 4 * cells, cells);
                }
            }
            FDT_PROP => {
                let len = be32(b, p) as usize;
                let nameoff = be32(b, p + 4) as usize;
                let val = p + 8;
                p = (val + len + 3) & !3;
                if depth == 0 || depth > DEPTH {
                    continue;
                }
                let name = cstr(b, off_strings + nameoff);
                let n = &mut stack[depth - 1];
                let v = &b[val..val + len];
                match name {
                    b"#address-cells" if len == 4 => n.child_addr_cells = be32(b, val),
                    b"#size-cells" if len == 4 => n.child_size_cells = be32(b, val),
                    b"reg" => {
                        n.reg_off = val;
                        n.reg_len = len;
                    }
                    b"ranges" => {
                        n.has_ranges = true;
                        n.ranges_off = val;
                        n.ranges_len = len;
                    }
                    b"bus-range" => {
                        n.busrange_off = val;
                        n.busrange_len = len;
                    }
                    b"interrupts" => {
                        n.irq_off = val;
                        n.irq_len = len;
                    }
                    b"status" => {
                        let s = cstr(b, val);
                        n.status_ok = s == b"okay" || s == b"ok";
                    }
                    b"device_type" => {
                        let s = cstr(b, val);
                        n.is_cpu |= s == b"cpu";
                        n.is_memory |= s == b"memory";
                    }
                    b"method" => {
                        let s = cstr(b, val);
                        n.method = match s {
                            b"hvc" => PsciConduit::Hvc,
                            b"smc" => PsciConduit::Smc,
                            _ => PsciConduit::None,
                        };
                    }
                    b"enable-method" => n.spin_table = cstr(b, val) == b"spin-table",
                    b"cpu-release-addr" if len == 8 => n.release_addr = read_cells(b, val, 2),
                    b"cpu-release-addr" if len == 4 => n.release_addr = be32(b, val) as u64,
                    b"compatible" => {
                        n.is_gic |= strlist_contains(v, b"arm,cortex-a15-gic")
                            || strlist_contains(v, b"arm,gic-400");
                        n.is_uart |= strlist_contains(v, b"arm,pl011");
                        n.is_virtio |= strlist_contains(v, b"virtio,mmio");
                        n.is_timer |= strlist_contains(v, b"arm,armv8-timer")
                            || strlist_contains(v, b"arm,armv7-timer");
                        let psci = strlist_contains(v, b"arm,psci-1.0")
                            || strlist_contains(v, b"arm,psci-0.2")
                            || strlist_contains(v, b"arm,psci");
                        n.is_psci |= psci;
                        n.is_gicv3 |= strlist_contains(v, b"arm,gic-v3");
                        n.is_pcie |= strlist_contains(v, b"pci-host-ecam-generic");
                        n.is_local_intc |= strlist_contains(v, b"brcm,bcm2836-l1-intc");
                        n.is_armctrl |= strlist_contains(v, b"brcm,bcm2836-armctrl-ic");
                    }
                    _ => {}
                }
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => break, // malformed
        }
    }

    info
}
