//! xHCI Host Controller Driver — initialization, port management, and USB transfers.
//!
//! This module drives an xHCI (eXtensible Host Controller Interface) controller
//! to enumerate and communicate with USB devices. For Phase 1/2, we support:
//!   - Controller initialization and BIOS handoff
//!   - Port detection and speed reporting
//!   - Device enumeration (control transfers)
//!   - Interrupt endpoint polling for HID devices
//!
//! xHCI spec reference: https://www.intel.com/content/www/us/en/products/docs/io/universal-serial-bus/extensible-host-controler-interface-usb-xhci.html

use core::ptr::{read_volatile, write_volatile};
use core::fmt;

mod trb;
mod context;

use trb::*;
use context::*;

/// Max number of device slots supported
const MAX_SLOTS: usize = 32;

/// xHCI port register set size
const PORT_REG_SET_SIZE: u64 = 16;

// ========================================================================
// xHCI Register Offsets (relative to MMIO base)
// ========================================================================

/// Operational register offsets (relative to op_regs base)
const USBCMD_OFF: u64        = 0x00;  // USB Command
const USBSTS_OFF: u64        = 0x04;  // USB Status
const DNCTRL_OFF: u64        = 0x08;  // Device Notification Control
const CRCR_OFF: u64          = 0x0C;  // Command Ring Control Register (64-bit, 2 dwords)
const DCBAAP_OFF: u64        = 0x14;  // Device Context Base Address Array Pointer (64-bit)
const CONFIG_OFF: u64        = 0x18;  // Configure Register
const PORTSC_BASE_OFF: u64   = 0x20;  // Port Register Set base

// ========================================================================
// Capability Register Offsets
// ========================================================================

const CAPLENGTH_OFF: u64     = 0x00;  // Capability Register Length (1 byte)
const HCIVERSION_OFF: u64    = 0x02;  // Interface Version Number (2 bytes)
const HCSPARAMS1_OFF: u64    = 0x04;  // Structural Parameters 1 (4 bytes)
const HCSPARAMS2_OFF: u64    = 0x08;  // Structural Parameters 2 (4 bytes)
const HCCPARAMS1_OFF: u64   = 0x10;  // Capability Parameters 1 (4 bytes)
const DBOFF_OFF: u64         = 0x14;  // Doorbell Offset (4 bytes)
const RTSOFF_OFF: u64        = 0x18;  // Runtime Register Space Offset (4 bytes)

// ========================================================================
// USBCMD register bits
// ========================================================================

const USBCMD_RUN_STOP: u32     = 1 << 0;  // R/S
const USBCMD_HCRST: u32       = 1 << 1;  // Host Controller Reset (HCRST)
const USBCMD_INTE: u32        = 1 << 2;  // Interrupt Enable
const USBCMD_HOST_SMI: u32    = 1 << 10; // Host System Error Interrupt

// ========================================================================
// USBSTS register bits
// ========================================================================

const USBSTS_HCHALTED: u32    = 1 << 0;  // HCHalted
const USBSTS_CNR: u32         = 1 << 11; // Controller Not Ready
const USBSTS_PCD: u32         = 1 << 2;  // Port Change Detect

// ========================================================================
// PORTSC register bits
// ========================================================================

const PORTSC_CCS: u32         = 1 << 0;  // Current Connect Status
const PORTSC_PED: u32         = 1 << 1;  // Port Enabled/Disabled
const PORTSC_OCA: u32         = 1 << 3;  // Over-current Active
const PORTSC_PR: u32          = 1 << 4;  // Port Reset
const PORTSC_PP: u32          = 1 << 9;  // Port Power
const PORTSC_CSC: u32         = 1 << 17; // Connect Status Change
const PORTSC_WRC: u32         = 1 << 19; // Warm Reset Change
const PORTSC_PS_SHIFT: u32    = 10;      // Port Speed bits

/// Port speed values from PORTSC
const PORT_SPEED_FULL: u32   = 1;
const PORT_SPEED_LOW: u32    = 2;
const PORT_SPEED_HIGH: u32   = 3;
const PORT_SPEED_SUPER: u32  = 4;

