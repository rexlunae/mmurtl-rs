//! xHCI Transfer Request Blocks — the fundamental communication unit for xHCI.
//!
//! TRBs are 16-byte structures that form rings (circular buffers). There are
//! three types of rings: Command Ring (driver → xHC), Transfer Rings (driver →
//! endpoint), and Event Rings (xHC → driver).

use core::fmt;

/// Size of a TRB in bytes
pub const TRB_SIZE: usize = 16;

/// TRB Type codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrbType {
    /// Reserved
    Reserved = 0,
    /// Normal transfer
    Normal = 1,
    /// Setup stage of a control transfer
    Setup = 2,
    /// Data stage of a control transfer
    Data = 3,
    /// Status stage of a control transfer
    Status = 4,
    /// Isochronous transfer
    Isoch = 5,
    /// Link TRB for ring wrapping
    Link = 6,
    /// Event data
    EventData = 7,
    /// No-op
    NoOp = 8,
    /// Enable slot command
    EnableSlot = 9,
    /// Disable slot command
    DisableSlot = 10,
    /// Address device command
    AddressDevice = 11,
    /// Configure endpoint command
    ConfigureEndpoint = 12,
    /// Evaluate context command
    EvaluateContext = 13,
    /// Reset endpoint command
    ResetEndpoint = 14,
    /// Stop endpoint command
    StopEndpoint = 15,
    /// Set TR dequeue pointer command
    SetTrDequeue = 16,
    /// Reset device command
    ResetDevice = 17,
    /// Force event command
    ForceEvent = 18,
    /// Negotiate bandwidth command
    NegotiateBandwidth = 19,
    /// Set latency tolerance value command
    SetLatencyTolerance = 20,
    /// Set port BIOS ownership
    SetPortBio = 21,
    /// Clear feature command
    ClearFeature = 22,
    /// Transfer event
    TransferEvent = 32,
    /// Command completion event
    CommandCompletion = 33,
    /// Port status change event
    PortStatusChange = 34,
    /// Bandwidth request event
    BandwidthRequest = 35,
    /// Doorbell event
    Doorbell = 36,
    /// Host controller event
    HostController = 37,
    /// Device notification
    DeviceNotification = 38,
    /// MFINDEX wrap event
    MfindexWrap = 39,
}

impl TrbType {
    pub fn from_value(v: u8) -> Option<Self> {
        use TrbType::*;
        match v {
            0 => Some(Reserved),
            1 => Some(Normal),
            2 => Some(Setup),
            3 => Some(Data),
            4 => Some(Status),
            5 => Some(Isoch),
            6 => Some(Link),
            7 => Some(EventData),
            8 => Some(NoOp),
            9 => Some(EnableSlot),
            10 => Some(DisableSlot),
            11 => Some(AddressDevice),
            12 => Some(ConfigureEndpoint),
            13 => Some(EvaluateContext),
            14 => Some(ResetEndpoint),
            15 => Some(StopEndpoint),
            16 => Some(SetTrDequeue),
            17 => Some(ResetDevice),
            18 => Some(ForceEvent),
            19 => Some(NegotiateBandwidth),
            20 => Some(SetLatencyTolerance),
            21 => Some(SetPortBio),
            22 => Some(ClearFeature),
            32 => Some(TransferEvent),
            33 => Some(CommandCompletion),
            34 => Some(PortStatusChange),
            35 => Some(BandwidthRequest),
            36 => Some(Doorbell),
            37 => Some(HostController),
            38 => Some(DeviceNotification),
            39 => Some(MfindexWrap),
            _ => None,
        }
    }
}

/// A raw 16-byte TRB
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct Trb {
    /// Lower 64 bits — parameter
    pub params: u64,
    /// Bits 0-31: status/transfer length
    /// Bits 32-47: flags
    /// Bits 48-63: control (type + cycle bit + other flags)
    pub status: u32,
    pub control: u32,
}

impl Trb {
    /// Create a new TRB
    pub const fn new(params: u64, status: u32, control: u32) -> Self {
        Self { params, status, control }
    }

    /// Zero-initialized TRB
    pub const fn zero() -> Self {
        Self { params: 0, status: 0, control: 0 }
    }

    /// Get the TRB type
    pub fn trb_type(&self) -> Option<TrbType> {
        TrbType::from_value(((self.control >> 10) & 0x3F) as u8)
    }

    /// Get the cycle bit (bit 0 of control)
    pub fn cycle_bit(&self) -> bool {
        (self.control & 1) != 0
    }

    /// Set the cycle bit
    pub fn set_cycle_bit(&mut self, cycle: bool) {
        if cycle {
            self.control |= 1;
        } else {
            self.control &= !1;
        }
    }

    /// Get the completion code from an event TRB
    pub fn completion_code(&self) -> u8 {
        ((self.status >> 24) & 0xFF) as u8
    }

    /// Get TRB transfer length from an event TRB
    pub fn event_trb_length(&self) -> u32 {
        self.status & 0x1FFFF
    }
}

impl fmt::Debug for Trb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TRB(params=0x{:016x}, status=0x{:08x}, ctrl=0x{:08x})",
            self.params, self.status, self.control)
    }
}

