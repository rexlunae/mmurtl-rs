//! RQB (Request Block) — Message-passing IPC for MMURTL/RS.
//!
//! RQBs are fixed-size messages that tasks exchange for IPC. The blocking
//! primitives live in the scheduler (they need its task states):
//!   - `send_rqb(to, &mut rqb)` — queue a request, block until the reply
//!   - `receive_rqb()` — block until a request arrives
//!   - `reply_rqb(sender, &rqb)` — answer a sender blocked in send_rqb
//!
//! This is the heart of MMURTL's IPC model. Everything is message-passing.

use core::fmt;

// ========================================================================
// RQB Structure (128 bytes total)
// ========================================================================

/// Maximum data size in an RQB
pub const RQB_DATA_SIZE: usize = 64;

/// An RQB service code — identifies what operation the request is for
pub type RqbServiceCode = u16;

/// RQB status codes (from kernel or service task)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum RqbStatus {
    Success = 0,
    GeneralFailure = 1,
    InvalidService = 2,
    InvalidParam = 3,
    Timeout = 4,
    NoMemory = 5,
    AccessDenied = 6,
    NotFound = 7,
    Busy = 8,
    Aborted = 9,
}

impl From<u16> for RqbStatus {
    fn from(v: u16) -> Self {
        match v {
            0 => Self::Success,
            1 => Self::GeneralFailure,
            2 => Self::InvalidService,
            3 => Self::InvalidParam,
            4 => Self::Timeout,
            5 => Self::NoMemory,
            6 => Self::AccessDenied,
            7 => Self::NotFound,
            8 => Self::Busy,
            9 => Self::Aborted,
            _ => Self::GeneralFailure,
        }
    }
}

/// A fixed-size inter-task message (128 bytes)
#[repr(C, align(16))]
#[derive(Clone)]
pub struct Rqb {
    /// Service code — identifies what this request is for
    pub service: RqbServiceCode,
    /// Status of the request/response
    pub status: u16,
    /// Sender task ID (set by kernel)
    pub sender_id: u32,
    /// Receiver task ID (set by kernel or caller)
    pub receiver_id: u32,
    /// Size of data in the data field (bytes)
    pub data_size: u16,
    /// Reserved for future use (padding)
    pub reserved: [u16; 2],
    /// Payload data
    pub data: [u8; RQB_DATA_SIZE],
}

impl Rqb {
    /// Create a new, zeroed-out RQB
    pub const fn new() -> Self {
        Self {
            service: 0,
            status: 0,
            sender_id: 0,
            receiver_id: 0,
            data_size: 0,
            reserved: [0; 2],
            data: [0; RQB_DATA_SIZE],
        }
    }

    /// Create an RQB with a service code
    pub fn with_service(service: RqbServiceCode) -> Self {
        Self {
            service,
            ..Self::new()
        }
    }

    /// Set the data payload from a byte slice (truncates if > RQB_DATA_SIZE)
    pub fn set_data(&mut self, data: &[u8]) {
        let len = core::cmp::min(data.len(), RQB_DATA_SIZE);
        self.data[..len].copy_from_slice(data);
        self.data_size = len as u16;
    }

    /// Get the data payload as a byte slice
    pub fn get_data(&self) -> &[u8] {
        &self.data[..core::cmp::min(self.data_size as usize, RQB_DATA_SIZE)]
    }

    /// Set the status code
    pub fn set_status(&mut self, status: RqbStatus) {
        self.status = status as u16;
    }

    /// Check if the status indicates success
    pub fn is_ok(&self) -> bool {
        self.status == RqbStatus::Success as u16
    }
}

impl fmt::Debug for Rqb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RQB[svc={:#06x} status={} sender={} recv={} size={}]",
            self.service, self.status, self.sender_id, self.receiver_id, self.data_size)
    }
}

// ========================================================================
// Standard Service Codes
// =======================================================================+

/// System service: no operation (ping)
pub const SVC_NOP: RqbServiceCode = 0x0000;
/// System service: get system info
pub const SVC_SYS_INFO: RqbServiceCode = 0x0001;
/// System service: get time
pub const SVC_GET_TIME: RqbServiceCode = 0x0002;
/// System service: sleep for N milliseconds
pub const SVC_SLEEP: RqbServiceCode = 0x0003;
/// Console service: write string
pub const SVC_CONSOLE_WRITE: RqbServiceCode = 0x0100;
/// Console service: read line
pub const SVC_CONSOLE_READ: RqbServiceCode = 0x0101;
/// Memory service: allocate memory
pub const SVC_MEM_ALLOC: RqbServiceCode = 0x0200;
/// Memory service: free memory
pub const SVC_MEM_FREE: RqbServiceCode = 0x0201;
/// Demo text service: upper-case the payload
pub const SVC_TEXT_UPPER: RqbServiceCode = 0x0300;
/// Demo text service: reverse the payload
pub const SVC_TEXT_REVERSE: RqbServiceCode = 0x0301;
/// Demo text service: shut the service down (exits without replying)
pub const SVC_TEXT_SHUTDOWN: RqbServiceCode = 0x03FF;
