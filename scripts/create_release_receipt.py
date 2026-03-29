#!/usr/bin/env python3
import hashlib
import json
import sys
import time
from pathlib import Path


def sha256_hex(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def main() -> int:
    if len(sys.argv) not in (2, 3):
        print(
            "usage: create_release_receipt.py <release_dir> [release_id]",
            file=sys.stderr,
        )
        return 1

    release_dir = Path(sys.argv[1]).resolve()
    if not release_dir.is_dir():
        print(f"release dir does not exist: {release_dir}", file=sys.stderr)
        return 1

    release_id = sys.argv[2] if len(sys.argv) == 3 else release_dir.name
    templates = []
    manifest_hashes = {}
    for manifest_path in sorted(release_dir.glob("templates/*/template.manifest.json")):
        rel_manifest = manifest_path.relative_to(release_dir).as_posix()
        workdir = manifest_path.parent.relative_to(release_dir).as_posix()
        language = manifest_path.parent.name
        templates.append(
            {
                "language": language,
                "workdir": workdir,
                "manifest_path": rel_manifest,
            }
        )
        manifest_hashes[rel_manifest] = sha256_hex(manifest_path)

    if not templates:
        print("release dir does not contain any template manifests", file=sys.stderr)
        return 1

    receipt = {
        "release_id": release_id,
        "created_at_unix_ms": int(time.time() * 1000),
        "templates": templates,
        "manifest_hashes": manifest_hashes,
    }
    output_path = release_dir / "release-receipt.json"
    output_path.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(output_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
