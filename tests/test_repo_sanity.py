import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LOCK_FILES = [
    'manifests/python-build.lock.json',
    'manifests/node-build.lock.json',
    'manifests/firecracker.lock.json',
    'manifests/runtime-artifacts.lock.json',
]


class RepoSanityTests(unittest.TestCase):
    def test_manifests_do_not_leave_required_placeholders(self):
        manifest_dir = ROOT / 'manifests'
        for path in manifest_dir.glob('*.manifest'):
            for raw_line in path.read_text().splitlines():
                line = raw_line.strip()
                if not line or line.startswith('#') or '=REQUIRED' not in line:
                    continue
                self.fail(f"{path.name}: unresolved REQUIRED placeholder remains: {line}")

    def test_lock_files_do_not_leave_required_placeholders(self):
        for rel in LOCK_FILES:
            path = ROOT / rel
            self.assertNotIn('REQUIRED', path.read_text(), rel)

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
