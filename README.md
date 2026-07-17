# MMURTL/RS 🔥

A Rust rewrite of MMURTL (Message-Passing Multi-User Real-Time Kernel) targeting x86_64 long mode.

## Status: Phase 8 (exFAT Filesystem) Complete ✅

- ✅ Bootable via BIOS (UEFI support coming)
- ✅ Serial output on COM1 (115200 8N1)
- ✅ GDT with kernel/user segments + TSS (IST for double faults)
- ✅ Full IDT — all 20 CPU exceptions handled with proper `x86-interrupt` convention
- ✅ PIC (8259) remapped to 0x20/0x28 (fallback; fully masked in APIC mode)
- ✅ Kernel panic handler with file:line + message output
- ✅ Physical memory region enumeration and usable memory counting
- ✅ Preemptive round-robin scheduler + kernel heap
- ✅ ACPI table parsing (RSDP → RSDT/XSDT → MADT)
- ✅ Local APIC: PIT-calibrated LAPIC timer drives the 100 Hz scheduler tick
- ✅ I/O APIC: legacy IRQ routing (keyboard) with interrupt source overrides
- ✅ Multi-core boot: INIT-SIPI-SIPI trampoline brings all APs into long mode
- ✅ SMP scheduling: every CPU runs the scheduler off its own LAPIC timer;
  tasks migrate freely between cores, idle CPUs woken by reschedule IPIs
- ✅ Per-CPU GDT/TSS with dedicated double-fault IST stacks on every core
- ✅ Virtio core: legacy PCI transport, split virtqueues, contiguous DMA allocator
- ✅ Storage: virtio-blk driver with sector read/write (verified end-to-end)
- ✅ Network: virtio-net driver with a live ARP round trip through QEMU user-net
- ✅ Input: PS/2 keyboard driver — scancode set 1 → ASCII with shift, char queue
- ✅ Filesystem: exFAT (mount, list, read, create) — interoperable with Linux
  in both directions, `fsck.exfat`-clean after kernel writes

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
# Optional: test disk for the virtio-blk self-test
qemu-img create -f raw test-disk.img 16M

qemu-system-x86_64 \
    -drive format=raw,file=target/mmurtl-rs-bios.img \
    -drive format=raw,file=test-disk.img,if=virtio \
    -netdev user,id=n0 -device virtio-net-pci,netdev=n0 \
    -serial stdio \
    -m 256M \
    -smp 4
```

(The virtio drive and NIC are optional — the kernel skips those drivers
gracefully when the devices are absent.)

## Project Structure

```
src/
├── main.rs        — Entry point, kernel init sequence
├── serial.rs      — UART 16550 serial output
├── gdt.rs         — Global Descriptor Table + TSS
├── interrupts.rs  — IDT, exception handlers, PIC, timer/keyboard
├── acpi.rs        — ACPI table parsing (RSDP/RSDT/XSDT/MADT)
├── apic.rs        — Local APIC + I/O APIC driver, LAPIC timer, IPIs
├── smp.rs         — Multi-core boot (AP trampoline, INIT-SIPI-SIPI)
├── memory/
│   └── mod.rs     — Frame allocator, paging, kernel heap
├── scheduler/
│   └── mod.rs     — Preemptive round-robin scheduler
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
| 3 | Physical frame allocator, paging, kernel heap | ✅ Done |
| 4 | Scheduler + RQB IPC (message-passing kernel) | ✅ Done |
| 5 | Local APIC, I/O APIC, multi-core boot (SMP) | ✅ Done |
| 6 | SMP scheduling (all CPUs schedule, IPI reschedule) | ✅ Done |
| 7 | Drivers: virtio-blk, virtio-net, PS/2 keyboard | ✅ Done |
| 8 | exFAT filesystem (read + create, Linux-interoperable) | ✅ Done |
| 9 | Real RQB IPC (blocking send/receive/reply) | 🔜 |
| 10 | Userspace + syscalls | 🌱 |

## Memory Management (Phase 3)

### Physical Frame Allocator
- Bitmap-based: 1 bit per 4 KiB frame, supports up to 32 GiB
- Scans bootloader memory map, auto-marks all non-usable regions
- Next-fit allocation strategy with OOM detection
- Reports free/total MiB at boot

