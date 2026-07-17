//! virtio-net — network driver with an ARP proof-of-life.
//!
//! Queue 0 = RX, queue 1 = TX. With no features negotiated beyond
//! VIRTIO_NET_F_MAC, every packet is prefixed by the 10-byte legacy
//! virtio-net header, and RX buffers must be large enough for a full
//! frame (we use 2 KiB).
//!
//! The demo sends an ARP request for the QEMU user-net gateway
//! (10.0.2.2) and prints the reply — a full TX → device → RX round trip.

use spin::Mutex;

use super::{DmaRegion, VirtioLegacy, Virtqueue};

/// VIRTIO_NET_F_MAC — device has a valid MAC in config space
const FEATURE_MAC: u32 = 1 << 5;

/// Legacy virtio-net header size (no MRG_RXBUF)
const NET_HDR_LEN: usize = 10;

const RX_BUFFERS: usize = 8;
const RX_BUF_LEN: usize = 2048;

struct NetDevice {
    transport: VirtioLegacy,
    rx: Virtqueue,
    tx: Virtqueue,
    /// RX_BUFFERS × RX_BUF_LEN receive area (device writes)
    rx_buf: DmaRegion,
    /// One page TX staging area (header + frame)
    tx_buf: DmaRegion,
    mac: [u8; 6],
    /// Poisoned after a TX timeout or mismatched TX completion: the
    /// device still owns the TX descriptor and the shared tx_buf, so a
    /// late completion would be misattributed to the next send (and the
    /// device could DMA from tx_buf while it's being rewritten).
    /// Recovery requires a full device reset; until then, fail fast.
    /// (RX wait timeouts are normal — "no packet yet" — not failures.)
    failed: bool,
}

static NET: Mutex<Option<NetDevice>> = Mutex::new(None);

pub fn available() -> bool {
    NET.lock().as_ref().map_or(false, |d| !d.failed)
}

pub fn mac() -> [u8; 6] {
    NET.lock().as_ref().map_or([0; 6], |d| d.mac)
}

fn write_mac(mac: &[u8; 6]) {
    for (i, b) in mac.iter().enumerate() {
        if i > 0 {
            crate::serial::write_str(":");
        }
        crate::serial::write_hex_byte(*b);
    }
}

/// Initialize a transitional virtio-net PCI device
pub fn init(dev: &crate::pci::PciDevice) {
    let transport = match VirtioLegacy::new(dev) {
        Some(t) => t,
        None => {
            crate::serial::write_line("[NET] virtio-net has no I/O BAR — skipped");
            return;
        }
    };

    let host = transport.host_features();
    transport.set_guest_features(host & FEATURE_MAC);

    let rx_size = transport.queue_size(0);
    let tx_size = transport.queue_size(1);
    if rx_size == 0 || tx_size == 0 {
        crate::serial::write_line("[NET] Missing RX/TX queue — skipped");
        return;
    }
    let mut rx = Virtqueue::new(rx_size);
    let tx = Virtqueue::new(tx_size);
    transport.set_queue_pfn(0, rx.ring_phys);
    transport.set_queue_pfn(1, tx.ring_phys);

    // MAC address: config bytes 0..6
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = transport.config_read8(i as u16);
    }

    // Prefill the RX queue with device-writable buffers
    let rx_buf = super::dma_alloc(RX_BUFFERS * RX_BUF_LEN / 4096);
    for i in 0..RX_BUFFERS {
        let phys = rx_buf.phys + (i * RX_BUF_LEN) as u64;
        rx.submit(&[(phys, RX_BUF_LEN as u32, true)]);
    }

    transport.driver_ok();
    transport.notify(0); // RX buffers are available

    crate::serial::write_str("[NET] virtio-net ready, MAC ");
    write_mac(&mac);
    crate::serial::write_str(", RX/TX queues ");
    crate::serial::write_dec(rx_size as u64);
    crate::serial::write_str("/");
    crate::serial::write_dec(tx_size as u64);
    crate::serial::write_str("\n");

    *NET.lock() = Some(NetDevice {
        transport,
        rx,
        tx,
        rx_buf,
        tx_buf: super::dma_alloc(1),
        mac,
        failed: false,
    });
}

