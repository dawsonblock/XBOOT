import hashlib
import json
import subprocess
import tempfile
from pathlib import Path
import unittest

REPO = Path(__file__).resolve().parents[1]
SCRIPT = REPO / "scripts" / "validate_template_manifest.py"


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class TemplateManifestValidatorTests(unittest.TestCase):
    def make_workdir(self):
        td = tempfile.TemporaryDirectory()
        workdir = Path(td.name)
        snap = workdir / "snapshot"
        snap.mkdir()
        state = snap / "vmstate"
        mem = snap / "mem"
        kernel = workdir / "vmlinux-fc"
        rootfs = workdir / "rootfs-python.ext4"
        state_bytes = b"abc"
        mem_bytes = b"defgh"
        kernel_bytes = b"kernel"
        rootfs_bytes = b"rootfs"
        state.write_bytes(state_bytes)
        mem.write_bytes(mem_bytes)
        kernel.write_bytes(kernel_bytes)
        rootfs.write_bytes(rootfs_bytes)
        manifest = {
            "language": "python",
            "kernel_path": str(kernel),
            "kernel_sha256": sha256_hex(kernel_bytes),
            "rootfs_path": str(rootfs),
            "rootfs_sha256": sha256_hex(rootfs_bytes),
            "init_path": "/init",
            "mem_size_mib": 512,
            "snapshot_state_path": str(state),
            "snapshot_mem_path": str(mem),
            "snapshot_state_bytes": len(state_bytes),
            "snapshot_mem_bytes": len(mem_bytes),
            "snapshot_state_sha256": sha256_hex(state_bytes),
            "snapshot_mem_sha256": sha256_hex(mem_bytes),
            "protocol_version": "ZB1",
            "firecracker_version": "firecracker v1.8.0",
        }
        return td, workdir, manifest

    def test_accepts_matching_sizes_and_hashes(self):
        td, workdir, manifest = self.make_workdir()
        with td:
            (workdir / "template.manifest.json").write_text(json.dumps(manifest))
            proc = subprocess.run(["python3", str(SCRIPT), str(workdir)], capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_rejects_size_mismatch(self):
        td, workdir, manifest = self.make_workdir()
        with td:
            manifest["snapshot_state_bytes"] = 2
            (workdir / "template.manifest.json").write_text(json.dumps(manifest))
            proc = subprocess.run(["python3", str(SCRIPT), str(workdir)], capture_output=True, text=True)
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("size mismatch", proc.stderr)

    def test_rejects_hash_mismatch(self):
        td, workdir, manifest = self.make_workdir()
        with td:
            manifest["snapshot_mem_sha256"] = "0" * 64
            (workdir / "template.manifest.json").write_text(json.dumps(manifest))
            proc = subprocess.run(["python3", str(SCRIPT), str(workdir)], capture_output=True, text=True)
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("sha256 mismatch", proc.stderr)

    def test_rejects_missing_language(self):
        td, workdir, manifest = self.make_workdir()
        with td:
            manifest.pop("language")
            (workdir / "template.manifest.json").write_text(json.dumps(manifest))
            proc = subprocess.run(["python3", str(SCRIPT), str(workdir)], capture_output=True, text=True)
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("manifest missing field: language", proc.stderr)


if __name__ == "__main__":
    unittest.main()
