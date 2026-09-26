//! xHCI Context Structures — Slot, Endpoint, and Input contexts.
//!
//! These structures define the state of devices and endpoints managed by
//! the xHCI controller. They live in main memory and are referenced by
//! the Device Context Base Address Array (DCBAAP).

use core::fmt;

/// Size of a device context entry in the DCBAAP
pub const DEVICE_CONTEXT_SIZE: usize = 1024; // xHCI spec: device context is 1024 bytes

/// Size of an input context
pub const INPUT_CONTEXT_SIZE: usize = 1056; // Input context control + device context

/// Number of context entries per device (max 31)
pub const MAX_CONTEXT_ENTRIES: usize = 31;

// ========================================================================
// Slot Context (8 dwords = 32 bytes)
// ========================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SlotContext {
    pub dw0: u32,  // Route String, Speed, MTK, Hub, Context Entries
    pub dw1: u32,  // Max Exit Latency, Root Hub Port Number
    pub dw2: u32,  // Number of Ports, TT info
    pub dw3: u32,  // Device Address, Slot State
    pub dw4: u32,  // Reserved
    pub dw5: u32,  // Reserved
    pub dw6: u32,  // Reserved
    pub dw7: u32,  // Reserved
}

impl SlotContext {
    pub const fn zero() -> Self {
        Self { dw0: 0, dw1: 0, dw2: 0, dw3: 0, dw4: 0, dw5: 0, dw6: 0, dw7: 0 }
    }

    /// Get route string (bits 31-0 of dw0)
    pub fn route_string(&self) -> u32 {
        self.dw0 & 0xFFFFF
    }

    /// Get device speed (bits 23-20 of dw0)
    pub fn speed(&self) -> u8 {
        ((self.dw0 >> 20) & 0xF) as u8
    }

    /// Set device speed
    pub fn set_speed(&mut self, speed: u8) {
        self.dw0 = (self.dw0 & !(0xF << 20)) | ((speed as u32) << 20);
    }

    /// Get number of context entries (bits 27-24 of dw0)
    pub fn context_entries(&self) -> u8 {
        ((self.dw0 >> 27) & 0x1F) as u8
    }

    /// Set number of context entries
    pub fn set_context_entries(&mut self, entries: u8) {
        self.dw0 = (self.dw0 & !(0x1F << 27)) | ((entries as u32) << 27);
    }

    /// Get root hub port number (bits 31-24 of dw1)
    pub fn root_hub_port(&self) -> u8 {
        ((self.dw1 >> 24) & 0xFF) as u8
    }

    /// Set root hub port number
    pub fn set_root_hub_port(&mut self, port: u8) {
        self.dw1 = (self.dw1 & !(0xFF << 24)) | ((port as u32) << 24);
    }

    /// Get slot state
    pub fn state(&self) -> u8 {
        ((self.dw3 >> 27) & 0x1F) as u8
    }

    pub fn set_state(&mut self, state: u8) {
        self.dw3 = (self.dw3 & !(0x1F << 27)) | ((state as u32) << 27);
    }

    /// Speed string for debugging
    pub fn speed_str(&self) -> &'static str {
        match self.speed() {
            1 => "Full (12 Mbps)",
            2 => "Low (1.5 Mbps)",
            3 => "High (480 Mbps)",
            4 => "Super (5 Gbps)",
            5 => "Super+ (10 Gbps)",
            _ => "Unknown",
        }
    }
}

impl fmt::Debug for SlotContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Slot(speed={} port={} entries={})",
            self.speed(), self.root_hub_port(), self.context_entries())
    }
}

// ========================================================================
// Endpoint Context (8 dwords = 32 bytes)
// ========================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EndpointContext {
    pub dw0: u32,  // EP State, Mult, MaxPStreams, LSA, Interval
    pub dw1: u32,  // Max ESIT Payload, CErr, EP Type
    pub dw2: u32,  // TR Dequeue Pointer (low 32 bits)
    pub dw3: u32,  // TR Dequeue Pointer (high 32 bits) + DCS
    pub dw4: u32,  // Average TRB Length, Max Packet Size
    pub dw5: u32,  // Reserved
    pub dw6: u32,  // Reserved
    pub dw7: u32,  // Reserved
}

