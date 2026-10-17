# MMURTL/RS 🔥

A Rust rewrite of MMURTL (Message-Passing Multi-User Real-Time Kernel),
running on **amd64** (x86_64 long mode) and **arm64** (AArch64).

## Status: kernel roadmap complete — interactive CLI on two architectures

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
- ✅ **(arm64)** Relocatable kernel (self-relocating PIE) — boots on QEMU
  `virt` and the Raspberry Pi 3 (`raspi3b`) from an ELF or `Image`, at EL1,
  EL2, or EL3; PL011 console; RAM, CPUs, and devices from the device tree
- ✅ **(arm64)** High-half TTBR1 kernel, ASID-tagged user TTBR0, EL1 vector table, GICv2/GICv3 or the
  Pi's BCM2836 interrupt controller + generic timer tick, PSCI or
  spin-table multi-core boot, virtio-mmio (legacy + modern)
- ✅ **(amd64)** Input: PS/2 keyboard driver — scancode set 1 → ASCII with shift, char queue
- ✅ Filesystems: FAT12/16/32 with VFAT long filenames, plus exFAT — full API: subdirectories, mkdir, create, read,
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

Build and launch the port matching your host architecture automatically:

```bash
./tools/run-qemu.sh
./tools/run-qemu.sh --arch amd64   # override the detected architecture
./tools/run-qemu.sh --arm64        # arm64 QEMU virt, even on an amd64 host
./tools/run-qemu.sh -- -smp 4      # pass additional options to QEMU
./tools/run-qemu.sh --help
```

`--arch` also accepts `x86_64` and `aarch64`. The launcher can run from any
directory and requires `make`, the project's Rust toolchain, and the QEMU
system emulator for the selected architecture. First launch also needs Python 3
and exfatprogs (`mkfs.exfat`, `fsck.exfat`) to create the data disk;
no root privileges or filesystem mounts are needed. amd64 uses BIOS boot;
arm64 uses the kernel ELF directly. Set `ARM64_SMP` and `ARM64_GIC` to
customize the arm64 defaults (4 CPUs and GICv3). Relative paths in extra
QEMU arguments resolve from the directory where you invoke the launcher.

The launcher creates `disk-amd64.img` or `disk-arm64.img` in the repository
on first use, with `/BIN/HELLO.ELF` and `/BIN/SIEVE.ELF`, and attaches it as a
virtio disk. Subsequent launches preserve and reuse it, including files
created in the shell. `--disk path/to/image.img` selects an existing FAT or
exFAT image instead; relative paths use your current directory. Use
`--no-disk` to boot without a data disk or attach one manually through
extra QEMU arguments. `make disk` and `make disk-arm64` also create images,
but refuse to overwrite existing ones. To refresh a disk, keep a backup
and explicitly remove or move the old image before recreating it.

### Interactive shell

After the boot checks finish, the demo workers stop and the CLI displays
`/> ` in the **serial terminal where QEMU was launched**. Type `HELP` for
commands. The amd64 graphical window still displays the bootloader's last
message: there is no framebuffer console yet. Hide that window with
`./tools/run-qemu.sh -- -display none`. amd64 accepts input from either
the serial terminal or QEMU's PS/2 keyboard; arm64 uses its serial UART.

