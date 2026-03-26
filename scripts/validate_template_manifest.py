#!/usr/bin/env python3
import hashlib
import json
import sys
from pathlib import Path

REQUIRED_FIELDS = [
    "language",
    "kernel_path",
    "rootfs_path",
    "init_path",
    "mem_size_mib",
    "snapshot_state_path",
    "snapshot_mem_path",
    "snapshot_state_bytes",
    "snapshot_mem_bytes",
    "protocol_version",
]

HASH_FIELDS = {
    "kernel_sha256": "kernel_path",
    "rootfs_sha256": "rootfs_path",
    "snapshot_state_sha256": "snapshot_state_path",
    "snapshot_mem_sha256": "snapshot_mem_path",
}


def fail(msg: str) -> int:
    print(msg, file=sys.stderr)
    return 1


def sha256_hex(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def resolve(workdir: Path, value: str) -> Path:
    path = Path(value)
    return path if path.is_absolute() else workdir / path


def main() -> int:
    if len(sys.argv) != 2:
        return fail("usage: validate_template_manifest.py <workdir>")
    workdir = Path(sys.argv[1])
    manifest_path = workdir / "template.manifest.json"
    if not manifest_path.exists():
        return fail(f"missing template manifest: {manifest_path}")

    data = json.loads(manifest_path.read_text())
    for key in REQUIRED_FIELDS:
        if key not in data:
            return fail(f"manifest missing field: {key}")

    state = resolve(workdir, data["snapshot_state_path"])
    mem = resolve(workdir, data["snapshot_mem_path"])
    if not state.exists():
        return fail(f"missing snapshot state file: {state}")
    if not mem.exists():
        return fail(f"missing snapshot memory file: {mem}")

    if state.stat().st_size != int(data["snapshot_state_bytes"]):
        return fail("snapshot state size mismatch")
    if mem.stat().st_size != int(data["snapshot_mem_bytes"]):
        return fail("snapshot memory size mismatch")

    for hash_key, path_key in HASH_FIELDS.items():
        expected = data.get(hash_key)
        if not expected:
            return fail(f"manifest missing field: {hash_key}")
        target = resolve(workdir, data[path_key])
        if not target.exists():
            return fail(f"missing artifact for {hash_key}: {target}")
        actual = sha256_hex(target)
        if actual != expected.lower():
            return fail(f"sha256 mismatch for {hash_key}: expected {expected}, got {actual}")

    print(f"template manifest OK: {manifest_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
