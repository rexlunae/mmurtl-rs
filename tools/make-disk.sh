#!/bin/sh
# Create a new 16 MiB exFAT /BIN image; existing images are never replaced.
# Requires Python 3 and exfatprogs. No mounts, loop devices, or sudo.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec python3 "$ROOT/tools/make_disk.py" "$@"
