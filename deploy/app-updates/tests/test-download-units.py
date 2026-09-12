"""Validate units offline against a temporary filesystem; never start a unit."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]


class DownloadUnitTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which('systemd-analyze'), 'systemd-analyze unavailable')
    def test_offline_unit_validation(self):
        with tempfile.TemporaryDirectory(prefix='jarvis-unit-fixture-') as temporary:
            root = Path(temporary)
            units = root / 'etc/systemd/system'
            units.mkdir(parents=True)
            binary = root / 'usr/local/libexec/jarvis-app-downloads'
            binary.parent.mkdir(parents=True)
            binary.write_text('#!/bin/sh\nexit 0\n')
            binary.chmod(0o755)
            for name in ('sysinit.target', 'basic.target', 'shutdown.target', 'timers.target', 'network-online.target'):
                (units / name).write_text('[Unit]\nDescription=Fixture only\nDefaultDependencies=no\n')
            names = ['jarvis-app-downloads.service', 'jarvis-app-downloads.timer',
                     'jarvis-app-release-sync.service', 'jarvis-app-release-sync.timer']
            for name in names:
                shutil.copyfile(ROOT / 'deploy/app-updates' / name, units / name)
            result = subprocess.run(['systemd-analyze', '--root', str(root), 'verify', '--man=no', *names],
                                    capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_service_cannot_write_config_or_core_state(self):
        service = (ROOT / 'deploy/app-updates/jarvis-app-downloads.service').read_text()
        self.assertIn('ReadWritePaths=/var/lib/jarvis-public-downloads\n', service)
        self.assertIn('ReadOnlyPaths=/etc/jarvis/app-downloads\n', service)
        self.assertIn('ProtectSystem=strict\n', service)
        self.assertIn('CapabilityBoundingSet=\n', service)
        self.assertNotIn('docker.sock', service)

    def test_complete_release_service_writes_only_two_mirrors(self):
        service = (ROOT / 'deploy/app-updates/jarvis-app-release-sync.service').read_text()
        self.assertIn('ReadWritePaths=/var/lib/jarvis-public-downloads /var/lib/jarvis-app-updates\n', service)
        self.assertIn('ExecStart=/usr/local/libexec/jarvis-app-downloads sync-release\n', service)
        self.assertIn('CapabilityBoundingSet=\n', service)
        self.assertIn('ProtectSystem=strict\n', service)


if __name__ == '__main__':
    unittest.main()