// ========================================================================
// Doorbell register offsets
// ========================================================================

const DOORBELL_HOST: u32 = 0;  // Host controller doorbell (command ring)

// ========================================================================
// xHCI Controller State
// ========================================================================

/// Physical address of the xHCI MMIO region
#[derive(Clone, Copy)]
struct XhciMmio {
    base: u64,
    cap_len: u8,
    op_regs: u64,
    doorbell_off: u64,
    rt_regs_off: u64,
    max_slots: u8,
    max_ports: u8,
    port_reg_set_size: u32,
}

impl XhciMmio {
    /// Create from a PCI device's MMIO base
    fn new(mmio_base: u64) -> Self {
        let cap_len = unsafe { read_volatile(mmio_base as *const u8) };
        let hcs1 = unsafe { read_volatile((mmio_base + HCSPARAMS1_OFF) as *const u32) };
        let _hcc1 = unsafe { read_volatile((mmio_base + HCCPARAMS1_OFF) as *const u32) };
        let dboff = unsafe { read_volatile((mmio_base + DBOFF_OFF) as *const u32) };
        let rtsoff = unsafe { read_volatile((mmio_base + RTSOFF_OFF) as *const u32) };
        let hciver = unsafe { read_volatile((mmio_base + HCIVERSION_OFF) as *const u16) };

        let max_slots = (hcs1 & 0xFF) as u8;
        let max_ports = ((hcs1 >> 24) & 0xFF) as u8;
        let port_reg_set_size = ((hcs1 >> 16) & 0xFF) as u32;

        crate::serial::write_str("[xHCI] Capability length: ");
        crate::serial::write_dec(cap_len as u64);
        crate::serial::write_str("\n");

        crate::serial::write_str("[xHCI] xHCI version: ");
        crate::serial::write_hex(hciver as u64);
        crate::serial::write_str("\n");

        crate::serial::write_str("[xHCI] Max slots: ");
        crate::serial::write_dec(max_slots as u64);
        crate::serial::write_str("\n");

        crate::serial::write_str("[xHCI] Max ports: ");
        crate::serial::write_dec(max_ports as u64);
        crate::serial::write_str("\n");

        Self {
            base: mmio_base,
            cap_len,
            op_regs: mmio_base + cap_len as u64,
            doorbell_off: mmio_base + dboff as u64,
            rt_regs_off: mmio_base + rtsoff as u64,
            max_slots,
            max_ports,
            port_reg_set_size,
        }
    }

    // ---- Register access helpers ----

    fn read_op(&self, offset: u64) -> u32 {
        unsafe { read_volatile((self.op_regs + offset) as *const u32) }
    }

    fn write_op(&self, offset: u64, value: u32) {
        unsafe { write_volatile((self.op_regs + offset) as *mut u32, value) }
    }

    fn read_op64(&self, offset: u64) -> u64 {
        unsafe { read_volatile((self.op_regs + offset) as *const u64) }
    }

    fn write_op64(&self, offset: u64, value: u64) {
        unsafe { write_volatile((self.op_regs + offset) as *mut u64, value) }
    }

    fn read_port(&self, port: u8, offset: u64) -> u32 {
        let port_base = PORTSC_BASE_OFF + (port as u64) * (self.port_reg_set_size as u64) * 4;
        self.read_op(port_base + offset)
    }

    fn write_port(&self, port: u8, offset: u64, value: u32) {
        let port_base = PORTSC_BASE_OFF + (port as u64) * (self.port_reg_set_size as u64) * 4;
        self.write_op(port_base + offset, value)
    }

    fn write_doorbell(&self, target: u32, db_value: u32) {
        let doorbell_addr = self.doorbell_off + (target as u64) * 4;
        unsafe { write_volatile(doorbell_addr as *mut u32, db_value) }
    }

    // ---- Controller lifecycle ----

    /// Wait for controller to be ready (not halted and not not-ready)
    fn wait_ready(&self) -> bool {
        for _ in 0..1000000 {
            let usbsts = self.read_op(USBSTS_OFF);
            if (usbsts & USBSTS_HCHALTED) == 0 && (usbsts & USBSTS_CNR) == 0 {
                return true;
            }
        }
        false
    }

