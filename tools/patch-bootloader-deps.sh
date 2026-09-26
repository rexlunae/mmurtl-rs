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
