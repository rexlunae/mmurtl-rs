#!/usr/bin/env python3
"""Run CSPRNG vectors/policy tests and reject vector-register kernel code.

Build both kernels first (make build arm64). The disassembly check guards the
scalar cryptography requirement while kernel FP/SIMD context is disabled.
"""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parent.parent

def main():
    subprocess.run(['cargo', 'test', '--locked', '--manifest-path', str(ROOT / 'tools/random-tests/Cargo.toml')], check=True)
    sysroot = Path(subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip())
    host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host:'))
    objdump = sysroot / 'lib/rustlib' / host / 'bin/llvm-objdump'
    for target in ['x86_64-unknown-none', 'aarch64-unknown-none-softfloat']:
        disassembly = subprocess.check_output([str(objdump), '-d', '--no-show-raw-insn', str(ROOT / 'target' / target / 'release/mmurtl-rs')], text=True)
        pattern = r'\b(?:xmm|ymm|zmm)\d+\b|\bvzero(?:upper|all)\b' if target.startswith('x86') else r'(?:\s|,)(?:v\d+\.\d*[bhsd]|[qdsz]\d+)\b'
        violations = [line for line in disassembly.splitlines() if re.search(pattern, line)]
        assert not violations, f'{target}: SIMD appeared in kernel text: {violations[:5]}'
        print(f'{target}: scalar kernel text verified')

if __name__ == '__main__':
    main()
