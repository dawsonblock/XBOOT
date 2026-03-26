import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PLACEHOLDER_ALLOWLIST = {
    'kernel.manifest': {'firecracker_version', 'kernel_version', 'kernel_sha256'},
    'python-guest.manifest': {'base_rootfs', 'base_rootfs_sha256', 'runtime_version'},
    'node-guest.manifest': {'base_rootfs', 'base_rootfs_sha256', 'runtime_version'},
}
LOCK_FILES = [
    'manifests/python-build.lock.json',
    'manifests/node-build.lock.json',
    'manifests/firecracker.lock.json',
]


class RepoSanityTests(unittest.TestCase):
    def test_manifests_do_not_use_unknown_required_placeholders(self):
        manifest_dir = ROOT / 'manifests'
        for path in manifest_dir.glob('*.manifest'):
            allowed = MANIFEST_PLACEHOLDER_ALLOWLIST.get(path.name, set())
            for raw_line in path.read_text().splitlines():
                line = raw_line.strip()
                if not line or line.startswith('#') or '=REQUIRED' not in line:
                    continue
                key, value = line.split('=', 1)
                self.assertEqual(value, 'REQUIRED', f"{path.name}: unresolved placeholder must be exactly REQUIRED")
                self.assertIn(key, allowed, f"{path.name}: unexpected REQUIRED placeholder for {key}")

    def test_repo_contains_no_pin_me_tokens(self):
        sentinel = 'PIN_' + 'ME'
        for path in ROOT.rglob('*'):
            if path.is_file() and '.git' not in path.parts and '__pycache__' not in path.parts and path.suffix != '.pyc' and path.name != 'test_repo_sanity.py':
                self.assertNotIn(sentinel, path.read_text(errors='ignore'), str(path.relative_to(ROOT)))

    def test_template_manifest_validator_exists(self):
        self.assertTrue((ROOT / 'scripts' / 'validate_template_manifest.py').exists())

    def test_build_lock_files_are_valid_json(self):
        for rel in LOCK_FILES:
            path = ROOT / rel
            self.assertTrue(path.exists(), rel)
            data = json.loads(path.read_text())
            self.assertIsInstance(data, dict)
            self.assertTrue(data, rel)


if __name__ == '__main__':
    unittest.main()
