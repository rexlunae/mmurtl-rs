# MMURTL/RS 🔥

A Rust rewrite of MMURTL (Message-Passing Multi-User Real-Time Kernel),
running on **amd64** (x86_64 long mode) and **arm64** (AArch64).

## Status: roadmap finished ✅ — now on two architectures

Both ports run the same kernel: the scheduler, blocking RQB IPC, memory
management, virtio drivers, exFAT, syscalls, and userspace are shared
code, and each architecture supplies only its hardware layer (see
[Architecture ports](#architecture-ports-amd64--arm64)). The feature list below
is common to both unless marked.

- ✅ **(amd64)** Bootable via BIOS (UEFI support coming)
- ✅ **(amd64)** Serial output on COM1 (115200 8N1)
- ✅ **(amd64)** GDT with kernel/user segments + TSS (IST for double faults)
- ✅ **(amd64)** Full IDT — all 20 CPU exceptions handled with proper `x86-interrupt` convention
- ✅ **(amd64)** PIC (8259) remapped to 0x20/0x28 (fallback; fully masked in APIC mode)
- ✅ Kernel panic handler with file:line + message output
- ✅ Physical memory region enumeration and usable memory counting
- ✅ Preemptive round-robin scheduler + kernel heap
- ✅ **(amd64)** ACPI table parsing (RSDP → RSDT/XSDT → MADT)
- ✅ **(amd64)** Local APIC: PIT-calibrated LAPIC timer drives the 100 Hz scheduler tick
- ✅ **(amd64)** I/O APIC: legacy IRQ routing (keyboard) with interrupt source overrides
- ✅ **(amd64)** Multi-core boot: INIT-SIPI-SIPI trampoline brings all APs into long mode
- ✅ SMP scheduling: every CPU runs the scheduler off its own local timer;
  tasks migrate freely between cores, idle CPUs woken by reschedule IPIs
- ✅ **(amd64)** Per-CPU GDT/TSS with dedicated double-fault IST stacks on every core
- ✅ **(amd64)** Virtio core: legacy PCI transport, split virtqueues, contiguous DMA allocator
- ✅ Storage: virtio-blk driver with sector read/write (verified end-to-end)
- ✅ Network: virtio-net driver with a live ARP round trip through QEMU user-net
- ✅ **(arm64)** Boots on QEMU `virt` from an ELF at EL1 (or EL2); PL011
  console; RAM, CPUs, and devices from the device tree
- ✅ **(arm64)** Identity-mapped MMU, EL1 vector table, GICv2/GICv3 + generic
  timer tick, PSCI multi-core boot, virtio-mmio (legacy + modern)
- ✅ **(amd64)** Input: PS/2 keyboard driver — scancode set 1 → ASCII with shift, char queue
- ✅ Filesystem: exFAT — full API: subdirectories, mkdir, create, read,
  overwrite, append, delete; interoperable with Linux in both directions,
  `fsck.exfat`-clean after kernel writes
- ✅ IPC: blocking RQB message passing — `send_rqb` / `receive_rqb` /
  `reply_rqb` with per-task inboxes, a service name registry, and real
  blocking (`sleep_ms`, voluntary yield) instead of busy-waiting
- ✅ Userspace: user-mode tasks (ring 3 / EL0) with their own user pages,
  a syscall trap (`int 0x80` / `svc #0`) with validated user pointers, and
  fault isolation — a
  misbehaving program is killed, the kernel keeps running

## Building

```bash
# amd64: build the kernel and create BIOS/UEFI boot images
# (runs tools/patch-bootloader-deps.sh first: bootloader 0.11.17's stage
# builds need two small source patches to compile on the pinned nightly)
make bios

# arm64: build the kernel ELF (QEMU loads it directly)
make arm64
```

## Running (requires QEMU)

### arm64

```bash
make run-arm64              # QEMU virt, GICv3, 4 CPUs (ARM64_SMP=N, ARM64_GIC=2|3)
make arm64-image            # target/mmurtl-rs-arm64.Image (Linux Image format)

# With a disk and NIC (virtio-mmio):
qemu-system-aarch64 -machine virt,gic-version=3 -cpu cortex-a72 -smp 4 -m 256M \
    -nographic -kernel target/aarch64-unknown-none-softfloat/release/mmurtl-rs \
    -drive if=none,format=raw,file=test-disk.img,id=hd0 -device virtio-blk-device,drive=hd0 \
    -netdev user,id=n0 -device virtio-net-device,netdev=n0
# ...or over PCIe instead: -device virtio-blk-pci,drive=hd0 -device virtio-net-pci,netdev=n0
```

### amd64

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
├── main.rs            — kernel_run(): shared second half of boot, demo tasks
├── serial.rs          — console (locked, over the arch UART)
├── keyboard.rs        — console input queue (+ amd64 PS/2 translation)
├── syscall.rs         — syscall dispatch (behind each arch's trap stub)
├── userspace.rs       — user-program loading + kernel-side checks
├── memory/            — frame allocator, heap, user window
├── scheduler/         — SMP scheduler + blocking RQB IPC primitives
├── ipc/               — service registry + IPC demo
├── pci.rs             — PCI enumeration + BAR assignment
├── virtio/            — virtqueues + Transport trait, blk + net, virtio-pci
├── fs/exfat.rs        — exFAT filesystem
└── arch/
    ├── mod.rs         — the architecture interface
    ├── amd64/         — bootloader entry, 16550, GDT/IDT, APIC, ACPI,
    │                    SMP trampoline, paging, port I/O, xHCI,
    │                    int 0x80, ring-3 programs
    └── arm64/         — _start + linker script, PL011, device tree, MMU,
                         vectors, GICv2/v3 + timer, PSCI SMP, virtio-mmio, PCIe,
                         svc #0, EL0 programs
user/                  — user-program runtime + Rust programs (ELF)
tools/                 — boot-image builder, disk-image + dependency scripts
```

## Architecture

Originally by Richard Burgess (1994):

- **Message-passing IPC** via Request Blocks (RQBs) — synchronous send/receive
- **Cooperative multitasking** with priority queues
- **Flat memory model** (we use 4-level paging on both architectures)
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
| 8 | exFAT filesystem (full read/write API, Linux-interoperable) | ✅ Done |
| 9 | Real RQB IPC (blocking send/receive/reply) | ✅ Done |
| 10 | Userspace + syscalls (ring 3, int 0x80) | ✅ Done |
| — | **arm64 port** (shared core + arch layer) | ✅ Done |

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
  FAT cluster chains *and* the NoFatChain contiguous fast path, path
  traversal through subdirectories
- **Write API**: `create_file`, `write_file` (overwrite + cluster
  realloc), `append_file`, `delete` (files and empty directories, with
  bitmap + FAT cleanup), `mkdir` — entry sets carry correct rotate-right
  checksums, up-cased name hashes, and exFAT timestamps
- **Directory management**: reuses runs of deleted entries, appends at the
  end marker, and grows the root directory by extending its FAT chain

Verified end to end against the reference implementations:
```
[FS] HOST_DIR/HELLO.TXT: Nested file seeded by the host.
[FS] mkdir DOCS: created
[FS] DOCS/BOOTLOG.TXT now records 3 boot(s)      ← appends once per boot
[FS] create+delete TEMP.TXT: OK (gone after delete)
[FS] overwrite DOCS/OVERWRITE.TXT: verified
[FS] stress: 50 creates (root dir grown) + 50 deletes OK
```
- `fsck.exfat` reports the volume **clean** after every boot's mutations
- Linux mounts the image: `DOCS/BOOTLOG.TXT` shows one line per kernel
  boot, the deleted file is gone, timestamps and sizes intact

Current limitations: ASCII names (≤ 255 chars), no rename, overwrites
reallocate contiguously, non-root directories have fixed capacity,
512-byte sectors.

## Blocking RQB IPC (Phase 9)

MMURTL's defining feature — synchronous Request/Respond message passing —
implemented as real task-state transitions in the SMP scheduler:

| Primitive | Behavior |
|---|---|
| `send_rqb(tid, &mut rqb)` | copies the request into the receiver's inbox, blocks the sender (`WaitingReply`) until the reply lands in its reply slot; returns the reply's status |
| `receive_rqb()` | pops the oldest pending request, blocking (`WaitingRqb`) while the inbox is empty |
| `reply_rqb(sender, &rqb)` | fills the blocked sender's reply slot and makes it Ready |
| `sleep_ms(ms)` | blocks against the system clock (CPU 0's tick) |
| `ipc::register_service` / `lookup_service` | MMURTL-style named services |

Blocked tasks consume no CPU. Every blocking primitive gives up the CPU
through a **voluntary-yield vector** (0x31) that shares the timer's
save/switch path but sends no EOI.

Correctness under SMP:
- **`on_cpu` guard**: a task can be woken (e.g. by a reply on another
  core) while it is still executing on its way into a yield. The scheduler
  never picks a task that is still on a CPU, so its stale saved context
  can't be resumed twice.
- **Lost-wakeup free**: "check inbox / reply slot, else block" happens
  under the scheduler lock, and the matching send/reply also runs under it.
- **Failure is explicit, never a hang**: sending to an unknown or exited
  task → `NotFound`; to itself → `InvalidParam`; to a full inbox →
  `Busy`; and a receiver that exits fails every request it still owes
  with `Aborted`.
- **Wakeups spread across cores**: when a tick wakes several sleepers,
  idle CPUs are kicked with reschedule IPIs.

Boot demo (`-smp 4`) — a text service, three concurrent clients, and an
error-path checker:
```
[IPC] client T10 (CPU0): 5/5 round trips verified, last reply "T10 MSG 4"
[IPC] client T11 (CPU0): 5/5 round trips verified, last reply "T11 MSG 4"
[IPC] client T12 (CPU1): 5/5 round trips verified, last reply "T12 MSG 4"
[IPC] Error-path checks:
[IPC]   send to self                       -> InvalidParam ✓
[IPC]   send to unknown task               -> NotFound ✓
[IPC]   unknown service code               -> InvalidService ✓
[IPC]   reply to a non-waiting task        -> InvalidParam ✓
[IPC]   sleep_ms(250)                      -> 250 ms ✓
[IPC] textsvc: served 16 requests, shutting down
[IPC]   receiver exits before replying     -> Aborted ✓
[IPC]   send to exited task                -> NotFound ✓
[IPC] ✓ All IPC checks passed
```
Verified at `-smp 1`, `2`, and `4`, and across six concurrent 4-CPU boots.

Limitations: no send timeouts, and the service registry is a flat list.

## Userspace + Syscalls (Phase 10)

Tasks can now run in **ring 3**:

- **User window** (`memory/user.rs`): user programs live in a dedicated
  lower-half region whose pages carry the USER bit — code read+execute,
  stack read+write+no-execute, with unmapped guard pages. Every other
  mapping (kernel image, heap, physical-memory window) stays
  supervisor-only.
- **Per-CPU TSS.RSP0**: on every switch to a user task, the scheduler
  points that CPU's TSS at the task's own kernel stack, so interrupts and
  syscalls from ring 3 land on the right stack — even as tasks migrate.
- **`int 0x80` syscall gate** (DPL 3 — the only gate ring 3 may invoke).
  The handler runs on the caller's kernel stack with interrupts on, so a
  syscall can block in IPC or sleep and be preempted like kernel code.
  The `syscall` instruction stays disabled: it doesn't switch stacks.
- **Validated user pointers**: every pointer argument is checked against
  the user window *and* the page tables (present + user + writable where
  needed) before the kernel touches it; RQBs cross the boundary in an
  explicit 96-byte wire format. STAC/CLAC are used when SMAP is on.
- **Fault isolation**: #PF, #GP, #UD, #DE, or #SS raised by ring-3 code
  kills that task (failing any requests it owes with `Aborted`) instead
  of panicking the kernel.

| # | Syscall | | # | Syscall |
|---|---|---|---|---|
| 0 | `exit()` | | 5 | `get_tid()` |
| 1 | `log(ptr, len)` | | 6 | `sleep_ms(ms)` |
| 2 | `send_rqb(tid, rqb*)` | | 7 | `lookup_service(name*, len)` |
| 3 | `receive_rqb(rqb*)` | | 8 | `register_service(name*, len)` |
| 4 | `reply_rqb(tid, rqb*)` | | 9 | `yield()` |

The demo programs are position-independent assembly blobs, copied into
user pages at boot:
```
[USER T17 CPL3] Hello from ring 3! Asking the kernel's sysinfo service over RQB IPC...
[USER T17 CPL3] MMURTL/RS v0.1.0: 4 CPUs, up 30 ms
[USER T16 CPL3] uecho: user-mode service registered, serving requests
[USER T19 CPL3] rogue_read: reading kernel memory...
[USER] T19 "rogue_read" killed: #PF protection violation at rip=0x640000301016, addr=0x10000013d4d
[USER T20 CPL3] rogue_priv: executing cli...
[USER] T20 "rogue_priv" killed: #GP general protection fault at rip=0x640000401013
[USER T21 CPL3] rogue_ptr: kernel refused a kernel pointer with EFAULT
[USER T18 CPL3] spinner: 150M-iteration ring-3 loop done (preempted, never yielded)
[USER] Userspace checks:
[USER]   kernel -> ring-3 service (3 round trips) ✓
[USER]   hello finished                           ✓
[USER]   spinner preempted in ring 3, finished    ✓
[USER]   rogue_read killed, kernel alive          ✓
[USER]   rogue_priv killed, kernel alive          ✓
[USER]   rogue_ptr refused, exited                ✓
[USER]   ring-3 receiver exits -> sender gets Aborted ✓
[USER] ✓ All userspace checks passed
```
Verified at `-smp 1` (the spinner can only finish alongside everything
else if the timer preempts ring 3), `-smp 2`, and `-smp 4`, including
concurrent runs.

### Per-task address spaces and reclamation

Every user task gets **its own address space**: a page-table root that
shares all of the kernel's mappings but whose user window holds only that
task's pages. The scheduler loads the incoming task's space on every
switch (CR3 on amd64, TTBR0 + a local TLB flush on arm64; kernel tasks
run in the kernel's own tables). A program that reaches for another
program's memory now finds nothing mapped there:
```
[USER] T23 "rogue_peek" killed: #PF not-present page at pc=0x640000601016          (amd64)
[USER] T23 "rogue_peek" killed: data abort (translation fault) at pc=0x640000601018 (arm64)
```

**Exited tasks are reclaimed.** Kernel stacks now come from the frame
allocator, and a reaper task frees an exited task's stack, user pages,
page tables, and user-window slot once no CPU is running it — which is
guaranteed after it has been switched out, since every switch loads the
incoming task's space. Task slots are reused. A churn test runs 16
short-lived user tasks and checks that every frame comes back (heap
growth is accounted for separately, so it can't mask a leak):
```
[USER]   16 churn tasks reclaimed (272 frames)      ✓
```

### ELF programs from disk (written in Rust)

User programs no longer have to be assembly blobs inside the kernel. The
`user/` crate is a small runtime (the syscall ABI for both
architectures, RQB messages, `println!`, `entry!`) plus programs written
in ordinary Rust, built as ELF executables linked into the user window.
At boot the kernel loads every `*.ELF` in `/BIN` on the exFAT disk,
each into its own address space:

```bash
make user && make disk          # amd64: builds user/ and disk-amd64.img
make user-arm64 && make disk-arm64
```
```
[USER] Loaded /BIN/HELLO.ELF (19104 bytes) as T24, entry 0x640003000000
[USER T24 CPL3] Rust ELF program running as task 24
[USER T24 CPL3] sysinfo (task 15) replied, status 0: MMURTL/RS v0.1.0: 4 CPUs, up 0 ms
[USER T24 CPL3] longest Collatz chain below 20000 starts at 17647 (279 steps)
[USER T25 CPL3] sieve: 1229 primes below 10000 (data segment says 8)
[USER]   2 ELF programs from /BIN ran to completion ✓
[USER]   ELF loader: accepts valid, rejects 5 bad ✓
```

The loader treats the image as untrusted: it checks the ELF identity,
type, and machine, requires every `PT_LOAD` segment to lie inside the
user window (below the stack) and inside the file, refuses
writable+executable segments and segments that share a page, and
requires the entry point to be in an executable segment — all before
mapping anything. Segments are mapped with exactly their permissions,
zero-filled past their file data (`.bss`).

Limitations: the kernel heap is a bump allocator, so small kernel-side
task metadata is not recycled; no ASIDs on arm64 (each address-space
switch flushes the local TLB); ELF programs are static executables (no
dynamic linking or relocation).

## Architecture ports (amd64 + arm64)

The kernel is split into a **portable core** and an **architecture
layer** (`src/arch/`). The core — scheduler, IPC, frame allocator and
heap, user-memory validation, virtio blk/net drivers, exFAT, syscall
dispatch, userspace checks — is identical on both ports; each port
implements the interface documented in `src/arch/mod.rs` (console, CPU
identity and IPIs, timer tick, task contexts and yield, physical memory
translation, heap placement, user page mapping, user programs) and its
own boot path.

| | amd64 | arm64 |
|---|---|---|
| Boot | `bootloader` crate (BIOS/UEFI), long mode | ELF at 0x4020_0000, EL1 (drops from EL2) |
| Discovery | ACPI MADT, PCI | Device tree (RAM, CPUs, PSCI, GIC, UART, virtio, PCIe host bridge) |
| Console | 16550 COM1 | PL011 (RX interrupt feeds console input) |
| Paging | bootloader tables + offset window | own identity map; EL0/EL1 AP bits, PXN/UXN |
| Interrupts | IDT; PIC → Local/I/O APIC | EL1 vector table; GICv2 or GICv3 (redistributors, ICC system registers) |
| Tick | LAPIC timer (PIT fallback) | generic virtual timer (PPI 27) |
| Reschedule IPI | vector 0x30 | SGI 1 (GICv3: routed by MPIDR affinity) |
| Yield | `int 0x31` | `svc` from EL1 |
| Multi-core | INIT-SIPI-SIPI trampoline | PSCI `CPU_ON` (HVC/SMC per DT) |
| Syscalls | `int 0x80` (DPL 3), TSS.RSP0 per task | `svc #0` from EL0 (x8 = number) |
| User isolation | U/S bit, NX, SMAP-aware | AP[7:6], PXN/UXN; UMA=0 traps DAIF |
| PCI | config via 0xCF8/0xCFC; firmware-assigned BARs | ECAM from the DT; kernel assigns BARs; I/O space via the bridge window |
| virtio | legacy virtio-pci (port I/O) | virtio-mmio v1 (legacy) and v2, and the same legacy virtio-pci driver |

Both ports run the same boot demo end to end — storage self-test, ARP,
exFAT, every IPC check, and every userspace check (including the rogue
programs being killed). The arm64 port is verified on QEMU `virt` at 1,
2, 4, and 8 CPUs, with `-cpu cortex-a72` and `-cpu max`, entered at EL1
or EL2, and over both legacy and modern virtio-mmio:
```
[DTB] Device tree at 0x0000000040000000: 4 CPU(s), 32 virtio-mmio slots, PSCI via HVC
[GIC] GICv2: distributor 0x0000000008000000, CPU interface 0x0000000008010000; timer INTID 27 @ 62 MHz
[SMP] CPU 1 online (MPIDR 0x1), scheduling
[BLK] virtio-blk (virtio-mmio v1 (legacy)) ready: 32768 sectors (16384 KiB), queue size 256
[USER T14 EL0] Hello from EL0! Asking the kernel's sysinfo service over RQB IPC...
[USER] T16 "rogue_read" killed: data abort (permission fault) at pc=0x640000301018, addr=0x40223818
[USER] T17 "rogue_priv" killed: trapped system instruction at pc=0x640000401014
[IPC] ✓ All IPC checks passed
[USER] ✓ All userspace checks passed
```
The same exFAT disk image can be booted alternately on both: its
per-boot log keeps counting across architectures and stays
`fsck.exfat`-clean.

With GICv3 the port runs well past GICv2's 8-CPU limit — verified at
16 and 32 CPUs, where CPUs 16-31 sit in a second affinity cluster
(MPIDR 0x100+) and are reached by affinity-routed SGIs:
```
[GIC] GICv3: distributor 0x8000000, redistributors 0x80a0000 (+0xf60000); timer INTID 27 @ 62 MHz
[SMP] CPU 16 online (MPIDR 0x100), scheduling
[SMP] 32 CPU(s) online
```

PCI enumeration and the legacy virtio-pci transport are shared code: on
arm64 the kernel maps the ECAM window, sizes and assigns BARs from the
host bridge's I/O and 32-bit memory windows, and reaches PCI I/O space
through the bridge's memory-mapped I/O window — so `virtio-blk-pci` and
`virtio-net-pci` work there exactly as on amd64:
```
[PCI] ECAM host bridge at 0x4010000000 (buses 0-255); I/O window 0x3eff0000, MMIO window 0x10000000; 6 BARs assigned
[BLK] virtio-blk (legacy virtio-pci) ready: 32768 sectors (16384 KiB), queue size 256
```

### arm64 on real hardware

Steps toward booting on physical boards (QEMU can boot these paths but
does not model caches, so the cache fixes are unverified on silicon):

- **Standard `Image` format**: the kernel starts with the arm64 Linux
  `Image` header, so U-Boot `booti`, firmware, or `qemu -kernel` can load
  `make arm64-image`'s output and pass the device tree in `x0` (the ELF
  still boots too).
- **Cache maintenance**: the kernel image range is invalidated to the
  point of coherency before caches are enabled (so no pre-boot cache line
  can shadow `.bss`, the stack, or the page tables written with caches
  off); every CPU invalidates its I-cache after enabling the MMU; and
  code the kernel copies into user pages is cleaned to the point of
  unification with the I-caches invalidated before it runs — ARM's
  I-cache does not snoop data writes, so without this a user program
  could execute stale instructions.
- Already hardware-shaped: everything comes from the device tree (RAM,
  CPUs, GIC v2/v3, UART, timer interrupt, PCIe), PSCI via HVC or SMC,
  EL2 entry.

What a Raspberry Pi 4 would still need (not done, and untested on real
hardware): a **relocatable load address** — the kernel is linked and
identity-mapped at 0x4020_0000 (RAM at 1 GiB, as on QEMU `virt`), while
the Pi's RAM starts at 0; **spin-table SMP** (the Pi firmware's default
secondary-CPU release method; only PSCI is implemented); and UART
clock/pin setup if the firmware doesn't leave the PL011 configured. The
Pi 4's GIC-400 is a GICv2 and is supported; a Pi 3 has no GIC at all.

arm64 limitations: RAM beyond the first 4 GiB above 1 GiB is ignored;
GICv3 support covers the first redistributor region (no ITS/LPIs); PCI
covers bus 0 (no bridges) and legacy virtio-pci (no modern
capability-based transport).

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