impl EndpointContext {
    pub const fn zero() -> Self {
        Self { dw0: 0, dw1: 0, dw2: 0, dw3: 0, dw4: 0, dw5: 0, dw6: 0, dw7: 0 }
    }

    /// Get endpoint type (bits 6-3 of dw1)
    pub fn ep_type(&self) -> u8 {
        ((self.dw1 >> 3) & 0xF) as u8
    }

    /// Set endpoint type
    /// 0=Invalid, 1=Isoch OUT, 2=Bulk OUT, 3=Interrupt OUT,
    /// 4=Control BIDIR, 5=Isoch IN, 6=Bulk IN, 7=Interrupt IN
    pub fn set_ep_type(&mut self, ep_type: u8) {
        self.dw1 = (self.dw1 & !(0xF << 3)) | ((ep_type as u32) << 3);
    }

    /// Set max packet size (bits 31-16 of dw4)
    pub fn set_max_packet_size(&mut self, size: u16) {
        self.dw4 = (self.dw4 & !(0xFFFF << 16)) | ((size as u32) << 16);
    }

    /// Get max packet size
    pub fn max_packet_size(&self) -> u16 {
        ((self.dw4 >> 16) & 0xFFFF) as u16
    }

    /// Set average TRB length (bits 15-0 of dw4)
    pub fn set_avg_trb_length(&mut self, len: u16) {
        self.dw4 = (self.dw4 & !0xFFFF) | (len as u32);
    }

    /// Set TR dequeue pointer (64-bit physical address of transfer ring)
    pub fn set_tr_dequeue_ptr(&mut self, ptr: u64, dcs: bool) {
        self.dw2 = (ptr & 0xFFFFFFFF) as u32;
        self.dw3 = ((ptr >> 32) & 0xFFFFFFFF) as u32 | if dcs { 1 } else { 0 };
    }

    /// Get CErr (bits 1-0 of dw1)
    pub fn cerr(&self) -> u8 {
        (self.dw1 & 0x3) as u8
    }

    pub fn set_cerr(&mut self, cerr: u8) {
        self.dw1 = (self.dw1 & !0x3) | (cerr as u32);
    }

    /// Get max ESIT payload (bits 31-16 of dw1)
    pub fn max_esit_payload(&self) -> u16 {
        ((self.dw1 >> 16) & 0xFFFF) as u16
    }

    pub fn set_max_esit_payload(&mut self, payload: u16) {
        self.dw1 = (self.dw1 & !(0xFFFF << 16)) | ((payload as u32) << 16);
    }

    /// Endpoint type string
    pub fn ep_type_str(&self) -> &'static str {
        match self.ep_type() {
            0 => "Invalid",
            1 => "Isoch OUT",
            2 => "Bulk OUT",
            3 => "Interrupt OUT",
            4 => "Control",
            5 => "Isoch IN",
            6 => "Bulk IN",
            7 => "Interrupt IN",
            _ => "Reserved",
        }
    }
}

impl fmt::Debug for EndpointContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EP(type={} max_pkt={})", self.ep_type_str(), self.max_packet_size())
    }
}

// ========================================================================
// Input Context Control (first 8 bytes of Input Context)
// ========================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct InputControlContext {
    /// Drop context flags
    pub drop_context: u32,
    /// Add context flags
    pub add_context: u32,
    /// Reserved
    pub reserved: [u32; 5],
    /// Reserved 2
    pub reserved2: u32,
}

impl InputControlContext {
    pub const fn zero() -> Self {
        Self { drop_context: 0, add_context: 0, reserved: [0; 5], reserved2: 0 }
    }

    /// Add a context entry (set bit for slot/endpoint)
    pub fn add(&mut self, index: u8) {
        self.add_context |= 1 << index;
    }

    /// Drop a context entry
    pub fn drop(&mut self, index: u8) {
        self.drop_context |= 1 << index;
    }
}

// ========================================================================
// Device Context (32 bytes * 31 entries = 992 bytes + alignment padding)
// ========================================================================

/// Full Device Context — contains slot context + up to 31 endpoint contexts
#[repr(C, align(64))]
pub struct DeviceContext {
    pub slot: SlotContext,
    pub endpoints: [EndpointContext; MAX_CONTEXT_ENTRIES],
}

/// Full Input Context — input control context + device context
#[repr(C, align(64))]
pub struct InputContext {
    pub control: InputControlContext,
    pub device: DeviceContext,
}
