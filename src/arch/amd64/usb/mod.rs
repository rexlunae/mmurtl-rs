//! USB Subsystem — initialization and coordination of USB controllers.

pub mod xhci;
pub mod hid;

/// Initialize the USB subsystem
///
/// Scans PCI bus for USB controllers and initializes them.
/// For now, focuses on xHCI (USB 3.x) controllers.
pub fn init() {
    crate::serial::write_str("[USB] Initializing USB subsystem...\n");

    // Discover USB controllers on PCI bus
    xhci::init_xhci();

    crate::serial::write_str("[USB] USB subsystem initialized.\n");
}
