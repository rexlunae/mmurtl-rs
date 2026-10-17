//! One bounded boot request to the host entropy source. DMA allocations are
//! retained for the device lifetime, including on timeout; late DMA can never
//! overwrite a frame reassigned to another owner. No RNG interrupt is needed.
use alloc::boxed::Box;
use super::{DmaRegion, Transport, Virtqueue};
struct RngDevice { _transport: Box<dyn Transport>, _queue: Virtqueue, _buffer: DmaRegion }
static DEVICES: spin::Mutex<heapless::Vec<RngDevice, 8>> = spin::Mutex::new(heapless::Vec::new());
pub fn init(transport: Box<dyn Transport>) {
    if DEVICES.lock().is_full() { return; }
    transport.set_guest_features(0);
    let size = transport.queue_max(0);
    if size == 0 { return; }
    let mut queue = Virtqueue::new(size);
    transport.setup_queue(0, &queue);
    transport.driver_ok();
    let buffer = super::dma_alloc(1);
    let head = queue.submit(&[(buffer.phys, 64, true)]).expect("new RNG queue");
    transport.notify(0);
    let mut completion = None;
    for _ in 0..100 {
        if let Some(used) = queue.pop_used_checked(1, Some(head)) { completion = Some(used); break; }
        if queue.is_broken() { break; }
        crate::arch::delay_ms(1);
    }
    if let Some((_, len)) = completion.filter(|(_, len)| (16..=64).contains(len) && len % 8 == 0) {
        let mut sample = [0u8; 64];
        for (i, b) in sample[..len as usize].iter_mut().enumerate() {
            *b = unsafe { buffer.virt.add(i).read_volatile() };
        }
        crate::random::seed("virtio-rng host", &mut sample[..len as usize], 256);
        crate::random::crypto::erase(&mut sample);
        for i in 0..64 { unsafe { buffer.virt.add(i).write_volatile(0); } }
    } else { crate::serial::write_line("[RNG] virtio-rng timed out or returned an invalid completion; no credit"); }
    // The device still owns its queue. Never recycle it without a reset.
    let _ = DEVICES.lock().push(RngDevice { _transport: transport, _queue: queue, _buffer: buffer });
}