The CLI is a Rust adaptation of the original
[MMURTL CLI](https://github.com/bproctor/MMURTL/blob/master/MSAMPLES/CLI/CLI.C),
currently running as a kernel task. Commands are case insensitive and
paths may use `/` or `\`; quote arguments containing spaces. Backspace
edits the input, and Ctrl-C/Ctrl-U in the serial terminal cancel the line.
UART receive interrupts keep collecting input during foreground commands;
if bytes are lost, queued input is discarded through a new Enter boundary.
`PS` uses a fixed snapshot buffer, with no per-command heap allocation.

```text
/> HELP
/> VER
/> MEM
/> PS
/> DIR
/> MD notes
/> PATH notes
/notes> WRITE greeting.txt "Hello from MMURTL"
/notes> TYPE greeting.txt
/notes> COPY greeting.txt backup.txt
/notes> DUMP backup.txt
/notes> DEL backup.txt
/notes> CD /
/> RUN hello
```

The default launcher supplies the exFAT disk and user programs automatically.
To select another existing disk:

```bash
./tools/run-qemu.sh --amd64 --disk my-amd64-disk.img
./tools/run-qemu.sh --arm64 --disk my-arm64-disk.img
```

`RUN` loads a native ELF program, waits for it to exit, and returns to the
prompt. A bare program name works too: `hello` searches the current
directory and then `/BIN`, adding `.ELF` when no extension is given.
Programs in `/BIN` also continue to run once during the existing boot
tests. `RD` removes empty directories; `CLS` clears an ANSI terminal.
Aliases include `LS`, `CD`, `PWD`, `CAT`, `RM`, `MKDIR`, and `RMDIR`.

This is an initial CLI port: legacy `.RUN` binaries, program arguments,
batch files, wildcards, history, `REN`, and the original monitor/debugger
commands are not implemented. Ctrl-C cancels input; it does not interrupt
a running program. There is one mounted volume, with no drive letters.
File reads are limited to 1 MiB per command; the current kernel bump
allocator does not reclaim filesystem allocations, so long shell sessions
consume memory. Moving the CLI to userspace requires console and filesystem
syscalls that are not yet implemented.

Shell validation (requires QEMU and exfatprogs; uses disposable test disks):

```bash
rustc --edition=2021 --test src/shell/parse.rs -o /tmp/mmurtl-shell-tests
/tmp/mmurtl-shell-tests
make bios user
python3 tools/test-shell.py --arch amd64 --cpus 4
make arm64 user-arm64
python3 tools/test-shell.py --arch arm64 --cpus 4
```

### FAT and EFI volumes

The filesystem layer supports **FAT12, FAT16, FAT32, and exFAT** on the
current virtio block device. FAT discovery recognizes whole-volume images,
primary MBR partitions, and GPT partitions, including EFI system partitions.
There is one mounted filesystem at `/`; drive letters and multiple mounts
are not implemented. EFI system partitions use FAT; the kernel accesses
that format directly rather than relying on the firmware's boot-time file
protocol. See the [UEFI media-access specification](https://uefi.org/specs/UEFI/2.10/13_Protocols_Media_Access.html).

FAT operations include listing, reading, creating, overwriting, appending,
deleting files and empty directories, and creating/growing directories.
VFAT long names support up to 255 UTF-16 code units, including Unicode
and spaces, with checksum/sequence validation and unique 8.3 aliases.
The serial shell accepts UTF-8 names (quote names containing spaces);
its input line and resolved paths remain limited to 256 UTF-8 bytes.
Short-name lookup supports DOS case flags and the default CP437 code page.
exFAT currently supports ASCII names only.

To try an empty FAT32 disk (requires dosfstools):

```bash
truncate -s 64M fat32.img
mkfs.fat -F 32 fat32.img
./tools/run-qemu.sh --disk fat32.img
# At the prompt: WRITE "Long filename.txt" "hello", then TYPE "Long filename.txt"
```

FAT currently requires 512-byte sectors, limits the cached FAT and loaded
directories to 8 MiB each, and limits individual file operations to 16 MiB
(the shell has a stricter 1 MiB read limit). FAT12/16 fixed root directories
cannot grow. Extended MBR partitions, other OEM code pages, rename, and
journaling are not implemented. New entries use a fixed valid timestamp
until an RTC service exists. Read-only file attributes are honored; disk
write failures stop further operations on that volume until remounting.
The block layer still supports virtio only; FAT support does not add IDE,
AHCI, or NVMe hardware drivers.

Additional validation:

```bash
python3 tools/test-launcher.py       # disk creation/selection and preservation
python3 tools/test-fat.py            # host FAT tests; requires dosfstools
rustc --edition=2021 --test tools/tests/console_input.rs -o /tmp/mmurtl-input-tests
/tmp/mmurtl-input-tests
python3 tools/test-shell.py --arch amd64 --filesystem fat12
python3 tools/test-shell.py --arch arm64 --filesystem fat16
python3 tools/test-shell.py --arch arm64 --filesystem fat32
```

### arm64

```bash
make run-arm64              # QEMU virt, GICv3, 4 CPUs (ARM64_SMP=N, ARM64_GIC=2|3)
make arm64-image            # target/mmurtl-rs-arm64.Image (Linux Image format)
make run-rpi3               # QEMU raspi3b, 4 CPUs (needs the Pi firmware's
                            # bcm2710-rpi-3-b.dtb; RPI3_DTB=path)

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
holds only that task's user pages. amd64 roots share kernel mappings; arm64
uses a separate shared kernel TTBR1. The scheduler loads CR3 on amd64 or
TTBR0 with an ASID on arm64. Normal arm64 switches retain cached translations;
kernel tasks use a reserved empty TTBR0. A program that reaches for another
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

High-resolution timer reads require the kernel-granted `HIGH_RES_TIME`
task capability, denied by default and applied on every context switch.
The kernel creation API grants it before making a task runnable; disk ELF
programs and user syscalls cannot grant it. amd64 enforces CR4.TSD and
clears PCE; arm64 sets CNTKCTL_EL1 explicitly and disables user PMU access
where a PMU exists. The capability allows counter reads, not timer
programming, PMU, power/energy MSRs, or voltage/frequency controls.
`SYS_CLOCK_MS` (10) returns uptime at scheduler tick resolution for every
task. Restricting timers raises the cost of timing attacks but does not
eliminate channels such as counting threads on another CPU.
On amd64, user debug, alignment, x87 and SIMD exceptions terminate the
faulting task. Kernel single-step traps clear TF and debug status. Dedicated
per-CPU supervisor RW/NX stacks with unmapped guards handle #DF, #NMI, #MC,
and #DB. NMI handling takes no locks; fatal machine checks emit valid MCA
bank diagnostics through bounded emergency serial writes and halt the CPU.
The interrupt syscall gate remains in use; SYSCALL/SYSRET is disabled.
Boot probes exercise user single-step and MOV-SS deferred debug delivery.
The alignment probe uses mapped unaligned memory with AC set, followed by
UD2 so QEMU TCG versions that omit alignment checking still terminate it.
Actual #MF/#XM delivery remains unavailable while FP/SIMD is disabled.

The syscall boundary masks dispatch indices, stack-buffer lengths, RQB
lengths, and user-copy pointers with opaque assembly comparisons. Invalid
pointers become the unmapped end of the user window. amd64 uses CMP/SBB;
arm64 uses CMP/CSETM plus CSDB. Scheduler lock acquisitions and syscall
dispatch also use a serializing barrier (CPUID on amd64; DSB SY/ISB on
arm64). CPUID is conservative: LFENCE dispatch-serialization support varies
on older AMD CPUs. Task IDs are searched through bounded iterators rather
than used directly as table indices. Service names and log lengths are
masked before slicing local buffers; RQBs are copied once and clamped.

`tools/test-nospec.py` checks boundary/overflow cases and the optimized
mask/barrier sequences at dispatch and both copy routines; CI builds both
architectures and runs it. This is a focused regression audit, not a full
gadget scanner or proof against all transient attacks. QEMU TCG does not
speculate and this environment has no KVM device, so hardware leakage
validation remains outstanding. An uncontended host CAS-lock benchmark
(`tools/tests/nospec_lock_bench.rs`) measured median 3.7 ns without the
barrier and 24.1 ns with CPUID here; these are host measurements, not kernel
or contested-lock latency guarantees.
Store-bypass protection is an own-task setting. User tasks (including
ELFs from disk) request it by default; kernel tasks default off. Boot
`ssb=per-task` (also the auto default), `ssb=on`, or `ssb=off` selects the
policy. Forced modes reject contradictory task requests with EPERM.
Syscall 11 accepts 0=relax, 1=disable bypass, 2=query the effective requested
policy. Unsupported affected CPUs return EOPNOTSUPP when protection is
requested, rather than claiming success. The user runtime exports
`set_store_bypass_disabled()` and `store_bypass_disabled()`.

amd64 uses CPUID-gated SSBD and PSFD through one cached SPEC_CTRL owner,
preserving IBRS/STIBP and unrelated fields. AMD VIRT_SPEC_CTRL is used when
advertised instead; model-specific LS_CFG programming is not guessed.
Changed controls are read back, with IRQs masked across each update.
arm64 uses saved SPSR.SSBS, sets the kernel exception-entry DSSBS default,
and reapplies policy on every EL0 return after releasing the scheduler
lock. Where SSBS is absent, a supported firmware WA2 backend is selected.
No running task's stale saved-context pointer is modified by the setter.
SSBS is task architectural state; the kernel reestablishes its requested
value at each return, without claiming isolation between code sharing one
user task. Firmware availability and feature gates are required on every
admitted CPU.

A user regression checks invalid operations, forced-mode permissions,
unsupported requests, and policy persistence across sleeps. On SSBS2 it
also reads the actual SSBS register in EL0 after both toggles and switches.
Host tests verify backend gates and preservation of other SPEC_CTRL bits.
TCG does not expose the x86 SSBD controls or a WA2-capable firmware here;
positive x86 MSR and firmware backends still need hardware/KVM validation.

CPU vulnerability reporting records capabilities on every admitted CPU.
amd64 captures gated CPUID leaves 7.0/7.1/7.2, AMD 80000008/80000021, and
IA32_ARCH_CAPABILITIES only when its CPUID gate is present. arm64 records
MIDR, CSV2/CSV3, SSBS, ECBHB, CLRBHB, and probes SMCCC ARCH_FEATURES for
WA1/WA2/WA3 through the device tree's PSCI conduit. SMC32 return values are
sign-extended from the low 32 bits. A small conservative Arm core table
covers the A35/A53/A55 and known Meltdown-unaffected A57/A72/A73/A76;
unknown cores stay unknown unless architectural immunity is advertised.
Spectre-v2 and BHB are reported separately. Controls being advertised
never causes the report to claim they are installed.

The shared policy uses the intersection of features and immunity across
admitted CPUs; late CPUs are parked after finalization. `PatchSite`
provides runtime-selected trusted implementations, with a checked selector
that requires common features, an enabled policy, and an affected family.
Disabled sites make no indirect call. Other mitigation issues add actual
control setup and register installed behavior with the status reporter.

Default policy is `mitigations=auto`. Overrides are `mitigations=off`,
`mitigations=auto,nosmt`, and `<family>=auto|on|off` using the names printed
in the SPEC table (for example `spectre_v2=on ssb=off`). Malformed recognized
settings fall back to auto. Baseline pointer validation, bounds masks,
access protection, and timer restrictions remain enforced with off.
`ssb=per-task` is also supported.
The default admits one hardware thread per core. x86 uses CPUID topology
and ARM uses MPIDR affinity/MT; unknown topology parks secondary CPUs
conservatively. The boot log lists discovered topology and parked siblings.
`nosmt=off` allows all threads, while `nosmt`, `nosmt=on`, and
`mitigations=auto,nosmt` enforce isolation. The latter cannot be cancelled by
`nosmt=off`. `mitigations=off` allows SMT unless explicitly disabled.
Detection depends on the topology exposed by firmware or the hypervisor.
Core scheduling is not implemented: allowing siblings disables simultaneous
cross-thread isolation, even when other speculation mitigations are enabled.

arm64 reads `/chosen/bootargs` (QEMU `-append`); amd64 QEMU reads the bounded
read-only `opt/mmurtl/bootargs` fw_cfg file. Both use `MMURTL_BOOTARGS` at
build time as the embedded fallback for other boot environments. Examples:
```sh
python3 tools/test-security.py --arch amd64 --bootargs 'mitigations=off spectre_v2=on'
python3 tools/test-security.py --arch arm64 --bootargs 'mitigations=auto,nosmt'
MMURTL_BOOTARGS='mitigations=auto' make bios
rustc --edition=2021 tools/tests/cpu_policy.rs -o /tmp/mmurtl-cpu-policy
/tmp/mmurtl-cpu-policy
```
The host regression checks mixed-CPU intersections, missing-capability
fallback, MSR-read gating, policy overrides, SMCCC status widths, and
alternative selection. TCG strips unsupported ARCH_CAPABILITIES requests;
positive MSR decoding is covered with injected host inputs, not claimed as
real-hardware validation. Raw per-CPU words remain available in the log.

amd64 intersects SMEP, SMAP, and UMIP support across online CPUs, then
applies that common policy and CR0.WP before each CPU runs scheduled tasks.
CPU admission is serialized with policy publication; late starters are
parked before loading the shared IDT. AP startup stops on timeout so its
shared low-memory mailbox is never reused while a late CPU may read it.
Loaded read-only image pages (including RELRO) also have read-only NX
physmap aliases, protected before AP startup by splitting huge mappings.
The boot test verifies writes through a text alias fault.
SMAP access decisions are cached; syscall/timer/yield assembly entries
conditionally clear AC before saving registers, and every Rust exception
or IRQ handler clears it before its body. STAC/CLAC include compiler memory
barriers around deliberate user copies. Boot probes verify inherited AC
cannot bypass SMAP, supervisor execution of user pages is denied by SMEP,
and user SIDT is denied where UMIP is supported. Unsupported CPUs use the
baseline user page permissions. LASS is enabled when every admitted CPU
supports LASS and SMAP. Its higher-half code/stack/physical-window prerequisites
are asserted before setting CR4.LASS on each CPU. STAC/CLAC permits deliberate
user data access; unguarded accesses and lower-half instruction fetches fault
before page walking. Exact copy/probe fixups handle #GP(0) as well as #PF,
clear saved AC, and leave unrelated kernel #GP faults fatal.

LASS is selected independently of the transient-execution policy, like the
other baseline access protections. Boot probes automatically exercise its
read/execute fault paths on a supporting CPU. Noncanonical read/write and
private-gate probes validate #GP recovery even without LASS. Host tests cover
mixed CPU capabilities, missing SMAP, empty admission and reserved bits.
The installed QEMU lacks LASS support; hardware enforcement remains untested.
The architecture reports instruction-fetch violations at the branch target,
which is why the armed fetch probe fixes up that target, not its caller
([Intel SDM, linear-address preprocessing](https://cdrdv2-public.intel.com/851064/325384-087-sdm-vol-3abcd.pdf)).

User copies use registered exception-table fixups: amd64 `rep movsb`
inside the SMAP access window, and arm64 `ldtrb`/`sttrb` with EL0 permissions.
Only faults at those exact instructions recover; unrelated kernel faults
still panic. A cheap overflow-safe window check replaces the per-page
validation walk for copies. On failure, copies may have changed a prefix;
RQBs and log buffers are consumed only after the complete copy succeeds.
IPC retains its initial writable-buffer validation so invalid destinations
do not dequeue requests, but revalidates by fault-tolerant copying after
blocking. Remaining page queries include ancestor permission restrictions.
Boot tests cover read-only destinations, partial guard-page faults, stale
validation after deterministic unmapping, and live syscalls returning
EFAULT while the task continues. This does not introduce shared address
spaces or mapping mutations from user mode.

Physical frames are scrubbed with volatile stores before being published
free. All allocation APIs return zeroed frames, including contiguous DMA
buffers and heap-growth claims. A bitmap tracks already-scrubbed free
frames so reuse avoids a redundant erase; untouched firmware RAM is cleared
on first allocation. Allocator metadata and its reservation padding are
also initialized. Scrubbing stays under the frame lock until ownership is
released, preventing reuse races. The boot churn test fills user stacks
with a marker and scans scrubbed free frames after reaping.

Memory encryption remains a boot/firmware integration project: AMD SME
requires consistent C-bit mappings from the bootloader onward; Intel TME
must be configured by firmware. amd64 reports its TME activation/lock/bypass
status when CPUID advertises the MSR, and reports SME support without
changing encryption configuration. Neither is enabled by this kernel.
There is currently no OS shutdown/reboot service. The allocator provides
`scrub_free_memory()` for a future orderly shutdown after CPUs and DMA
are quiesced; that service must also wipe live heap/stack allocations
before transferring control. Scrub-on-free does not erase live allocations
or defeat physical access to running memory.

FP/SIMD instructions are deliberately disabled on every CPU until the
scheduler implements eager per-task FP state save/restore. The policy is
set explicitly on BIOS/UEFI amd64 and on arm64 entry/secondary paths,
including inherited firmware settings. User FP/SIMD attempts are killed
and checked at boot as `rogue_fp` and `rogue_simd`; kernel FP use is a bug.

Limitations: the kernel heap is a bump allocator, so small kernel-side
task metadata is not recycled; ELF programs are static executables (no
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
| Boot | `bootloader` crate (BIOS/UEFI), long mode | ELF or `Image`, loaded anywhere (self-relocating); EL1 (drops from EL2/EL3) |
| Discovery | ACPI MADT, PCI | Device tree (RAM, reserved memory, CPUs, PSCI/spin tables, interrupt controller, UART, virtio, PCIe host bridge) |
| Console | 16550 COM1 | PL011 (RX interrupt feeds console input) |
| Paging | bootloader tables + offset window | high TTBR1 kernel + user-only TTBR0/ASIDs; AP, PXN/UXN |
| Interrupts | IDT; PIC → Local/I/O APIC | EL1 vector table; GICv2 or GICv3 (redistributors, ICC system registers), or BCM2836 local + BCM2835 armctrl (Pi 2/3) |
| Tick | LAPIC timer (PIT fallback) | generic virtual timer (PPI 27) |
| Reschedule IPI | vector 0x30 | SGI 1 (GICv3: routed by MPIDR affinity); BCM2836 mailbox 0 |
| Yield | `int 0x31` | `svc` from EL1 |
| Multi-core | INIT-SIPI-SIPI trampoline | PSCI `CPU_ON` (HVC/SMC per DT) or spin-table release |
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
[IRQ] GICv2: distributor 0x0000000008000000, CPU interface 0x0000000008010000; timer INTID 27 @ 62 MHz
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
[IRQ] GICv3: distributor 0x8000000, redistributors 0x80a0000 (+0xf60000); timer INTID 27 @ 62 MHz
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

- **Relocatable**: the kernel is a position-independent executable
  (linked at 0x4020_0000, all absolute addresses as
  `R_AARCH64_RELATIVE` relocations). `_start` applies them for wherever
  it was loaded, before anything reads an absolute address; the device
  tree is then parsed with the MMU still off and the TTBR1 map built
  from its RAM and device ranges. A second relocation pass rebases references
  into the high kernel window immediately before translation is enabled.
  Verified at the link address, at 0x4100_0000 on `virt`
  (loaded with `-device loader`), and at 0x20_0000
  on the Pi 3:
  ```
  [BOOT] Loaded at 0x200000 (linked at 0x40200000), 269 relocations applied
  [DTB] Device tree at 0x8000000: 4 CPU(s), 0 virtio-mmio slots, SMP via spin-table
  [MEM] RAM 0x0000000000000000 - 0x000000003c000000
  [IRQ] BCM2836 local controller 0x40000000, armctrl 0x3f00b200; UART IRQ 89; counter 62 MHz
  [SMP] CPU 3 online (MPIDR 0x3), scheduling
  [SMP] 4 CPU(s) online
  ```
- **Spin-table SMP** (`enable-method = "spin-table"`, the Raspberry Pi
  firmware's method): the boot CPU writes `secondary_spin_entry`'s address
  to each CPU's `cpu-release-addr`, cleans it to the point of coherency,
  and `sev`s; PSCI is used where the device tree says so.
- **Raspberry Pi 2/3 interrupts**: the BCM2836 per-core controller (the
  virtual timer and mailbox-0 IPIs) and the BCM2835 armctrl controller
  (the PL011's interrupt), verified on QEMU `raspi3b` with the official
  `bcm2710-rpi-3-b.dtb` — every IPC and userspace check passes on 4 CPUs.
  The Pi 4's GIC-400 is a GICv2, which is supported.
- **Device-tree details real boards use**: `ranges` translation (the Pi's
  peripherals are described at bus address 0x7e00_0000), `/memreserve/`
  entries, two- and three-cell interrupt specifiers, RAM that overlaps
  device blocks.
- **EL3 entry** (no firmware below the kernel): drops through EL2 to EL1.
- The PL011's FIFOs are enabled at boot (out of reset it has a
  one-character receive register, and typed input was being dropped).

Not verified on physical hardware; the Pi 4 (RAM above 4 GiB behind a
different peripheral window, GIC-400) has not been run.

arm64 limitations: the high physical window covers RAM/devices below 512 GiB
with 2 MiB blocks (up to 24 distinct GiB of them); RAM not covering a whole
2 MiB block is unused;
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

Security boot regression configurations (build images/kernels first):

```bash
python3 tools/test-security.py --arch amd64 --cpus 4 --nmi
python3 tools/test-security.py --arch amd64 --uefi --cpus 4  # needs OVMF
python3 tools/test-security.py --arch arm64 --cpu max --el2 --cpus 4
```

Frame allocator host regression (dirty simulated RAM and released-frame checks):
```sh
rustc --edition=2021 tools/tests/frame_allocator.rs -o /tmp/mmurtl-frame-test
/tmp/mmurtl-frame-test
```

### Strict kernel W^X

arm64 maps kernel text as EL1 read-only/executable, constants and relocated
GOT/data as read-only/non-executable, and all other RAM (including user-frame
high physical aliases, heap, stacks and page tables) as writable/non-executable.
The image uses 4 KiB pages; ordinary RAM remains in 2 MiB blocks. Relocations
finish before enabling these permissions. Every CPU enables SCTLR_EL1.WXN.

amd64 starts APs through a supervisor RX trampoline, patching its mailbox only
through the NX physical alias. GDT descriptors have their accessed bits set
before paging. Once CPU admission closes, the BSP removes the identity mapping,
makes the physical alias RO/NX, and synchronously flushes all admitted CPUs'
TLBs, including global entries. If an AP timed out and could still be executing
the low trampoline, its RX identity mapping is retained; no writable executable
mapping or writable physical alias remains.

Before launching untrusted tasks, both ports walk the kernel page tables and
reject W+X or user-accessible kernel mappings. Controlled boot fault probes
verify text writes, ordinary RAM execution and execution through a user-code
physical alias are denied. These probes use a private image owned by the boot
test, without exposing a syscall that executes arbitrary kernel pointers.
arm64 also probes rodata and RELRO writes. Run `make arm64-image` and
`python3 tools/test-security.py --arch arm64 --raw-image --cpus 4` to check the
relocated raw Image path as well as the usual ELF boot.

### Kernel randomness

`random::fill` and `random::u64` return an error until at least 128 bits of
conditional source credit are available. `early_u64` works without a heap but
can be predictable; callers must check `ready()` before claiming ASLR or key
security. There is no silent fallback to weak security randomness.

A BLAKE2s pool derives distinct per-CPU ChaCha20 keys. Each 32-byte output
refill replaces its key using the unreturned half of a ChaCha20 block. CPU-local
output runs with IRQs masked and takes no global lock after initialization,
unless a newly mixed seed requires reseeding. NMIs must never call RNG APIs.
Retained keys and temporary output blocks are erased with volatile stores;
RustCrypto's ChaCha state uses zeroize-on-drop. Scalar backends are forced
because kernel FP/SIMD context switching is not yet implemented.

Boot mixes bounded, health-checked RDSEED/RDRAND or RNDRRS/RNDR, discovered
SMCCC TRNG_RND64, firmware DT seeds, and an optional virtio-rng host source.
DT seed properties and completed DMA seed samples are erased after consumption.
Virtqueue free lists and in-flight chain lengths live in CPU-owned metadata;
invalid/duplicate completion IDs or ring-index jumps poison the queue until
reset. Network RX marks the device failed rather than gradually losing posted
buffers. The virtio-rng DMA allocations remain reserved even on timeout, so late DMA
cannot hit recycled memory. Timestamps, normal interrupt timing, and public
ACPI/DT machine data are supplements with **zero entropy credit**. This is not
a validated jitterentropy implementation. Credits are conditional on trusting
the CPU, firmware or VM host RNG; health checks detect simple failures and
cannot prove entropy or repair SRBDS/microcode defects. Credits use the largest
individual source estimate rather than adding potentially correlated sources.
The log reports weak/ready state and a hash of a discarded output, never the
seed or random output itself. Real machines without a trustworthy source stay
weak until one is supplied; future ASLR/canary/PAC users must honor that state.

Run `python3 tools/test-random.py` after building both kernels for cryptographic
known-answer, failure-policy, per-CPU/reseed and scalar-code regressions.
`tools/test-security.py --rng` attaches host entropy; on arm64,
`--no-firmware-seed` disables QEMU's DT seeds for failure-path tests.

### Early amd64 microcode

Supply a vendor container as the bootloader ramdisk with
`MMURTL_MICROCODE=/absolute/path/to/container.bin make bios uefi`.
Use Intel's concatenated binary updates or AMD's `microcode_amd*.bin`
containers, obtained from the processor vendor/linux-firmware. This module
currently occupies the single bootloader ramdisk slot. No firmware payload
is embedded or downloaded automatically.

The BSP loads before vulnerability enumeration and CPU entropy collection;
each AP loads before its feature snapshot or scheduler admission. Previously
started APs spin with interrupts masked until the update phase closes. A
serialized boot seal rejects late AP update attempts before userspace starts.
Virtual machines skip all microcode MSRs and retain host-managed revisions.
On hardware, matching includes Intel signature/platform flags, extended
signature checksums (the runtime-minimum field does not constrain early loading); AMD uses legacy
equivalence tables or exact Zen family/model/stepping encoded in patch IDs.
Updates must be newer. AMD patches additionally require a pinned SHA-256 hash
of vendor-published bytes, and cannot cross a known firmware signing boundary.
Aligned WB update buffers are erased and freed after revision verification.
Recoverable RDMSR/WRMSR #GP fixups handle unavailable or firmware-locked MSRs.
APs install their normal guarded-stack GDT/IDT before handling microcode, under
the CPU-admission seal. Each AP delivers a self-NMI during update preparation
to verify its non-maskable emergency gate while the boot locks are held.

The AMD digest reference is linux-firmware commit
`12dfe670ce7c5987a60fab36d974c4a7d2a6eeeb`, containing 46 patch hashes. The
allowlist is an explicit source-controlled trust root: update it only after
reviewing vendor provenance with `tools/make-microcode-digests.py`. Vendor
CPU authentication remains enabled. BIOS updates remain necessary for signing
format changes and CPUs without OS-distributed patches. The initial advisory
minimum check covers AMD EPYC Rome Zenbleed (AMD-SB-7008, `0x0830107b`);
it is not a complete revision database or a claim that all vulnerabilities
are fixed. The mitigation table still reports the actual post-update CPUID
capabilities. Keep firmware current even when OS microcode is supplied.

Parser checks run in CI via `tools/tests/microcode.rs`; optional arguments
accept local Intel/AMD sample files and additionally test AMD pinned hashes
against modified payloads. Public fixtures were checked from Intel commit
`bdc92abe5c499c3fd7988b76a128d05c9120a520` (`06-3a-09`, `06-55-04`) and the
five AMD family containers in the pinned linux-firmware release. QEMU verifies
all CPU boot/fixup/skip paths; successful real microcode installation requires
bare-metal validation and is not exercised by the VM tests.

### arm64 TTBR1 and ASIDs

The kernel image and the RAM/device physical window have independent randomized
placements in TTBR1. The heap uses the physical window; allocated stacks use
a separate guarded virtual region. The primary stub rebases absolute references
before enabling translation, then branches high. Secondary entry/context and
spin-table release values remain physical for firmware; their stacks and Rust
entry points are high virtual addresses. The temporary TTBR0 maps only physical
text as RX, EL1-only, non-global pages under reserved bootstrap ASID 255.
Every CPU replaces it with the empty ASID-0 TTBR0 immediately after branching
high. Neither reserved tag is allocated to a user process.

Each user root contains only its user window; every user leaf is non-global.
The common admitted-CPU capability selects 8- or 16-bit ASIDs, with TBI0 off.
A generation allocator reserves tags still loaded on other CPUs. Rollover
broadcasts and completes TLBI before reusing retired tags; freeing/unmapping
an image invalidates its ASID before its frames are recycled. Normal context
switches change TTBR0/ASID without flushing the TLB. Kernel mappings are still
present in TTBR1 at EL0 with supervisor-only permissions; KPTI/E0PD remains a separate follow-up.

Boot audits check TTBR1 permissions, the empty kernel TTBR0 and the private
bootstrap map. A real translation test switches through 600 private images
with a forced 8-bit tag limit, checks absent mappings in the other image and
empty TTBR0, and exercises generation rollover. Host tests also stress live
CPU reservations, migration, exhaustion and 16-bit wrap. Validated on QEMU
virt GICv2/v3, EL1/EL2, 1/4/32 CPUs, alternate physical loading, and Raspberry
Pi 3 spin tables. Disk/ELF shell tests and legacy PCI virtio DMA tests pass.
Use `tools/test-security.py --gic 2`, `--load-address 0x41000000`,
`--raspi3-dtb /path/to/bcm2710-rpi-3-b.dtb`, or `--virtio-pci --net --rng`
for those paths. Raw/alternate-address/Pi tests require `make arm64-image`.

### arm64 privileged user access

Hardware PAN is enabled when all admitted CPUs support it, with SPAN cleared
on each CPU and EPAN enabled where available. Otherwise software PAN keeps
TTBR0 on the reserved empty ASID-0 root during kernel execution, including
Cortex-A53/A72 and Raspberry Pi 3. Exception entry closes access before Rust;
EL1 returns stay protected, while EL0 returns restore the saved task root.
Page validation walks the logical task root even while TTBR0 is empty.

User copies use fault-tolerant LDTRB/STTRB loops with UAO cleared. Explicit
access windows mask interrupts and close access before restoring interrupt
state. These windows must not nest or yield. Fault fixups leave access closed.
PAN remains enabled with `mitigations=off`, as a baseline memory boundary.
Boot probes check denied ordinary reads/writes, permitted guarded copies,
rejected kernel pointers, inherited UAO, exception return and execute-only
pages (EPAN or software PAN). Host tests check feature decoding and saved
PSTATE preservation. Hardware PAN does not remove TTBR1 from EL0; KPTI is
tracked separately.

### Kernel address randomization and diagnostics

The amd64 bootloader randomizes its PIE image, physical window, boot info and
boot stack within `0xffff800000000000..0xffffbfffffffffff`. The heap has a
separate, excluded range starting at `0xffffc00000000000`, with 21 bits of
2-MiB placement selected after early microcode and CPU entropy initialization.
Emergency stacks use a separate randomized region beginning at
`0xffffc80000000000`, also excluded from bootloader and heap placement.
The temporary SIPI identity page is retired after AP admission; a timed-out
late AP may still need its read-only executable entry page.

ARM chooses independent image and linear-map regions before enabling
translation. The physical window has 128 possible 512-GiB slots; the image
has 128 separate slots and 17 bits of 2-MiB displacement within its slot.
Relative relocations and AP handoffs carry the image offset; physical table
walks, devices, DMA and heap use the linear offset. The image's physical alias
is NX, with text/constants read-only. Both aliases retain W^X.

Early ARM seeding consumes and erases DT `rng-seed`/`kaslr-seed` before mapping,
and uses RNDR or firmware TRNG where available. These credits assume a trusted
source. With less than 128 credited bits, ARM reports that placement is not
cryptographically assured; the amd64 heap does likewise. The x86 bootloader
uses its own RDRAND/TSC/PIT seed, so its assurance depends on that source too.
A host-backed virtio RNG discovered later cannot retroactively improve the
already selected layout. KASLR alone does not prevent cache/prefetch attacks;
KPTI and other transient-execution mitigations remain separate work.

Routine kernel pointers use keyed BLAKE2s boot-secret identifiers, or `(ptr)`
before a credited RNG is ready. User-fault addresses outside the user window
are restricted the same way. Bootloader routine address logs are suppressed. The lock-free fatal machine-check
logger also redacts RIP, address and auxiliary fields unless diagnostics are enabled.
`make bios arm64-image KERNEL_FEATURES=debug-addresses` explicitly enables raw
address diagnostics. Rebuild without that option before normal use.

`python3 tools/test-kaslr.py --arch amd64 --debug` (or `arm64`) runs 20 complete
boots cycling 1/2/4 CPUs and checks image, physical-map and heap diversity.
After a normal rebuild, omit `--debug` to verify default log pointer hygiene.
Host tests also check canonical ranges, alignment, non-overlap, pointer
redaction and keyed identifier stability. The BIOS compatibility patch gives
stage 4 a dedicated stack because ASLR's RNG overflows its inherited stack,
corrects bootloader 0.11.17's inverted TSC capability gate, and bounds ASLR
offsets to the slots actually reserved (including exactly full PML4 spans).

The fatal logging regression uses a disposable image built with
`make bios KERNEL_FEATURES=test-machine-check` and
`python3 tools/test-machine-check.py`. It populates one synthetic MCA bank
with kernel addresses and enters the real guarded #MC gate, checking that
RIP/ADDR/MISC are restricted. Adding `,debug-addresses` and passing `--debug`
checks the opt-in diagnostic output. Rebuild normally afterward: the test
image intentionally halts instead of starting the shell.

### Entry register and straight-line speculation hygiene

All x86 syscall, timer/yield, exception and device-interrupt gates save the
complete GPR state before clearing live registers. Exception stubs normalize
the error/vector prefix and invoke ordinary Rust handlers with only a trusted
vector, CPU-frame pointer and error code. The borrowed hardware frame carries
fault fixups back to assembly; the common tail restores GPRs and executes
IRETQ. Existing guarded IST stacks remain in use. CLD establishes the Rust
ABI's direction-flag assumption, while the saved user flags are restored.
ARM similarly clears x2–x30 after saving state and closing PAN, before Rust.

LLVM SLS features protect compiled returns and indirect transfers, including
core/alloc. Handwritten x86 RET/IRETQ, GDT far returns and indirect AP handoff paths have INT3
fall-through barriers; ARM RET/ERET/BR paths have baseline DSB NSH; ISB, and
the boot-only BLR fault probe does likewise. ARM compiler indirect-call thunks
use hardened branch tails. These build-time sequences remain present with
`mitigations=off`. They reduce fall-through speculation and available live
operands; predictor isolation and return-prediction attacks remain separate.

`tools/test-entry-sls.py` checks the optimized entry saves/clears, x86 CLD and
transfer barriers throughout executable kernel code. The mixed-mode x86 AP
trampoline is excluded from the general decode, with its 64-bit indirect
handoff checked separately. A user probe reuses the coarse-clock task, seeds
every non-return GPR and flags, invokes a syscall and verifies restoration;
it does not add a task or expose a debug syscall. The x86 probe includes DF=1.
These checks establish emitted instructions and architectural behavior in
QEMU, rather than a hardware side-channel proof.

### ARM predictor hardening

Each CPU selects its own VBAR table from its local capabilities and boot
policy before admission. Architectural CSV2/CSV2.3 immunity, ECBHB, CLRBHB,
known per-core loops and firmware availability are considered separately.
Cortex-A53 keeps plain vectors; Cortex-A57/A72 use k=8. Supported newer ARM
cores use k=24/32/38/132; an unknown core never receives a guessed loop.
CLRBHB is followed by ISB. Loop vectors end with DSB NSH; ISB before entering
common code. Every table is 2-KiB aligned with bounded 128-byte slots, and
all admitted CPUs' VBAR writes are read back and checked.

Firmware vectors preserve x0–x3 before calls, supply zero auxiliary arguments
and restore original user registers before common entry. WA1 combines with
plain, loop or CLRBHB vectors as needed. Firmware also runs on every EL0
return, including same-root syscall/IRQ resumes, before PAN restores user
access and before user GPRs are reloaded. WA1 additionally runs before each
different nonzero root activation, including root reuse. WA3 remains available
for Spectre v2 independently of BHB immunity or its policy, and supersedes
WA1 when selected. No root-cache hit can skip the return barrier.
These choices follow the [Linux ARM predictor backend guidance](https://github.com/torvalds/linux/blob/master/arch/arm64/kernel/proton-pack.c)
and [architectural workaround register preservation](https://github.com/torvalds/linux/blob/master/arch/arm64/include/asm/assembler.h).

Logs report each CPU's selected backend. A missing firmware workaround stays
explicitly vulnerable. A BHB loop alone does not produce a global mitigated
status while prerequisite Spectre-v2 protection is unavailable.
`mitigations=off`, `spectre_v2=off` and `bhb=off` disable their optional
backends; baseline PAN, register clearing and SLS code generation remain.
Host tests cover mixed CPU plans, all supported loop lengths, unknown cores,
feature/conduit gates, immunity and overrides. `tools/test-arm-predictor.py`
audits all vector slots, including firmware argument sanitization and saved
registers. QEMU validates the available plain/loop paths and architectural
behavior; positive firmware and CLRBHB/ECBHB hardware paths remain untested
on this host.

### Guarded kernel stacks and return erasure

Task stacks and CPU runtime/secondary boot stacks occupy separate 64 KiB
virtual slots with 32 KiB of supervisor RW/NX pages and unmapped gaps on
both sides. Independent physical frames back the stack; physical contiguity
is unnecessary. The region begins at a randomized 64 KiB offset within
kernel slot 448, separate from the image, heap and physical window. Early
entropy limitations apply as for the other KASLR placements. Virtual slots
are never reused (the finite 512 GiB region supports millions of lifetimes).
The reaper detaches mappings and empty lower page tables, synchronously
invalidates every admitted CPU, then scrubs and returns their frames. The
permanent root link is established before x86 private address spaces copy it.

x86 overflows enter the guarded double-fault IST. ARM's kernel synchronous
vector checks the stack bounds before its first stack store and can switch
to a per-CPU guarded emergency stack. That check temporarily uses the
kernel-owned TPIDRRO_EL0 register, restoring its normal zero value; no
read-only user TLS ABI is currently provided. Fatal output identifies the
CPU/task without taking scheduler, allocator or serial locks. The initial
firmware/bootstrap stack remains in use only until guarded mappings exist;
the BSP switches stacks before ordinary kernel execution.
APs handle boot IPIs before task execution is released; ordinary scheduling
starts only once the BSP finishes constructing boot tasks. Runtime stack
allocation/reclamation requires interrupts enabled. x86 shootdowns pin the
initiating task to its CPU until all acknowledgments arrive.

User-origin syscalls/exceptions reserve a fresh 16-byte-aligned gap of up to
1008 bytes below the saved register frame. They use the per-CPU CSPRNG when
ready, with a weak early fallback if no credited entropy is available.
Return assembly clears inactive task-stack bytes below the saved frame
after the final Rust call, preserving user registers and flags. The default
`stack-erase` Cargo feature enables erasure, including under
`mitigations=off`; `KERNEL_NO_DEFAULT_FEATURES=1` disables this feature for
overhead comparisons. Saved frames and suspended kernel continuations are
never erased. Stack erasure does not reclaim heap objects or device buffers.

`tools/test-stack-sizes.py` checks emitted optimized frame metadata on both
architectures: ordinary Rust frames must fit 4 KiB, with an explicit 8 KiB
allowance for ARM's early boot function on its 64 KiB bootstrap stack. This
checks individual frames, not cumulative call depth. IDT initialization
occurs in place and the shell's task snapshot uses heap storage to stay
within that budget.

Regression images and commands:
```sh
make bios arm64 KERNEL_FEATURES=test-stack-overflow
python3 tools/test-stack-overflow.py --arch amd64
python3 tools/test-stack-overflow.py --arch arm64
make bios arm64 KERNEL_FEATURES=test-stack-state
python3 tools/test-security.py --arch amd64 --cpus 4 --nmi --require '1000 churn tasks reclaimed'
python3 tools/test-security.py --arch arm64 --cpus 4 --require '1000 churn tasks reclaimed'
make bios arm64  # restore normal images
```
The state regression seeds initialized stack memory, observes it cleared on
the next user entry, checks varying offsets and runs 1,000 short task
lifetimes. Its frame accounting includes stack page-table reclamation;
existing bump-heap growth remains separately accounted for and is not fixed
by this change.
