//! RQB (Request Block) — Message-passing IPC for MMURTL/RS.
//!
//! RQBs are fixed-size messages (128 bytes) that tasks exchange for IPC.
//! The core primitives are:
//!   - SendRQB(to_task, rqb) — send a message and block until reply
//!   - WaitRQB() — wait for a message
//!   - SendRQBWait(to_task, rqb) — send then wait (for request-response pairs)
//!   - ReplyRQB(from_task, rqb) — reply to a waiting sender
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

// ========================================================================
// IPC Primitives (called by tasks, executed by kernel)
// ========================================================================

/// Send a message to another task and wait for a reply.
///
/// Returns the reply status. The caller's RQB is overwritten with the reply.
/// The calling task blocks until the receiver replies.
pub fn send_rqb(receiver_id: u32, rqb: &mut Rqb) -> RqbStatus {
    rqb.sender_id = crate::scheduler::current_task_id();

    // In MMURTL, the kernel copies the RQB to the receiver and blocks the sender.
    // For now, our scheduler manages this.
    crate::scheduler::send_message(receiver_id, rqb);

    // After the receiver replies, the scheduler wakes us and copies the reply back.
    // (The actual blocking happens inside send_message)
    crate::scheduler::current_rqb_status()
}

/// Wait to receive a message from another task.
///
/// Blocks the current task until a message arrives.
/// The received RQB is written to the provided buffer.
pub fn wait_rqb(rqb: &mut Rqb) {
    crate::scheduler::receive_message(rqb);
}

/// Send a message and immediately wait for a reply (convenience for RPC).
pub fn send_rqb_wait(receiver_id: u32, rqb: &mut Rqb) -> RqbStatus {
    send_rqb(receiver_id, rqb)
}

/// Reply to a sender with a response RQB.
pub fn reply_rqb(sender_id: u32, rqb: &Rqb) {
    crate::scheduler::reply_message(sender_id, rqb);
}
