#!/usr/bin/env python3
"""Verify the real fatal MCA/IST logging path in a test-machine-check BIOS image.

Build: make bios KERNEL_FEATURES=test-machine-check
For the explicit raw diagnostic path, add ,debug-addresses and pass --debug.
"""
import argparse
import os
import re
import select
import subprocess
import time
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--debug', action='store_true')
    args = parser.parse_args()
    output = bytearray()
    proc = subprocess.Popen(['qemu-system-x86_64','-cpu','max','-smp','4','-m','256M',
        '-drive',f'format=raw,file={ROOT / "target/mmurtl-rs-bios.img"}', '-snapshot',
        '-serial','stdio','-display','none','-monitor','none','-no-reboot'], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and proc.poll() is None:
            if select.select([proc.stdout], [], [], .1)[0]:
                output.extend(os.read(proc.stdout.fileno(),65536))
                if b'[MCE] CPU halted' in output: break
        text = output.decode(errors='replace')
        assert '[MCE] CPU halted' in text, text[-3000:]
        fatal = text[text.index('[MCE] fatal'):]
        if args.debug:
            assert re.search(r'check at 0xffff[0-9a-f]{12}',fatal),fatal
            assert ' addr=0xffff' in fatal and ' misc=0xffff' in fatal,fatal
        else:
            assert 'check at (ptr)' in fatal and ' addr=(ptr)' in fatal and ' misc=(restricted)' in fatal,fatal
            assert not re.search(r'0xffff[0-9a-f]{12}',fatal),fatal
        print('machine-check guarded IST logging: ' + ('explicit raw diagnostics' if args.debug else 'default redaction') + ' passed')
    finally:
        proc.terminate(); proc.wait(timeout=5)
        (ROOT / 'target/machine-check-test.log').write_bytes(output)

if __name__ == '__main__': main()