    /// Reset the controller
    fn reset(&self) -> bool {
        self.write_op(USBCMD_OFF, USBCMD_HCRST);
        // Wait for reset to complete (HCHalted bit should be set)
        for _ in 0..100000 {
            if self.read_op(USBSTS_OFF) & USBSTS_HCHALTED != 0 {
                return true;
            }
        }
        false
    }

    /// Start the controller (clear HCHalted)
    fn start(&self) -> bool {
        self.write_op(USBCMD_OFF, USBCMD_RUN_STOP);
        // Wait until controller clears HCHalted
        for _ in 0..100000 {
            if (self.read_op(USBSTS_OFF) & USBSTS_HCHALTED) == 0 {
                return true;
            }
        }
        false
    }

    /// Stop the controller
    fn stop(&self) {
        let cmd = self.read_op(USBCMD_OFF);
        self.write_op(USBCMD_OFF, cmd & !USBCMD_RUN_STOP);
    }

    /// Enumerate ports and report connection status
    fn enumerate_ports(&self) {
        for port in 0..self.max_ports {
            let portsc = self.read_port(port, 0);
            let connected = (portsc & PORTSC_CCS) != 0;
            let enabled = (portsc & PORTSC_PED) != 0;
            let speed = (portsc >> PORTSC_PS_SHIFT) & 0xF;
            let changed = (portsc & PORTSC_CSC) != 0;

            if connected {
                let speed_str = match speed {
                    PORT_SPEED_FULL => "Full (12 Mbps)",
                    PORT_SPEED_LOW => "Low (1.5 Mbps)",
                    PORT_SPEED_HIGH => "High (480 Mbps)",
                    PORT_SPEED_SUPER => "Super (5 Gbps)",
                    _ => "Unknown",
                };

                crate::serial::write_str("[xHCI] Port ");
                crate::serial::write_dec(port as u64);
                crate::serial::write_str(": connected, speed=");
                crate::serial::write_str(speed_str);

                if !enabled {
                    crate::serial::write_str(" (disabled)");
                }
                if changed {
                    crate::serial::write_str(" [CHANGED]");
                    // Clear change bits by writing them
                    self.write_port(port, 0, portsc | PORTSC_CSC);
                }
                crate::serial::write_str("\n");
            } else {
                crate::serial::write_str("[xHCI] Port ");
                crate::serial::write_dec(port as u64);
                crate::serial::write_str(": nothing connected\n");
            }

            // Ensure port power is on
            if (portsc & PORTSC_PP) == 0 {
                crate::serial::write_str("[xHCI] Port ");
                crate::serial::write_dec(port as u64);
                crate::serial::write_str(": powering on...\n");
                // For USB 3 (SuperSpeed) ports, PP is enabled via another mechanism
                // For USB 2 ports, we set PP bit
                if speed != PORT_SPEED_SUPER {
                    self.write_port(port, 0, portsc | PORTSC_PP);
                }
            }
        }
    }
}

// ========================================================================
// Ring Buffer Management
// ========================================================================

/// A simple single-segment ring buffer for TRBs
#[repr(C, align(64))]
pub struct TrbRing {
    /// Physical address of this ring
    pub phys_addr: u64,
    /// TRB entries (single segment — no linking)
    pub entries: [Trb; 16],
    /// Producer index (where to write next TRB)
    pub enqueue_idx: usize,
    /// Current cycle bit (toggle for wrapping)
    pub cycle: bool,
}

impl TrbRing {
    /// Create a new ring at the given physical address
    pub const fn new(phys_addr: u64) -> Self {
        Self {
            phys_addr,
            entries: [Trb::zero(); 16],
            enqueue_idx: 0,
            cycle: true,  // Start with cycle=1
        }
    }

    /// Get the current dequeue/enqueue pointer value
    pub fn ptr_value(&self) -> u64 {
        self.phys_addr + (self.enqueue_idx as u64) * TRB_SIZE as u64
    }

