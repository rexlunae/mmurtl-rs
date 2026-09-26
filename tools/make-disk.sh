#!/bin/sh
# Create a 16 MiB exFAT disk image holding the user programs in /BIN, for
# the kernel to load at boot.
#
#   tools/make-disk.sh <image> <rust-target>
#   e.g. tools/make-disk.sh disk-amd64.img x86_64-unknown-none
#
# Needs exfatprogs (mkfs.exfat) and exfat-fuse, and root for the loop
# device (run via sudo if not root). Build the programs first (make user).
set -eu

IMG="$1"
TARGET="$2"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/user/target/$TARGET/release"
SUDO=""
[ "$(id -u)" = 0 ] || SUDO=sudo

truncate -s 16M "$IMG"
mkfs.exfat -L MMURTL "$IMG" >/dev/null
MNT="$(mktemp -d)"
LOOP="$($SUDO losetup --show -f "$IMG")"
trap '$SUDO umount "$MNT" 2>/dev/null || true; $SUDO losetup -d "$LOOP"; rmdir "$MNT"' EXIT
$SUDO mount.exfat-fuse "$LOOP" "$MNT"
$SUDO mkdir -p "$MNT/BIN"
for prog in hello sieve; do
    NAME="$(echo "$prog" | tr '[:lower:]' '[:upper:]').ELF"
    $SUDO cp "$BIN/$prog" "$MNT/BIN/$NAME"
    echo "  /BIN/$NAME"
done
echo "Created $IMG"