// ========================================================================
// TRB Builders — Convenience functions for common TRB types
// ========================================================================

/// Build a NO-OP command TRB
pub fn make_noop(cycle: bool) -> Trb {
    let mut ctrl = 0u32;
    ctrl |= if cycle { 1 } else { 0 }; // Cycle bit
    ctrl |= (TrbType::NoOp as u32) << 10; // TRB type
    Trb::new(0, 0, ctrl)
}

/// Build an Enable Slot command TRB
pub fn make_enable_slot(cycle: bool) -> Trb {
    let mut ctrl = 0u32;
    ctrl |= if cycle { 1 } else { 0 };
    ctrl |= (TrbType::EnableSlot as u32) << 10;
    Trb::new(0, 0, ctrl)
}

/// Build an Address Device command TRB
pub fn make_address_device(slot_id: u8, input_ctx_ptr: u64, cycle: bool, block_set_address: bool) -> Trb {
    let mut ctrl = 0u32;
    ctrl |= if cycle { 1 } else { 0 };
    ctrl |= (TrbType::AddressDevice as u32) << 10;
    if block_set_address {
        ctrl |= (1 << 9); // Block Set Address Request (BSA) bit
    }
    let slot_id_dword = (slot_id as u32) << 24;
    Trb::new(input_ctx_ptr, slot_id_dword, ctrl)
}

/// Build a Configure Endpoint command TRB
pub fn make_configure_endpoint(slot_id: u8, input_ctx_ptr: u64, cycle: bool, deconfigure: bool) -> Trb {
    let mut ctrl = 0u32;
    ctrl |= if cycle { 1 } else { 0 };
    ctrl |= (TrbType::ConfigureEndpoint as u32) << 10;
    if deconfigure {
        ctrl |= (1 << 9); // Deconfigure bit
    }
    let slot_id_dword = (slot_id as u32) << 24;
    Trb::new(input_ctx_ptr, slot_id_dword, ctrl)
}

/// Build a Setup Stage TRB for control transfers
pub fn make_setup_stg(setup_packet: &[u8; 8], trt: u8, cycle: bool) -> Trb {
    let params = u64::from_le_bytes([
        setup_packet[0], setup_packet[1], setup_packet[2], setup_packet[3],
        setup_packet[4], setup_packet[5], setup_packet[6], setup_packet[7],
    ]);

    let mut ctrl = 0u32;
    ctrl |= if cycle { 1 } else { 0 };
    ctrl |= (TrbType::Setup as u32) << 10;
    ctrl |= (trt as u32) << 16; // Transfer Type: 0=no data, 2=IN, 3=OUT
    ctrl |= 1 << 6; // Immediate Data (ID) flag — data is in params
    ctrl |= 8 << 17; // Transfer length = 8 bytes (USB setup packet size)

    Trb::new(params, 0, ctrl)
}

/// Build a Data Stage TRB for control transfers
pub fn make_data_stg(buffer_ptr: u64, length: u32, direction_in: bool, cycle: bool, chain: bool) -> Trb {
    let mut ctrl = 0u32;
    ctrl |= if cycle { 1 } else { 0 };
    ctrl |= (TrbType::Data as u32) << 10;
    ctrl |= if chain { 1 << 4 } else { 0 }; // Chain bit
    ctrl |= if direction_in { 1 << 2 } else { 0 }; // Direction: IN
    ctrl |= (length & 0x1FFFF) << 17; // Transfer length

    Trb::new(buffer_ptr, 0, ctrl)
}

/// Build a Status Stage TRB for control transfers
pub fn make_status_stg(direction_in: bool, cycle: bool) -> Trb {
    let mut ctrl = 0u32;
    ctrl |= if cycle { 1 } else { 0 };
    ctrl |= (TrbType::Status as u32) << 10;
    ctrl |= if direction_in { 1 << 2 } else { 0 }; // Direction: IN
    ctrl |= 1 << 5; // Interrupt-on-complete (IOC)

    Trb::new(0, 0, ctrl)
}

/// Build a Normal TRB (for bulk/interrupt transfers)
pub fn make_normal(buffer_ptr: u64, length: u32, cycle: bool, chain: bool, ioc: bool) -> Trb {
    let mut ctrl = 0u32;
    ctrl |= if cycle { 1 } else { 0 };
    ctrl |= (TrbType::Normal as u32) << 10;
    ctrl |= if chain { 1 << 4 } else { 0 };
    ctrl |= if ioc { 1 << 5 } else { 0 };
    ctrl |= (length & 0x1FFFF) << 17;

    Trb::new(buffer_ptr, 0, ctrl)
}

/// Build a Link TRB for wrapping rings
pub fn make_link(segment_ptr: u64, cycle: bool, toggle_cycle: bool, chain: bool) -> Trb {
    let mut ctrl = 0u32;
    ctrl |= if cycle { 1 } else { 0 };
    ctrl |= (TrbType::Link as u32) << 10;
    ctrl |= if toggle_cycle { 1 << 2 } else { 0 }; // Toggle Cycle (TC)
    ctrl |= if chain { 1 << 4 } else { 0 };

    // Link TRB params is the 64-bit address of the next segment
    // Must be 16-byte aligned
    Trb::new(segment_ptr, 0, ctrl)
}