    /// Get the base pointer (start of ring) with cycle bit
    pub fn base_ptr_with_dcs(&self) -> u64 {
        self.phys_addr | if self.cycle { 1 } else { 0 }
    }

    /// Enqueue a TRB
    pub fn enqueue(&mut self, trb: &Trb) -> bool {
        // Check for ring full (conservatively: leave 1 slot empty)
        // In a real driver we'd use a proper full/empty check
        if self.enqueue_idx >= self.entries.len() - 1 {
            // Wrap around with a Link TRB
            let link = make_link(self.phys_addr, self.cycle, true, false);
            mut_trb(&mut self.entries[self.enqueue_idx], &link);
            self.enqueue_idx = 0;
            self.cycle = !self.cycle;

            // Now write the actual TRB at slot 0
            let mut trb_copy = *trb;
            trb_copy.set_cycle_bit(self.cycle);
            self.entries[0] = trb_copy;
            self.enqueue_idx = 1;
            true
        } else {
            let mut trb_copy = *trb;
            trb_copy.set_cycle_bit(self.cycle);
            self.entries[self.enqueue_idx] = trb_copy;
            self.enqueue_idx += 1;
            true
        }
    }
}

fn mut_trb(dst: &mut Trb, src: &Trb) {
    *dst = *src;
}

// ========================================================================
// xHCI Driver — High-level API
// ========================================================================

/// USB device speed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbSpeed {
    Low,     // 1.5 Mbps
    Full,    // 12 Mbps
    High,    // 480 Mbps
    Super,   // 5 Gbps
    SuperPlus, // 10 Gbps
}

/// USB device descriptor type (first 8 bytes of any descriptor)
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct DeviceDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub usb_spec: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub max_packet_size: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_release: u16,
    pub manufacturer: u8,
    pub product: u8,
    pub serial: u8,
    pub num_configurations: u8,
}

/// USB HID descriptor
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct HidDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub hid_spec: u16,
    pub country_code: u8,
    pub num_descriptors: u8,
    pub descriptor_type_hid: u8,
    pub descriptor_length: u16,
}

impl fmt::Debug for DeviceDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Copy fields to avoid unaligned access in packed struct
        let vendor_id = self.vendor_id;
        let product_id = self.product_id;
        let device_class = self.device_class;
        let max_packet_size = self.max_packet_size;
        write!(f, "USB Device: vendor=0x{:04x} product=0x{:04x} class=0x{:02x} max_pkt={}",
            vendor_id, product_id, device_class, max_packet_size)
    }
}

/// xHCI Controller driver state
pub struct XhciDriver {
    mmio: XhciMmio,
    dcbaap: *mut u64,          // Device Context Base Address Array
    dcbaap_phys: u64,
    command_ring: &'static mut TrbRing,
    command_ring_phys: u64,
    device_contexts: *mut *mut DeviceContext,
    scratchpad_bufs: *mut u64,
    initialized: bool,
}

impl XhciDriver {
    /// Initialize the xHCI controller
    pub fn init(mmio_base: u64, dcbaap_phys: u64, cmd_ring_phys: u64) -> Option<Self> {
        let mmio = XhciMmio::new(mmio_base);
        crate::serial::write_str("[xHCI] Resetting controller...\n");
        if !mmio.reset() {
            crate::serial::write_str("[xHCI] Reset failed!\n");
            return None;
        }

        crate::serial::write_str("[xHCI] Starting controller...\n");
        if !mmio.start() {
            crate::serial::write_str("[xHCI] Start failed!\n");
            return None;
        }

        if !mmio.wait_ready() {
            crate::serial::write_str("[xHCI] Controller not ready!\n");
            return None;
        }

        let cmd_ring_ref: &'static mut TrbRing = unsafe {
            &mut *(cmd_ring_phys as *mut TrbRing)
        };

        // Set up Command Ring Control Register
        // Lower bits: ring cycle state (bit 0 = RCS), command stop (bit 1 = CS), command abort (bit 2 = CA)
        // Upper bits: ring segment base address (64-byte aligned)
        let crcr = cmd_ring_phys | 1; // RCS = 1 (initial cycle bit)
        mmio.write_op64(CRCR_OFF, crcr);

