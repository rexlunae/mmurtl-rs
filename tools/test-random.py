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
        # The ARM Linux Image header includes data words before executable
        # code. LLVM prints their raw bytes even with --no-show-raw-insn;
        # a size byte such as d0 is not a floating-point register operand.
        kernel_start = None
        if target.startswith('aarch64'):
            symbols = subprocess.check_output([str(objdump.parent/'llvm-nm'),'-n',str(ROOT/'target'/target/'release/mmurtl-rs')],text=True)
            kernel_start = int(next(row.split()[0] for row in symbols.splitlines() if row.endswith(' __kernel_start')),16)
        def header_data(line):
            address = re.match(r'\s*([0-9a-f]+):',line)
            return kernel_start is not None and address and kernel_start <= int(address[1],16) < kernel_start+64 and '.word' in line
        violations = [line for line in disassembly.splitlines() if not header_data(line) and re.search(pattern, line)]
        assert not violations, f'{target}: SIMD appeared in kernel text: {violations[:5]}'
        print(f'{target}: scalar kernel text verified')

if __name__ == '__main__':
    main()