### Page Table Management
- Walks 4-level page tables (PML4 → PDP → PD → PT → 4K page)
- `translate_virtual()` — resolves any virtual address to physical
- `map_page()` — maps a 4K page with on-demand intermediate table creation
- `unmap_page()` — unmaps and returns the freed frame
- `query_page()` — checks flags for any mapped page

### Kernel Heap (Bump Allocator)
- 4 MiB initial, auto-extends in 1 MiB chunks
- Backed by frame allocator page mappings (allocates physical pages on demand)
- `#[global_allocator]` enabling Vec, Box, String, format! from Rust's `alloc` crate
- Fast bump-pointer, OOM handled by extension loop

### Boot output:
```
[MEM] Initializing memory manager...
[PAGING] Physical memory offset: 0xffff800000000000
[FRAME] Bitmap at physical 0x1000000, total=8388608, free=8382408 frames (32744 MiB)
[HEAP] Bump allocator at 0xffff900000000000 (4096 KiB)
[MEM] Memory manager initialized: 32744 MiB free / 32768 MiB total
[TEST] Box: 42
[TEST] Vec: [0, 100, 200, 300, 400, 500, 600, 700, 800, 900]
[TEST] String: test-format-42
[TEST] Heap allocation OK!
```

## APIC + SMP (Phase 5)

### ACPI (`acpi.rs`)
- Walks RSDP → RSDT/XSDT → MADT, mapping table pages on demand
- Discovers the Local APIC base, every processor's APIC ID, the I/O APIC,
  and ISA interrupt source overrides

### Local APIC (`apic.rs`)
- xAPIC MMIO mode, mapped uncached through the physical-memory offset
- LAPIC timer calibrated against the PIT (channel 2 one-shot), then run in
  periodic mode to drive the scheduler at 100 Hz — the legacy PIC/PIT path
  remains as a fallback when no MADT is found
- I/O APIC redirection entries route legacy IRQs (keyboard GSI 1) to the BSP
- ICR helpers send INIT and STARTUP IPIs for AP bring-up

### Multi-core boot (`smp.rs`)
- Position-independent trampoline copied to physical `0x8000`: real mode →
  protected mode → long mode (PAE + EFER.LME/NXE + kernel CR3)
- BSP boots APs one at a time with INIT-SIPI-SIPI and a mailbox handshake
  (per-CPU stack, entry point, CPU number)
- Each AP loads the kernel GDT/IDT, enables its Local APIC, reports in, and
  parks ready for future IPIs

Boot output with `-smp 4`:
```
[ACPI] MADT: LAPIC base 0x00000000fee00000, 4 CPU(s), IOAPIC at 0x00000000fec00000, 5 IRQ override(s)
[APIC] LAPIC base 0x00000000fee00000 (version 0x14), BSP APIC ID 0
[IOAPIC] GSI 1 -> vector 33 on APIC ID 0
[PIC] Fully masked (APIC mode)
[APIC] Timer calibrated: 654544 ticks / 10 ms (div 16)
[SMP] Booting 3 AP(s)...
[SMP] Trampoline installed at 0x0000000000008000 (216 bytes)
[SMP] Starting CPU 1 (APIC ID 1)...
[SMP] CPU 1 online (APIC ID 1)
[SMP] Starting CPU 2 (APIC ID 2)...
[SMP] CPU 2 online (APIC ID 2)
[SMP] Starting CPU 3 (APIC ID 3)...
[SMP] CPU 3 online (APIC ID 3)
[SMP] 4 CPU(s) online
```

## SMP Scheduling (Phase 6)

Every online CPU runs the scheduler:

- **One global run queue**, spinlock-protected; each CPU's LAPIC timer fires
  at 100 Hz and enters `schedule_and_switch`. Unpinned tasks migrate freely
  between cores; a global round-robin cursor spreads them out.
- **Per-CPU idle tasks by adoption** — each CPU's boot/park HLT loop is
  adopted into the task list as that CPU's pinned idle task, so there is
  always something to switch to.