/// Send a raw ethernet frame (blocking until the device consumes it)
fn send_frame(dev: &mut NetDevice, frame: &[u8]) -> bool {
    if dev.failed {
        return false;
    }
    assert!(NET_HDR_LEN + frame.len() <= dev.tx_buf.size);
    unsafe {
        // 10-byte legacy header, all zero (no checksum offload, no GSO)
        core::ptr::write_bytes(dev.tx_buf.virt, 0, NET_HDR_LEN);
        core::ptr::copy_nonoverlapping(
            frame.as_ptr(),
            dev.tx_buf.virt.add(NET_HDR_LEN),
            frame.len(),
        );
    }

    let chain = [(dev.tx_buf.phys, (NET_HDR_LEN + frame.len()) as u32, false)];
    let head = match dev.tx.submit(&chain) {
        Some(h) => h,
        None => return false,
    };
    dev.transport.notify(1);

    match dev.tx.wait_used(1, 1000) {
        None => {
            // TX descriptor + tx_buf remain device-owned; a late
            // completion would be misread by the next send. Poison.
            dev.failed = true;
            crate::serial::write_line("[NET] TX timed out — device disabled (needs reset)");
            false
        }
        Some((used_head, _)) if used_head != head => {
            dev.failed = true;
            crate::serial::write_line("[NET] Stale TX completion — device disabled");
            false
        }
        Some(_) => true,
    }
}

/// Receive one frame into `out` (without the virtio header), waiting up to
/// `timeout_ms`. Returns the frame length. A timeout here is NOT an error
/// (no packet arrived); the posted RX buffers remain valid.
fn recv_frame(dev: &mut NetDevice, out: &mut [u8], timeout_ms: u32) -> Option<usize> {
    if dev.failed {
        return None;
    }
    let (head, written) = dev.rx.wait_used(1, timeout_ms)?;
    let written = written as usize;

    // Which RX buffer completed? Descriptor heads for the prefilled
    // single-descriptor chains are 0..RX_BUFFERS in submit order.
    let buf_off = (head as usize % RX_BUFFERS) * RX_BUF_LEN;
    let frame_len = written.saturating_sub(NET_HDR_LEN).min(out.len());
    unsafe {
        core::ptr::copy_nonoverlapping(
            dev.rx_buf.virt.add(buf_off + NET_HDR_LEN),
            out.as_mut_ptr(),
            frame_len,
        );
        // Recycle the buffer back into the RX queue
        let phys = dev.rx_buf.phys + buf_off as u64;
        dev.rx.submit(&[(phys, RX_BUF_LEN as u32, true)]);
        dev.transport.notify(0);
    }
    Some(frame_len)
}

// ========================================================================
// ARP demo — resolve the QEMU user-net gateway
// ========================================================================

const OUR_IP: [u8; 4] = [10, 0, 2, 15]; // QEMU slirp default guest address
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2]; // QEMU slirp gateway

/// Send an ARP request for the gateway and wait for the reply.
/// Demonstrates a full TX + RX round trip through the device.
pub fn arp_demo() {
    let mut guard = NET.lock();
    let dev = match guard.as_mut() {
        Some(d) => d,
        None => {
            crate::serial::write_line("[NET] No virtio-net device — ARP demo skipped");
            return;
        }
    };

    // Ethernet: dst broadcast, src our MAC, ethertype 0x0806 (ARP)
    let mut frame = [0u8; 42];
    frame[0..6].copy_from_slice(&[0xFF; 6]);
    frame[6..12].copy_from_slice(&dev.mac);
    frame[12..14].copy_from_slice(&[0x08, 0x06]);
    // ARP: ethernet(1) / IPv4(0x0800), hlen 6, plen 4, op 1 (request)
    frame[14..22].copy_from_slice(&[0x00, 0x01, 0x08, 0x00, 6, 4, 0x00, 0x01]);
    frame[22..28].copy_from_slice(&dev.mac); // sender MAC
    frame[28..32].copy_from_slice(&OUR_IP); // sender IP
    // target MAC unknown (zeros), target IP = gateway
    frame[38..42].copy_from_slice(&GATEWAY_IP);

    crate::serial::write_str("[NET] ARP who-has 10.0.2.2 tell 10.0.2.15 ... ");
    if !send_frame(dev, &frame) {
        crate::serial::write_str("TX FAILED\n");
        return;
    }

    // Wait for the ARP reply (skip unrelated frames)
    let mut rx = [0u8; 128];
    for _ in 0..10 {
        let len = match recv_frame(dev, &mut rx, 500) {
            Some(l) => l,
            None => break,
        };
        // ARP reply (op 2) from the gateway?
        if len >= 42
            && rx[12..14] == [0x08, 0x06]
            && rx[20..22] == [0x00, 0x02]
            && rx[28..32] == GATEWAY_IP
        {
            crate::serial::write_str("reply: 10.0.2.2 is-at ");
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&rx[22..28]);
            write_mac(&mac);
            crate::serial::write_str("\n");
            return;
        }
    }
    crate::serial::write_str("no reply (timeout)\n");
}
