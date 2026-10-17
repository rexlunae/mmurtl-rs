#!/bin/sh
# Make bootloader 0.11.17's stage builds compile with this repo's pinned
# nightly (rust-toolchain.toml).
#
# `bootloader`'s build script `cargo install`s its BIOS/UEFI stage crates
# from crates.io, so Cargo [patch] sections can't reach them. Two things in
# those sources no longer build on the pinned nightly:
#
#   1. bootloader-0.11.17's stage target specs say "rustc-abi": "softfloat";
#      current rustc only accepts "x86-softfloat".
#   2. x86_64-0.15.5 (pulled in by the stages) implements the unstable
#      Step::forward_overflowing/backward_overflowing, which this nightly's
#      Step trait doesn't have. Both are optional (defaulted) methods, so
#      deleting the impls is behavior-preserving.
#
# This patches the extracted sources in the local Cargo registry. It is
# idempotent, and the Makefile runs it before building boot images.
set -eu

CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Make sure both crates are extracted into the registry
(cd "$ROOT/tools/image-builder" && cargo fetch --quiet)
if ! ls -d "$CARGO_HOME"/registry/src/*/x86_64-0.15.5 >/dev/null 2>&1; then
    tmp="$(mktemp -d)"
    mkdir "$tmp/src" && : > "$tmp/src/lib.rs"
    printf '[package]\nname = "fetch-x86_64"\nversion = "0.0.0"\nedition = "2021"\n\n[dependencies]\nx86_64 = "=0.15.5"\n' > "$tmp/Cargo.toml"
    (cd "$tmp" && cargo fetch --quiet)
    rm -rf "$tmp"
fi

for dir in "$CARGO_HOME"/registry/src/*/bootloader-0.11.17; do
    [ -d "$dir" ] || continue
    sed -i 's/"rustc-abi": "softfloat"/"rustc-abi": "x86-softfloat"/' "$dir"/*.json
done

for dir in "$CARGO_HOME"/registry/src/*/x86_64-0.15.5; do
    [ -d "$dir" ] || continue
    grep -rl 'fn \(forward\|backward\)_overflowing' "$dir/src" 2>/dev/null | while read -r f; do
        # Drop each `fn {forward,backward}_overflowing(...) { ... }` item
        # together with its attribute lines (the body ends at the first
        # 4-space-indented closing brace)
        perl -0pi -e 's/(?:\n[ \t]*#\[[^\n]*\])*\n[ \t]*fn (?:forward|backward)_overflowing\b.*?\n    \}\n/\n/gs' "$f"
    done
done

# BIOS stage 4 inherits a small real-mode stack. With ASLR enabled, HC128
# and placement selection overflow it before entering the kernel. Switch to
# a dedicated 128-KiB stack before the first Rust instruction. Stage 4 and
# its BSS lie below the loaded kernel; the legacy allocator starts after it.
# Fetch/extract the stage crate before patching on a clean machine.
if ! ls -d "$CARGO_HOME"/registry/src/*/bootloader-x86_64-bios-stage-4-0.11.17 >/dev/null 2>&1; then
    cargo info bootloader-x86_64-bios-stage-4@0.11.17 >/dev/null
fi
if ! ls -d "$CARGO_HOME"/registry/src/*/bootloader-x86_64-common-0.11.17 >/dev/null 2>&1; then
    cargo info bootloader-x86_64-common@0.11.17 >/dev/null
fi
stage_changed="$(python3 - "$CARGO_HOME" <<'PY'
from pathlib import Path
import sys
# Upstream 0.11.17 inverted its TSC capability gate. Correct it so the
# fallback samples available TSCs and never executes RDTSC when unsupported.
for path in Path(sys.argv[1]).glob('registry/src/*/bootloader-x86_64-common-0.11.17/src/entropy.rs'):
    source = path.read_text()
    if '&& !feature_info.has_tsc()' in source:
        path.write_text(source.replace('&& !feature_info.has_tsc()', '&& feature_info.has_tsc()'))
        print("changed")
# An exactly full PML4 span has zero offset slack. Upstream instead adds
# up to another 512 GiB, overflowing its reserved slots into other mappings.
for path in Path(sys.argv[1]).glob('registry/src/*/bootloader-x86_64-common-0.11.17/src/level_4_entries.rs'):
    source = path.read_text()
    old = "let max_offset = LEVEL_4_SIZE - (size % LEVEL_4_SIZE);\n            let uniform_range = Uniform::from(0..max_offset / alignment);"
    new = "let max_offset = level_4_entries * LEVEL_4_SIZE - size;\n            let uniform_range = Uniform::from(0..=max_offset / alignment);"
    if old in source:
        path.write_text(source.replace(old, new))
        print("changed")
    elif new not in source:
        raise SystemExit(f'unexpected ASLR placement code: {path}')
for path in Path(sys.argv[1]).glob('registry/src/*/bootloader-x86_64-bios-stage-4-0.11.17/src/main.rs'):
    source = path.read_text()
    old = '#[unsafe(no_mangle)]\n#[unsafe(link_section = ".start")]\npub extern "C" fn _start(info: &mut BiosInfo) -> ! {'
    new = '''#[repr(C, align(16))]
struct Stage4Stack([u8; 128 * 1024]);
static mut MMURTL_STAGE4_STACK: Stage4Stack = Stage4Stack([0; 128 * 1024]);
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".start")]
pub extern "C" fn _start(_info: &mut BiosInfo) -> ! {
    core::arch::naked_asm!(
        "lea rsp, [rip + {stack} + 131072]",
        "call {entry}",
        "ud2",
        stack = sym MMURTL_STAGE4_STACK,
        entry = sym mmurtl_stage4_main,
    );
}
extern "C" fn mmurtl_stage4_main(info: &mut BiosInfo) -> ! {'''
    if 'MMURTL_STAGE4_STACK' in source and '\nfn mmurtl_stage4_main(' in source:
        path.write_text(source.replace('\nfn mmurtl_stage4_main(', '\nextern "C" fn mmurtl_stage4_main('))
        print("changed")
    if 'MMURTL_STAGE4_STACK' not in source:
        if old not in source: raise SystemExit(f'unexpected stage-4 entry: {path}')
        path.write_text(source.replace(old, new, 1))
        print("changed")
PY
)"
if [ -n "$stage_changed" ]; then
    (cd "$ROOT/tools/image-builder" && cargo clean -p bootloader --release --target x86_64-unknown-linux-gnu)
fi
