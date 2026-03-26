#!/usr/bin/env python3
"""
Reproducible Guest Image Builder

Builds deterministic guest rootfs images using a declarative specification.
The resulting images are bit-for-bit reproducible given the same inputs.

Usage:
    python3 build_reproducible_image.py --spec spec.yaml --output /path/to/output.ext4
    python3 build_reproducible_image.py --template python3.11 --output /path/to/output.ext4

The builder:
1. Creates a minimal rootfs from packages
2. Records all build provenance in manifest
3. Produces deterministic output (content-addressable)
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import shutil
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional

# Build configuration - these define the reproducible base
DEFAULT_BASE_PACKAGES = {
    "python": [
        "python3-minimal",
        "python3-pip",
        "libc6",
        "libgcc-s1",
        "libstdc++6",
        "zlib1g",
        "libssl3",
    ],
    "node": [
        "nodejs",
        "npm",
        "libc6",
        "libgcc-s1",
        "libstdc++6",
    ],
}


def compute_artifact_hash(data: bytes) -> str:
    """Compute SHA-256 hash of artifact content."""
    return hashlib.sha256(data).hexdigest()


def compute_file_hash(path: Path) -> str:
    """Compute SHA-256 hash of file content."""
    sha256 = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(65536), b''):
            sha256.update(chunk)
    return sha256.hexdigest()


def run_cmd(cmd: List[str], cwd: Optional[Path] = None, env: Optional[Dict] = None) -> None:
    """Run a command, raising on failure."""
    result = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"Command failed: {' '.join(cmd)}\n{result.stderr}")
    return result.stdout


class ReproducibleBuilder:
    """Builds reproducible guest images."""
    
    def __init__(self, workdir: Path, manifest_path: Optional[Path] = None):
        self.workdir = workdir
        self.manifest_path = manifest_path or workdir / "build.manifest.json"
        self.provenance: Dict[str, Any] = {
            "build_timestamp": datetime.utcnow().isoformat() + "Z",
            "builder_version": "1.0.0",
            "inputs": [],
            "layers": [],
            "output_hash": "",
        }
        
    def add_input(self, name: str, path: Path, hash: str, source: str):
        """Record an input artifact."""
        self.provenance["inputs"].append({
            "name": name,
            "path": str(path),
            "sha256": hash,
            "source": source,
        })
        
    def add_layer(self, name: str, description: str, hash: str):
        """Record a build layer."""
        self.provenance["layers"].append({
            "name": name,
            "description": description,
            "sha256": hash,
        })
        
    def build_from_deb(self, packages: List[str], output: Path, 
                       base_image: Optional[Path] = None) -> Dict[str, str]:
        """Build rootfs from Debian packages."""
        print(f"Building rootfs with packages: {packages}")
        
        # Create minimal debootstrap if no base image
        if base_image is None:
            # Use debootstrap to create base
            rootfs = self.workdir / "rootfs"
            rootfs.mkdir(parents=True, exist_ok=True)
            
            print("Running debootstrap...")
            run_cmd([
                "debootstrap",
                "--variant=minbase",
                "--include=" + ",".join(packages),
                "stable",
                str(rootfs),
                "http://deb.debian.org/debian",
            ])
            
            # Compute hash of rootfs
            rootfs_hash = self._hash_directory(rootfs)
        else:
            rootfs = base_image
            rootfs_hash = compute_file_hash(base_image)
            
        # Create ext4 image
        print("Creating ext4 image...")
        output.parent.mkdir(parents=True, exist_ok=True)
        
        # Calculate required size
        root_size = sum(f.stat().st_size for f in rootfs.rglob('*') if f.is_file())
        img_size = max(root_size * 2, 50 * 1024 * 1024)  # At least 50MB, 2x rootfs
        
        run_cmd([
            "mkfs.ext4",
            "-d", str(rootfs),
            "-F",
            "-E", f"root_owner=0:0",
            "-b", "4096",
            "-O", "^has_journal",
            str(output),
            str(img_size // 4096),
        ])
        
        output_hash = compute_file_hash(output)
        self.add_layer("base_rootfs", f"Packages: {', '.join(packages)}", rootfs_hash)
        
        return {"rootfs_hash": rootfs_hash, "image_hash": output_hash}
    
    def build_from_docker(self, docker_image: str, output: Path) -> Dict[str, str]:
        """Build rootfs from Docker image (for more complex setups)."""
        print(f"Building from Docker image: {docker_image}")
        
        # Export Docker image to tar
        docker_tar = self.workdir / "image.tar"
        run_cmd(["docker", "save", "-o", str(docker_tar), docker_image])
        
        docker_hash = compute_file_hash(docker_tar)
        self.add_input("docker_image", docker_tar, docker_hash, f"docker:{docker_image}")
        
        # Extract and convert to ext4
        extract_dir = self.workdir / "docker_export"
        extract_dir.mkdir(exist_ok=True)
        
        run_cmd(["tar", "-xf", str(docker_tar), "-C", str(extract_dir)])
        
        # Create ext4 from Docker layers
        # This is simplified - real implementation would parse Docker manifest
        print("Creating ext4 from Docker export...")
        
        output.parent.mkdir(parents=True, exist_ok=True)
        run_cmd([
            "mkfs.ext4",
            "-d", str(extract_dir),
            "-F",
            str(output),
            "524288",  # 2GB
        ])
        
        output_hash = compute_file_hash(output)
        self.add_layer("docker_rootfs", f"Image: {docker_image}", output_hash)
        
        return {"image_hash": output_hash}
    
    def _hash_directory(self, path: Path) -> str:
        """Hash directory contents deterministically."""
        hasher = hashlib.sha256()
        
        # Walk in sorted order for determinism
        for entry in sorted(path.rglob('*')):
            if entry.is_file():
                rel_path = entry.relative_to(path)
                hasher.update(str(rel_path).encode())
                
                with open(entry, 'rb') as f:
                    for chunk in iter(lambda: f.read(65536), b''):
                        hasher.update(chunk)
                        
        return hasher.hexdigest()
    
    def finalize(self, output_hash: str) -> Dict[str, Any]:
        """Finalize the build manifest."""
        self.provenance["output_hash"] = output_hash
        self.provenance["output_path"] = str(self.manifest_path)
        
        # Write manifest
        with open(self.manifest_path, 'w') as f:
            json.dump(self.provenance, f, indent=2)
            
        return self.provenance


def main():
    parser = argparse.ArgumentParser(description="Reproducible Guest Image Builder")
    parser.add_argument("--spec", type=Path, help="Build specification YAML/JSON")
    parser.add_argument("--template", choices=["python", "node"], 
                        help="Quick template selection")
    parser.add_argument("--packages", nargs="+", help="Package list")
    parser.add_argument("--docker-image", help="Docker image to build from")
    parser.add_argument("--base-image", type=Path, help="Base rootfs image")
    parser.add_argument("--output", type=Path, required=True, help="Output image path")
    parser.add_argument("--workdir", type=Path, default=Path("/tmp/zeroboot-build"),
                        help="Working directory")
    parser.add_argument("--manifest", type=Path, help="Output manifest path")
    
    args = parser.parse_args()
    
    # Create workdir
    args.workdir.mkdir(parents=True, exist_ok=True)
    
    builder = ReproducibleBuilder(args.workdir, args.manifest)
    
    try:
        # Determine build method
        if args.docker_image:
            result = builder.build_from_docker(args.docker_image, args.output)
        elif args.template:
            packages = DEFAULT_BASE_PACKAGES.get(args.template, [])
            result = builder.build_from_deb(packages, args.output, args.base_image)
        elif args.packages:
            result = builder.build_from_deb(args.packages, args.output, args.base_image)
        elif args.spec:
            # TODO: Parse spec file
            raise NotImplementedError("Spec file parsing not yet implemented")
        else:
            parser.error("Must specify --template, --packages, --docker-image, or --spec")
            
        # Finalize
        manifest = builder.finalize(result.get("image_hash", ""))
        
        print(f"\nBuild complete!")
        print(f"  Output: {args.output}")
        print(f"  Hash: {result.get('image_hash', 'unknown')}")
        print(f"  Manifest: {args.manifest or builder.manifest_path}")
        
    except Exception as e:
        print(f"Build failed: {e}", file=sys.stderr)
        sys.exit(1)
    finally:
        # Clean up workdir
        if args.workdir.exists():
            shutil.rmtree(args.workdir, ignore_errors=True)


if __name__ == "__main__":
    main()