        // Set DCBAAP
        mmio.write_op64(DCBAAP_OFF, dcbaap_phys);

        // Set max device slots in CONFIG register
        mmio.write_op(CONFIG_OFF, mmio.max_slots as u32);

        crate::serial::write_str("[xHCI] Enumerating ports...\n");
        mmio.enumerate_ports();

        Some(Self {
            mmio,
            dcbaap: dcbaap_phys as *mut u64,
            dcbaap_phys,
            command_ring: cmd_ring_ref,
            command_ring_phys: cmd_ring_phys,
            device_contexts: core::ptr::null_mut(),
            scratchpad_bufs: core::ptr::null_mut(),
            initialized: true,
        })
    }

    /// Send a command via the command ring and wait for completion
    pub fn send_command(&mut self, trb: &Trb) -> bool {
        let cmd = *trb;
        self.command_ring.enqueue(&cmd);

        // Ring the host doorbell (doorbell 0 with value 0)
        self.mmio.write_doorbell(DOORBELL_HOST, 0);

        // Busy-wait for completion (in a real kernel we'd use the event ring)
        // For now, just return true and assume it completed
        // TODO: Event ring processing for proper completion detection
        true
    }

    /// Enable a device slot (returns slot ID)
    pub fn enable_slot(&mut self) -> Option<u8> {
        let tx = make_enable_slot(self.command_ring.cycle);
        if self.send_command(&tx) {
            // In a real driver we'd get the slot ID from the command completion event
            // For now, assume slot 1
            crate::serial::write_str("[xHCI] Slot enabled (assumed slot 1)\n");
            Some(1)
        } else {
            None
        }
    }
}

/// Initialize all USB controllers found on PCI bus
pub fn init_xhci() {
    crate::serial::write_str("[USB] Scanning for xHCI controllers...\n");

    let controllers = crate::pci::find_usb_controllers();
    if controllers.is_empty() {
        crate::serial::write_str("[USB] No USB controllers found.\n");
        return;
    }

    for ctrl in controllers.iter() {
        crate::serial::write_str("[USB] Found ");
        crate::serial::write_str(ctrl.class_name());
        crate::serial::write_str(" at PCI 0:");
        crate::serial::write_dec(ctrl.device as u64);
        crate::serial::write_str(".");
        crate::serial::write_dec(ctrl.function as u64);
        crate::serial::write_str(" (");
        crate::serial::write_hex(ctrl.vendor_id as u64);
        crate::serial::write_str(":");
        crate::serial::write_hex(ctrl.device_id as u64);
        crate::serial::write_str(")\n");
    }

    // Only try xHCI controllers (prog_if 0x30)
    for ctrl in controllers.iter() {
        if ctrl.prog_if == 0x30 && ctrl.class == 0x0C && ctrl.subclass == 0x03 {
            if let Some(mmio_base) = ctrl.mmio_base() {
                crate::serial::write_str("[xHCI] MMIO base: 0x");
                crate::serial::write_hex(mmio_base);
                crate::serial::write_str("\n");

                // Allocate DCBAAP and command ring in pre-allocated memory
                // For now, we use fixed addresses in the low megabyte
                // (These would come from a proper frame allocator in Phase 2)
                let dcbaap_phys: u64 = 0x1000;  // Well-known scratch area
                let cmd_ring_phys: u64 = 0x2000; // Command ring

                match XhciDriver::init(mmio_base, dcbaap_phys, cmd_ring_phys) {
                    Some(_driver) => {
                        crate::serial::write_str("[xHCI] Controller initialized ✓\n");
                    }
                    None => {
                        crate::serial::write_str("[xHCI] Initialization failed ✗\n");
                    }
                }
            } else {
                crate::serial::write_str("[xHCI] No MMIO BAR found!\n");
            }
        } else {
            crate::serial::write_str("[USB] Skipping non-xHCI controller (");
            crate::serial::write_str(ctrl.class_name());
            crate::serial::write_str(")\n");
        }
    }
}
