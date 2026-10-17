#!/bin/sh
# Build and boot the host's architecture, or an explicitly selected port.
set -eu

usage() {
    cat <<'EOF'
Usage: tools/run-qemu.sh [--arch ARCH | --amd64 | --arm64]
                         [--disk IMAGE | --no-disk] [-- QEMU_ARGS...]

Build and launch MMURTL/RS in QEMU. Defaults to the host architecture.

  -a, --arch ARCH  Select amd64 (x86_64) or arm64 (aarch64)
      --amd64      Select amd64 BIOS boot
      --arm64      Select arm64 QEMU virt boot
      --disk IMAGE Attach an existing FAT/exFAT disk instead of the default
      --no-disk    Boot without the default data disk
  -h, --help       Show this help
      --          Pass remaining arguments directly to QEMU

Requires make, the project's Rust toolchain, and qemu-system-x86_64 or
qemu-system-aarch64. ARM64_SMP and ARM64_GIC default to 4 and 3.
By default disk-ARCH.img in the repository is created with /BIN/HELLO.ELF
and /BIN/SIEVE.ELF if missing, then reused. Creating it requires Python 3
and exfatprogs (mkfs.exfat, fsck.exfat); existing disks are never replaced.
EOF
}

fail() {
    echo "run-qemu: $*" >&2
    exit 1
}

arch=
disk=
disk_mode=default
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
        --disk)
            [ "$#" -ge 2 ] && [ -n "$2" ] || fail "--disk requires an image path"
            disk=$2; disk_mode=custom; shift 2
            ;;
        --disk=*)
            disk=${1#*=}; [ -n "$disk" ] || fail "--disk requires an image path"
            disk_mode=custom; shift
            ;;
        --no-disk) disk_mode=none; shift ;;
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

case "$disk_mode" in
    default)
        disk="$ROOT/disk-$arch.img"
        if [ ! -e "$disk" ]; then
            for dependency in python3 mkfs.exfat fsck.exfat; do
                command -v "$dependency" >/dev/null 2>&1 || fail "$dependency is required to create the default disk"
            done
            echo "Creating data disk for $arch..."
            case "$arch" in
                amd64) make -C "$ROOT" disk ;;
                arm64) make -C "$ROOT" disk-arm64 ;;
            esac
        fi
        ;;
    custom) [ -f "$disk" ] || fail "disk image not found: $disk" ;;
esac

# Prepend the data device while preserving the caller's extra arguments
# and working directory (including relative paths passed via --disk).
if [ "$disk_mode" != none ]; then
    [ -f "$disk" ] || fail "disk image is not a regular file: $disk"
    case "$arch" in
        amd64) set -- -drive "format=raw,file=$disk,if=virtio" "$@" ;;
        arm64) set -- -drive "if=none,format=raw,file=$disk,id=mmurtl_data" \
            -device virtio-blk-device,drive=mmurtl_data "$@" ;;
    esac
fi

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
