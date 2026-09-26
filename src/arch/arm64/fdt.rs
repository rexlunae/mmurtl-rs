//! Flattened device tree (DTB) parser — just enough to discover the
//! machine: RAM, CPUs, the PSCI conduit, the GIC, the PL011 UART, the
//! generic timer's interrupt, and the virtio-mmio transports.
//!
//! No allocation (it runs before the heap exists) and byte-wise big-endian
//! reads only, so it never performs an unaligned access.

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

/// PSCI calling convention
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PsciConduit {
    None,
    Hvc,
    Smc,
}

/// What the kernel learns from the device tree
pub struct MachineInfo {
    pub ram: [(u64, u64); MAX_RAM], // (base, size)
    pub ram_count: usize,
    pub cpus: [u64; MAX_CPUS], // MPIDR affinity values
    pub cpu_count: usize,
    pub psci: PsciConduit,
    /// GIC architecture version (2 or 3)
    pub gic_version: u8,
    pub gicd: u64,
    /// GICv2 CPU interface base
    pub gicc: u64,
    /// GICv3 redistributor region (first region)
    pub gicr: u64,
    pub gicr_size: u64,
    pub uart: u64,
    pub uart_irq: u32, // GIC INTID
    pub timer_irq: u32, // virtual timer GIC INTID
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
            cpus: [0; MAX_CPUS],
            cpu_count: 0,
            psci: PsciConduit::None,
            gic_version: 2,
            gicd: 0x0800_0000,
            gicc: 0x0801_0000,
            gicr: 0,
            gicr_size: 0,
            uart: 0x0900_0000,
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
    ranges_off: usize,
    ranges_len: usize,
    busrange_off: usize,
    busrange_len: usize,
    is_uart: bool,
    is_virtio: bool,
    is_timer: bool,
    is_psci: bool,
    /// PSCI "method" (resolved when the node closes: property order varies)
    method: PsciConduit,
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
            ranges_off: 0,
            ranges_len: 0,
            busrange_off: 0,
            busrange_len: 0,
            is_uart: false,
            is_virtio: false,
            is_timer: false,
            is_psci: false,
            method: PsciConduit::None,
            reg_off: 0,
            reg_len: 0,
            irq_off: 0,
            irq_len: 0,
            status_ok: true,
        }
    }
}

/// Decode a GIC `interrupts` triple (type, number, flags) into an INTID
fn gic_intid(b: &[u8], off: usize) -> u32 {
    let kind = be32(b, off);
    let num = be32(b, off + 4);
    if kind == 1 { num + 16 } else { num + 32 } // PPI : SPI
}

/// Parse the blob at `addr`
pub fn parse(addr: u64) -> MachineInfo {
    let mut info = MachineInfo::qemu_virt_defaults();
    let size = total_size(addr) as usize;
    let b = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    let off_struct = be32(b, 8) as usize;
    let off_strings = be32(b, 12) as usize;

    const DEPTH: usize = 16;
    let mut stack = [Node::new(); DEPTH];
    let mut depth = 0usize; // stack[depth-1] is the current node
    let mut p = off_struct;

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
                if !n.status_ok {
                    continue;
                }
                if n.is_psci && n.method != PsciConduit::None {
                    info.psci = n.method;
                }
                if n.is_memory && n.reg_len >= entry {
                    let mut o = n.reg_off;
                    while o + entry <= n.reg_off + n.reg_len && info.ram_count < MAX_RAM {
                        let base = read_cells(b, o, ac);
                        let len = read_cells(b, o + 4 * ac as usize, sc);
                        if len > 0 {
                            info.ram[info.ram_count] = (base, len);
                            info.ram_count += 1;
                        }
                        o += entry;
                    }
                } else if n.is_cpu && n.reg_len >= 4 * ac as usize {
                    if info.cpu_count < MAX_CPUS {
                        info.cpus[info.cpu_count] = read_cells(b, n.reg_off, ac);
                        info.cpu_count += 1;
                    }
                } else if n.is_pcie && n.reg_len >= entry {
                    let mut host = PciHost {
                        ecam: read_cells(b, n.reg_off, ac),
                        ecam_size: read_cells(b, n.reg_off + 4 * ac as usize, sc),
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
                    info.gic_version = 3;
                    info.gicd = read_cells(b, n.reg_off, ac);
                    info.gicr = read_cells(b, n.reg_off + entry, ac);
                    info.gicr_size = read_cells(b, n.reg_off + entry + 4 * ac as usize, sc);
                } else if n.is_gic && n.reg_len >= 2 * entry {
                    info.gic_version = 2;
                    info.gicd = read_cells(b, n.reg_off, ac);
                    info.gicc = read_cells(b, n.reg_off + entry, ac);
                } else if n.is_uart && n.reg_len >= entry {
                    info.uart = read_cells(b, n.reg_off, ac);
                    if n.irq_len >= 12 {
                        info.uart_irq = gic_intid(b, n.irq_off);
                    }
                } else if n.is_virtio && n.reg_len >= entry && info.virtio_count < MAX_VIRTIO {
                    info.virtio[info.virtio_count] = read_cells(b, n.reg_off, ac);
                    info.virtio_count += 1;
                } else if n.is_timer && n.irq_len >= 36 {
                    // interrupts = <secure-phys>, <non-secure-phys>, <virtual>, <hyp>
                    info.timer_irq = gic_intid(b, n.irq_off + 24);
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
                    b"compatible" => {
                        n.is_gic |= strlist_contains(v, b"arm,cortex-a15-gic")
                            || strlist_contains(v, b"arm,gic-400");
                        n.is_uart |= strlist_contains(v, b"arm,pl011");
                        n.is_virtio |= strlist_contains(v, b"virtio,mmio");
                        n.is_timer |= strlist_contains(v, b"arm,armv8-timer");
                        let psci = strlist_contains(v, b"arm,psci-1.0")
                            || strlist_contains(v, b"arm,psci-0.2")
                            || strlist_contains(v, b"arm,psci");
                        n.is_psci |= psci;
                        n.is_gicv3 |= strlist_contains(v, b"arm,gic-v3");
                        n.is_pcie |= strlist_contains(v, b"pci-host-ecam-generic");
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
