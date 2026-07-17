# MMURTL/RS 🔥

A Rust rewrite of MMURTL (Message-Passing Multi-User Real-Time Kernel) targeting x86_64 long mode.

## Status: Phase 1 Complete ✅

- ✅ Bootable via BIOS (UEFI support coming)
- ✅ Serial output on COM1 (115200 8N1)
- ✅ GDT with kernel/user segments + TSS (IST for double faults)
- ✅ Full IDT — all 20 CPU exceptions handled with proper `x86-interrupt` convention
- ✅ PIC (8259) remapped to 0x20/0x28
- ✅ PIT timer handler + PS/2 keyboard scancode logging
- ✅ Kernel panic handler with file:line + message output
- ✅ Physical memory region enumeration and usable memory counting

## Building

```bash
# Build the kernel
cargo build -Z build-std=core,compiler_builtins,alloc \
    --target x86_64-unknown-none.json --release

# Create bootable BIOS image
/tmp/mmurtl-builder/target/release/mmurtl-builder \
    target/x86_64-unknown-none/release/mmurtl-rs \
    target/mmurtl-rs-bios.img
```

## Running (requires QEMU)

```bash
qemu-system-x86_64 \
    -drive format=raw,file=target/mmurtl-rs-bios.img \
    -serial stdio \
    -m 256M
```

## Project Structure

```
src/
├── main.rs        — Entry point, kernel init sequence
├── serial.rs      — UART 16550 serial output
├── gdt.rs         — Global Descriptor Table + TSS
├── interrupts.rs  — IDT, exception handlers, PIC, timer/keyboard
├── memory/
│   └── mod.rs     — Memory management (stub)
├── scheduler/
│   └── mod.rs     — Task scheduler (stub)
└── ipc/
    └── mod.rs     — RQB message-passing IPC (stub)
```

## Architecture

Originally by Richard Burgess (1994):

- **Message-passing IPC** via Request Blocks (RQBs) — synchronous send/receive
- **Cooperative multitasking** with priority queues
- **Flat memory model** (we use x86_64 paging + long mode)
- **Minimal kernel** — most services run as tasks

## Phase Roadmap

| Phase | What | Status |
|-------|------|--------|
| 1 | Boot, serial, interrupts, GDT/IDT, PIC | ✅ Done |
| 2 | PCI bus scanning + xHCI USB driver skeleton | ✅ Done |
| 3 | Memory management (frame allocator, paging, heap) | 🔜 |
| 4 | Scheduler + RQB IPC (message-passing kernel) | 🌱 |
| 5 | Drivers (storage, network, input) | 🌱 |
| 6 | Userspace + syscalls | 🌱 |

## USB Driver (xHCI)

- PCI bus scan for USB controllers (0x0C:0x03)
- xHCI register structures: capability, operational, port, doorbell, runtime
- Controller init + reset + start + BIOS handoff
- Port detection and speed reporting (Low, Full, High, Super)
- Command ring management with TRB enqueue
- Device context structures (Slot, Endpoint, Input contexts)
- Control transfer TRB builders (Setup, Data, Status stages)
- HID keyboard report parsing (boot protocol)
- HID mouse report parsing
- USB HID usage → ASCII translation table
- Keyboard state machine (press/release detection)

**Limitations (Phase 2):**
- Uses fixed memory addresses (0x1000/0x2000) until frame allocator is implemented
- No event ring processing yet (command completion is assumed)
- No interrupt-driven transfers (polling-only for now)
- Only xHCI controllers supported (no UHCI/EHCI fallback)

## License

MMURTL's original license is included in the [bproctor/MMURTL](https://github.com/bproctor/MMURTL) repo.
