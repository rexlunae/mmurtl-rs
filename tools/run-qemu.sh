#!/bin/sh
# Build and boot the host's architecture, or an explicitly selected port.
set -eu

usage() {
    cat <<'EOF'
Usage: tools/run-qemu.sh [--arch ARCH | --amd64 | --arm64] [-- QEMU_ARGS...]

Build and launch MMURTL/RS in QEMU. Defaults to the host architecture.

  -a, --arch ARCH  Select amd64 (x86_64) or arm64 (aarch64)
      --amd64      Select amd64 BIOS boot
      --arm64      Select arm64 QEMU virt boot
  -h, --help       Show this help
      --          Pass remaining arguments directly to QEMU

Requires make, the project's Rust toolchain, and qemu-system-x86_64 or
qemu-system-aarch64. ARM64_SMP and ARM64_GIC default to 4 and 3.
EOF
}

fail() {
    echo "run-qemu: $*" >&2
    exit 1
}

arch=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -a|--arch)
            [ "$#" -ge 2 ] || fail "$1 requires an architecture"
            arch=$2
            shift 2
            ;;
        --arch=*) arch=${1#*=}; shift ;;
        --amd64) arch=amd64; shift ;;
        --arm64) arch=arm64; shift ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        *) fail "unknown option '$1' (see --help)" ;;
    esac
done

[ -n "$arch" ] || arch=$(uname -m)
case "$arch" in
    amd64|x86_64) arch=amd64; qemu=qemu-system-x86_64; target=bios ;;
    arm64|aarch64) arch=arm64; qemu=qemu-system-aarch64; target=arm64 ;;
    *) fail "unsupported architecture '$arch'; use --arch amd64 or --arch arm64" ;;
esac

command -v "$qemu" >/dev/null 2>&1 || fail "$qemu is required; install QEMU for $arch"
command -v make >/dev/null 2>&1 || fail "make is required"
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

echo "Building MMURTL/RS for $arch..."
make -C "$ROOT" "$target"
echo "Launching MMURTL/RS with $qemu..."
case "$arch" in
    amd64)
        exec "$qemu" -drive "format=raw,file=$ROOT/target/mmurtl-rs-bios.img" \
            -serial stdio -m 256M "$@"
        ;;
    arm64)
        exec "$qemu" -machine "virt,gic-version=${ARM64_GIC:-3}" \
            -cpu cortex-a72 -smp "${ARM64_SMP:-4}" -m 256M -nographic \
            -kernel "$ROOT/target/aarch64-unknown-none-softfloat/release/mmurtl-rs" "$@"
        ;;
esac
