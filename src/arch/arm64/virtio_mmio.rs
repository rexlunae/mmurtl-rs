//! arm64 virtio transport: virtio-mmio, legacy (version 1) and modern
//! (version 2) register layouts.
//!
//! QEMU `virt` provides 32 virtio-mmio slots (0x0a00_0000 + n × 0x200,
//! listed in the device tree); a slot with DeviceID 0 is empty. QEMU
//! defaults to the legacy layout (`-global virtio-mmio.force-legacy=false`
//! selects modern). Both use our split-virtqueue memory: legacy takes a
//! page frame number with a 4 KiB used-ring alignment, modern takes the
//! three ring addresses separately.

use alloc::boxed::Box;
use core::arch::asm;

use crate::virtio::{
    Transport, Virtqueue, STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK, STATUS_FEATURES_OK,
};

const MAGIC: u32 = 0x7472_6976; // "virt"

const REG_MAGIC: u64 = 0x000;
const REG_VERSION: u64 = 0x004;
const REG_DEVICE_ID: u64 = 0x008;
const REG_DEVICE_FEATURES: u64 = 0x010;
const REG_DEVICE_FEATURES_SEL: u64 = 0x014;
const REG_DRIVER_FEATURES: u64 = 0x020;
const REG_DRIVER_FEATURES_SEL: u64 = 0x024;
const REG_GUEST_PAGE_SIZE: u64 = 0x028; // legacy
const REG_QUEUE_SEL: u64 = 0x030;
const REG_QUEUE_NUM_MAX: u64 = 0x034;
const REG_QUEUE_NUM: u64 = 0x038;
const REG_QUEUE_ALIGN: u64 = 0x03C; // legacy
const REG_QUEUE_PFN: u64 = 0x040; // legacy
const REG_QUEUE_READY: u64 = 0x044; // modern
const REG_QUEUE_NOTIFY: u64 = 0x050;
const REG_STATUS: u64 = 0x070;
const REG_QUEUE_DESC_LOW: u64 = 0x080; // modern
const REG_QUEUE_DESC_HIGH: u64 = 0x084;
const REG_QUEUE_DRIVER_LOW: u64 = 0x090;
const REG_QUEUE_DRIVER_HIGH: u64 = 0x094;
const REG_QUEUE_DEVICE_LOW: u64 = 0x0A0;
const REG_QUEUE_DEVICE_HIGH: u64 = 0x0A4;
const REG_CONFIG: u64 = 0x100;

/// VIRTIO_F_VERSION_1, feature bit 32 (bit 0 of the high word)
const F_VERSION_1_HI: u32 = 1;

pub struct VirtioMmio {
    base: u64,
    modern: bool,
}

impl VirtioMmio {
    fn rd(&self, off: u64) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + off) as *const u32) }
    }

    fn wr(&self, off: u64, v: u32) {
        unsafe { core::ptr::write_volatile((self.base + off) as *mut u32, v) }
    }

    /// Claim the device at `base`: reset + ACKNOWLEDGE + DRIVER
    fn new(base: u64, version: u32) -> Self {
        let t = Self { base, modern: version >= 2 };
        t.wr(REG_STATUS, 0);
        t.wr(REG_STATUS, STATUS_ACKNOWLEDGE as u32);
        t.wr(REG_STATUS, (STATUS_ACKNOWLEDGE | STATUS_DRIVER) as u32);
        if !t.modern {
            t.wr(REG_GUEST_PAGE_SIZE, 4096);
        }
        t
    }
}

impl Transport for VirtioMmio {
    fn host_features(&self) -> u32 {
        self.wr(REG_DEVICE_FEATURES_SEL, 0);
        self.rd(REG_DEVICE_FEATURES)
    }

    fn set_guest_features(&self, features: u32) {
        self.wr(REG_DRIVER_FEATURES_SEL, 0);
        self.wr(REG_DRIVER_FEATURES, features);
        if self.modern {
            self.wr(REG_DRIVER_FEATURES_SEL, 1);
            self.wr(REG_DRIVER_FEATURES, F_VERSION_1_HI);
            let s = (STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK) as u32;
            self.wr(REG_STATUS, s);
            if self.rd(REG_STATUS) & STATUS_FEATURES_OK as u32 == 0 {
                crate::serial::write_line("[VIRTIO] device rejected our feature set");
            }
        }
    }

    fn queue_max(&self, queue: u16) -> u16 {
        self.wr(REG_QUEUE_SEL, queue as u32);
        // Our rings hold at most 256 entries' worth of bookkeeping
        self.rd(REG_QUEUE_NUM_MAX).min(256) as u16
    }

    fn setup_queue(&self, queue: u16, vq: &Virtqueue) {
        self.wr(REG_QUEUE_SEL, queue as u32);
        self.wr(REG_QUEUE_NUM, vq.size() as u32);
        if self.modern {
            let (desc, avail, used) = vq.ring_addrs();
            self.wr(REG_QUEUE_DESC_LOW, desc as u32);
            self.wr(REG_QUEUE_DESC_HIGH, (desc >> 32) as u32);
            self.wr(REG_QUEUE_DRIVER_LOW, avail as u32);
            self.wr(REG_QUEUE_DRIVER_HIGH, (avail >> 32) as u32);
            self.wr(REG_QUEUE_DEVICE_LOW, used as u32);
            self.wr(REG_QUEUE_DEVICE_HIGH, (used >> 32) as u32);
            self.wr(REG_QUEUE_READY, 1);
        } else {
            self.wr(REG_QUEUE_ALIGN, 4096);
            self.wr(REG_QUEUE_PFN, (vq.ring_phys >> 12) as u32);
        }
    }

    fn notify(&self, queue: u16) {
        // Ring updates must reach memory before the device is poked
        unsafe { asm!("dsb sy") };
        self.wr(REG_QUEUE_NOTIFY, queue as u32);
    }

    fn driver_ok(&self) {
        let mut s = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK;
        if self.modern {
            s |= STATUS_FEATURES_OK;
        }
        self.wr(REG_STATUS, s as u32);
    }

    fn config_read8(&self, offset: u16) -> u8 {
        unsafe { core::ptr::read_volatile((self.base + REG_CONFIG + offset as u64) as *const u8) }
    }

    fn config_read32(&self, offset: u16) -> u32 {
        self.rd(REG_CONFIG + offset as u64)
    }

    fn modern(&self) -> bool {
        self.modern
    }

    fn name(&self) -> &'static str {
        if self.modern { "virtio-mmio v2" } else { "virtio-mmio v1 (legacy)" }
    }
}

/// Probe the device tree's virtio-mmio slots and attach drivers
pub fn probe(slots: &[u64]) {
    let mut found = 0;
    for &base in slots {
        let probe = VirtioMmio { base, modern: false };
        if probe.rd(REG_MAGIC) != MAGIC {
            continue;
        }
        let device_id = probe.rd(REG_DEVICE_ID);
        if device_id == 0 {
            continue; // empty slot
        }
        let version = probe.rd(REG_VERSION);
        found += 1;
        crate::virtio::attach(device_id, Box::new(VirtioMmio::new(base, version)));
    }
    if found == 0 {
        crate::serial::write_line("[VIRTIO] No virtio-mmio devices present");
    }
}
