#!/usr/bin/env python3
"""Check launcher disk selection/preservation and the unprivileged image builder."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from make_disk import create_image

ROOT = Path(__file__).resolve().parent.parent


class LauncherTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='mmurtl-launcher-')
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.repo = self.work / 'repo'
        self.caller = self.work / 'caller'
        self.bin = self.work / 'bin'
        (self.repo / 'tools').mkdir(parents=True)
        self.caller.mkdir()
        self.bin.mkdir()
        self.script = self.repo / 'tools/run-qemu.sh'
        shutil.copy(ROOT / 'tools/run-qemu.sh', self.script)
        self.calls = self.work / 'calls'
        self.env = {**os.environ, 'PATH': str(self.bin) + ':' + os.environ['PATH'],
                    'TEST_CALLS': str(self.calls), 'TEST_ARCH': 'x86_64'}
        self.tool('uname', 'import os; print(os.environ["TEST_ARCH"])')
        self.tool('make', '''import os, pathlib, sys
root = pathlib.Path(sys.argv[2])
assert sys.argv[1] == '-C'
target = sys.argv[-1]
with open(os.environ['TEST_CALLS'], 'a') as log: log.write(target + '\\n')
if target == os.environ.get('TEST_FAIL_TARGET'): sys.exit(7)
if target in ('disk', 'disk-arm64'):
    (root / ('disk-amd64.img' if target == 'disk' else 'disk-arm64.img')).write_bytes(b'new image')
''')
        qemu = 'import json, os, sys; print(json.dumps({"args": sys.argv[1:], "cwd": os.getcwd()}))'
        self.tool('qemu-system-x86_64', qemu)
        self.tool('qemu-system-aarch64', qemu)

    def tool(self, name, body):
        path = self.bin / name
        path.write_text(f'#!{sys.executable}\n{body}\n')
        path.chmod(0o755)

    def run_launcher(self, *args, success=True):
        result = subprocess.run([str(self.script), *args], cwd=self.caller, env=self.env,
                                capture_output=True, text=True)
        if not success:
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn('"args":', result.stdout)
            return result
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout.splitlines()[-1])

    def test_default_disk_created_once_and_preserved(self):
        result = self.run_launcher()
        self.assertEqual(result['cwd'], str(self.caller))
        self.assertIn(f'format=raw,file={self.repo}/disk-amd64.img,if=virtio', result['args'])
        self.assertEqual(self.calls.read_text().splitlines(), ['disk', 'bios'])
        disk = self.repo / 'disk-amd64.img'
        disk.write_bytes(b'user data must survive')
        self.run_launcher()
        self.assertEqual(disk.read_bytes(), b'user data must survive')
        self.assertEqual(self.calls.read_text().splitlines(), ['disk', 'bios', 'bios'])

    def test_arm64_device_and_build(self):
        result = self.run_launcher('--arch', 'aarch64')
        self.assertIn(f'if=none,format=raw,file={self.repo}/disk-arm64.img,id=mmurtl_data', result['args'])
        self.assertIn('virtio-blk-device,drive=mmurtl_data', result['args'])
        self.assertEqual(self.calls.read_text().splitlines(), ['disk-arm64', 'arm64'])

    def test_custom_disk_preserves_caller_relative_paths(self):
        disk = self.caller / 'custom disk.img'
        disk.write_bytes(b'keep')
        result = self.run_launcher('--disk', disk.name, '--', '-name', 'VM with spaces')
        self.assertEqual(result['cwd'], str(self.caller))
        self.assertIn('format=raw,file=custom disk.img,if=virtio', result['args'])
        self.assertIn('VM with spaces', result['args'])
        self.assertEqual(self.calls.read_text().splitlines(), ['bios'])
        self.assertEqual(disk.read_bytes(), b'keep')

    def test_no_disk_and_forwarded_arguments(self):
        result = self.run_launcher('--no-disk', '--', '-drive', 'file=manual.img,if=virtio')
        self.assertIn('file=manual.img,if=virtio', result['args'])
        self.assertFalse((self.repo / 'disk-amd64.img').exists())
        self.assertEqual(self.calls.read_text().splitlines(), ['bios'])

    def test_invalid_input_and_build_failure(self):
        for args in [('--disk',), ('--disk=',), ('--disk', 'missing.img'), ('--arch', 'bad')]:
            self.run_launcher(*args, success=False)
        self.assertFalse(self.calls.exists())
        self.env['TEST_FAIL_TARGET'] = 'disk'
        self.assertEqual(self.run_launcher(success=False).returncode, 7)
        self.env['TEST_FAIL_TARGET'] = 'bios'
        self.assertEqual(self.run_launcher('--no-disk', success=False).returncode, 7)

    def test_image_builder_refuses_overwrite_and_cleans_failed_creation(self):
        disk = self.work / 'actual.img'
        create_image(disk, {'HELLO.ELF': b'hello', 'SIEVE.ELF': b'sieve'})
        original = disk.read_bytes()
        with self.assertRaises(FileExistsError):
            create_image(disk, {'HELLO.ELF': b'replacement'})
        self.assertEqual(disk.read_bytes(), original)
        failed = self.work / 'failed.img'
        with self.assertRaises(ValueError):
            create_image(failed, {'invalid/name': b'bad'})
        self.assertFalse(failed.exists())
        self.assertFalse(list(self.work.glob('.mmurtl-disk-*')))


if __name__ == '__main__':
    unittest.main()
