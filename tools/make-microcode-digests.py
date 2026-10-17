#!/usr/bin/env python3
"""Regenerate the AMD patch allowlist from reviewed vendor container files.

Usage: make-microcode-digests.py <linux-firmware-commit> <vendor-files...>
Review the source commit and vendor signatures before updating the trust root.
This never downloads or approves new microcode automatically.
"""
import hashlib
from pathlib import Path
import struct
import sys

def main():
    if len(sys.argv) < 3: raise SystemExit(__doc__)
    commit = sys.argv[1]
    if len(commit) != 40 or any(c not in '0123456789abcdef' for c in commit): raise SystemExit('expected full source commit')
    patches = set()
    for name in sys.argv[2:]:
        data = Path(name).read_bytes()
        offset = 0
        while offset < len(data):
            magic, kind, table_len = struct.unpack_from('<III', data, offset)
            assert magic == 0x414d44 and kind == 0 and table_len % 16 == 0
            offset += 12 + table_len
            assert offset <= len(data)
            while offset < len(data) and struct.unpack_from('<I', data, offset)[0] != 0x414d44:
                kind, size = struct.unpack_from('<II', data, offset)
                assert kind == 1 and size >= 64 and offset + 8 + size <= len(data)
                patch = data[offset + 8:offset + 8 + size]
                revision = struct.unpack_from('<I', patch, 4)[0]
                patches.add((revision, size, hashlib.sha256(patch).digest()))
                offset += 8 + size
    lines = [f'// Generated from linux-firmware {commit}.', '// SHA-256 data only; no firmware payload is embedded.', 'pub const AMD_DIGESTS: &[(u32, usize, [u8; 32])] = &[']
    for rev, size, digest in sorted(patches):
        values = ', '.join(f'0x{b:02x}' for b in digest)
        lines.append(f'    (0x{rev:08x}, {size}, [{values}]),')
    lines.append('];')
    (Path(__file__).resolve().parent.parent / 'src/microcode/amd_digests.rs').write_text('\n'.join(lines) + '\n')
    print(f'Generated {len(patches)} reviewed-patch fingerprints')

if __name__ == '__main__': main()