- **Race-free context switch**: the scheduler lock is held *across* the
  stack switch (released from the timer asm only after RSP points at the
  new task's stack), so another CPU can never resume a task whose old
  stack is still in use.
- **Reschedule IPIs** (vector 0x30): creating a task kicks an idle CPU so
  the work starts immediately instead of waiting for its next tick.
- **Per-CPU GDT/TSS**: each AP gets its own GDT, TSS, and double-fault IST
  stack (a TSS cannot be shared — `ltr` marks the descriptor busy).
- **Interrupt-safe serial**: the serial lock is taken with interrupts
  disabled, so a printing task can't be preempted while holding it (which
  could deadlock a printing interrupt handler on the same CPU).

Demo output with `-smp 4` — six workers migrating across four cores:
```
[T5 on CPU1] count=0
[T6 on CPU3] count=0
[T7 on CPU0] count=0
[T8 on CPU2] count=0
[T5 on CPU1] count=1
[T5 on CPU0] count=2      ← task 5 migrated from CPU1 to CPU0
[T9 on CPU3] count=1
[T10 on CPU2] count=1
```

## Drivers (Phase 7)

### Virtio core (`virtio/mod.rs`)
- Legacy (0.9.5) virtio-pci transport over the I/O port BAR — QEMU's
  transitional devices (vendor `0x1AF4`) expose this alongside modern
- Split virtqueues in the legacy layout (descriptor table + avail ring,
  used ring on the next page boundary), free-list descriptor management,
  fenced avail-ring publishing
- DMA regions from a new physically-contiguous frame allocator path
  (`allocate_contiguous`), accessed via the phys-offset window

### Storage: virtio-blk (`virtio/blk.rs`)
- 3-descriptor request chains (header → data → status), synchronous with
  polled completion
- `read_sectors` / `write_sectors` API (512-byte sectors, up to 4 KiB per
  request)
- Boot self-test writes a signature to the device's last sector, reads it
  back, and verifies — confirmed from the host side with a hex dump of the
  disk image

### Network: virtio-net (`virtio/net.rs`)
- RX/TX virtqueues, MAC from device config (`VIRTIO_NET_F_MAC`),
  prefilled 2 KiB RX buffers with recycling
- Boot demo does a real ARP round trip through QEMU user networking:
  `ARP who-has 10.0.2.2 tell 10.0.2.15 ... reply: 10.0.2.2 is-at 52:55:0a:00:02:02`

### Input: PS/2 keyboard (`keyboard.rs`)
- IRQ1 feeds raw scancodes to the driver; scancode set 1 → ASCII with
  shift tracking
- Characters land in a lock-free ring buffer consumed by a `kbd_echo`
  task — IRQ on the BSP, consumption on whatever CPU the task runs on:
  `[KBD on CPU1] 'A'`

## exFAT Filesystem (Phase 8)

MMURTL (1994) spoke FAT; MMURTL/RS speaks its modern descendant. The
driver (`fs/exfat.rs`) implements the on-disk format from the Microsoft
exFAT specification, on top of the virtio-blk driver:

- **Mount**: boot sector parse (FAT offset, cluster heap, root directory),
  allocation bitmap + volume label discovery from the root directory
- **Read**: directory entry sets (File 0x85 + Stream 0xC0 + FileName 0xC1),
  FAT cluster chains *and* the NoFatChain contiguous fast path
- **Create**: contiguous NoFatChain allocation from the allocation bitmap,
  directory entry sets with correct rotate-right set checksums, up-cased
  name hashes, and exFAT timestamps

Verified end to end against the reference implementations:
```
[FS] exFAT mounted: label "MMURTL", 3584 clusters x 4 KiB, root @ cluster 5
[FS]   README.TXT  (55 bytes)          ← written by Linux, read by MMURTL/RS
[FS] Created MMURTL.TXT (108 bytes), read-back verified
```
- `fsck.exfat` reports the volume **clean** after kernel writes
- Linux mounts the image and reads `MMURTL.TXT` — content, size, and
  timestamps all intact
- A second kernel boot finds and reads the file it created (persistence)

Current limitations: root-directory files only, ASCII names, no
delete/rename/append, 512-byte sectors.

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
