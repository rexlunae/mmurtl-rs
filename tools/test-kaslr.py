#!/usr/bin/env python3
"""Repeat full boot checks; verify raw layout diversity or default pointer hygiene.

Build with `make bios arm64-image KERNEL_FEATURES=debug-addresses` to check
actual placement diversity. Rebuild without the feature and omit --debug to
check that routine boot logs do not expose upper-half addresses.
"""
import argparse
import re
import subprocess
import sys
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch', choices=['amd64', 'arm64'], required=True)
    parser.add_argument('--boots', type=int, default=20)
    parser.add_argument('--debug', action='store_true')
    args = parser.parse_args()
    if args.boots < 2: parser.error('at least two boots are needed to compare placements')
    layouts = []
    heap = []
    for i in range(args.boots):
        cpus = [1, 2, 4][i % 3]
        subprocess.run([sys.executable, str(ROOT / 'tools/test-security.py'), '--arch', args.arch,
                        '--cpu', 'max', '--cpus', str(cpus), '--require', '[KASLR]'], check=True)
        path = ROOT / f'target/security-{args.arch}-{cpus}-boot.log'
        text = path.read_text()
        (ROOT / f'target/kaslr-{args.arch}-{i}.log').write_text(text)
        if args.debug:
            match = re.search(r'\[KASLR\] image=(0x[0-9a-f]+) physmap=(0x[0-9a-f]+)', text)
            assert match, 'debug-addresses build required for raw placement checks'
            image, linear = map(lambda s: int(s, 16), match.groups())
            assert image >= 0xffff000000000000 and linear >= 0xffff000000000000
            layouts.append((image, linear))
            match = re.search(r'\[HEAP\] Bump allocator at (0x[0-9a-f]+)', text)
            assert match, 'missing raw heap address'
            heap.append(int(match[1], 16))
        else:
            assert not re.search(r'0xffff[0-9a-f]{12}\b', text), 'raw upper-half address in default boot log'
            assert 'image=ptr:' in text, 'seeded pointer identifiers missing'
        print(f'KASLR {args.arch}: boot {i + 1}/{args.boots} ({cpus} CPUs) passed', flush=True)
    if args.debug:
        assert len({x[0] for x in layouts}) > 1, 'kernel image never moved'
        assert len({x[1] for x in layouts}) > 1, 'physical map never moved'
        assert len(set(heap)) > 1, 'heap never moved'
        print(f'KASLR {args.arch}: image, physical map and heap vary across boots')

if __name__ == '__main__': main()
