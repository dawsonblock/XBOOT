import json
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
README = ROOT / "README.md"
API_DOC = ROOT / "docs" / "API.md"
DEPLOY_DOC = ROOT / "docs" / "DEPLOYMENT.md"
PATCHES_DOC = ROOT / "PATCHES_APPLIED.md"
KEY_SCRIPT = ROOT / "scripts" / "make_api_keys.py"


class DocsSanityTests(unittest.TestCase):
    def test_docs_use_current_release_paths(self):
        text = DEPLOY_DOC.read_text()
        self.assertIn("/var/lib/zeroboot/current/templates/python", text)
        self.assertIn("/var/lib/zeroboot/current/templates/node", text)
        self.assertNotIn("/var/lib/zeroboot/templates/python", text)

    def test_docs_describe_hashed_api_records(self):
        text = API_DOC.read_text()
        self.assertIn('"prefix"', text)
        self.assertIn('"hash"', text)
        self.assertIn("Authorization: Bearer zb_live_", text)

    def test_key_script_writes_record_schema(self):
        with tempfile.TemporaryDirectory() as td:
            output = Path(td) / "api_keys.json"
            proc = subprocess.run(
                [
                    "python3",
                    str(KEY_SCRIPT),
                    "--count",
                    "1",
                    "--pepper",
                    "test-pepper",
                    "--output",
                    str(output),
                ],
                capture_output=True,
                text=True,
                check=True,
            )
            records = json.loads(output.read_text())
            self.assertEqual(len(records), 1)
            self.assertIn("prefix", records[0])
            self.assertIn("hash", records[0])
            self.assertIn("id", records[0])
            self.assertIn("bearer tokens", proc.stdout)

    def test_readme_mentions_offline_only_release_scope(self):
        text = README.read_text()
        self.assertIn("offline-only", text)
        self.assertIn("controlled internal use", text)
        self.assertIn("Ubuntu 22.04 x86_64 with KVM", text)
        self.assertIn("Firecracker 1.12.0", text)
        self.assertNotIn("Production-grade security", text)

    def test_patch_notes_do_not_claim_plain_keys_or_missing_promotion(self):
        text = PATCHES_DOC.read_text()
        self.assertNotIn("API keys are still stored as plain strings", text)
        self.assertNotIn("artifact signing and promotion workflow are still missing", text)


if __name__ == "__main__":
    unittest.main()
