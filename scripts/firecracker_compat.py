#!/usr/bin/env python3
"""
Firecracker Compatibility Matrix

This script documents Firecracker version compatibility and validates
version requirements.

Usage:
    python3 firecracker_compat.py --check-version 1.10.0
    python3 firecracker_compat.py --list-versions
    python3 firecracker_compat.py --validate-template manifest.json
"""

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Optional, Tuple


@dataclass
class FirecrackerVersion:
    """A Firecracker release version."""
    major: int
    minor: int
    patch: int
    prerelease: str = ""
    
    def __str__(self) -> str:
        v = f"{self.major}.{self.minor}.{self.patch}"
        if self.prerelease:
            v += f"-{self.prerelease}"
        return v
    
    def __lt__(self, other: "FirecrackerVersion") -> bool:
        if self.major != other.major:
            return self.major < other.major
        if self.minor != other.minor:
            return self.minor < other.minor
        if self.patch != other.patch:
            return self.patch < other.patch
        # Pre-release versions are considered older
        if self.prerelease and not other.prerelease:
            return True
        if not self.prerelease and other.prerelease:
            return False
        return self.prerelease < other.prerelease
    
    def __eq__(self, other: "FirecrackerVersion") -> bool:
        return (self.major == other.major and 
                self.minor == other.minor and 
                self.patch == other.patch and
                self.prerelease == other.prerelease)


# Compatibility matrix - maps Firecracker versions to supported features
COMPATIBILITY_MATRIX = {
    "1.12.0": {
        "status": "supported",
        "kvm_features": ["snapshot-restore", "vmx", "sgx"],
        "snapshot_format": "v1",
        "vmstate_layout": "stable",
        "api_version": "2023-12-01",
        "max_vcpus": 32,
        "max_memory_mb": 4096,
        "notes": "Current stable, recommended for production",
    },
    "1.11.0": {
        "status": "supported",
        "kvm_features": ["snapshot-restore", "vmx"],
        "snapshot_format": "v1",
        "vmstate_layout": "stable",
        "api_version": "2023-08-01",
        "max_vcpus": 32,
        "max_memory_mb": 4096,
        "notes": "Previous stable, also recommended",
    },
    "1.10.0": {
        "status": "supported",
        "kvm_features": ["snapshot-restore", "vmx"],
        "snapshot_format": "v1",
        "vmstate_layout": "stable",
        "api_version": "2023-04-01",
        "max_vcpus": 16,
        "max_memory_mb": 4096,
        "notes": "Legacy but still supported",
    },
    "1.9.0": {
        "status": "legacy",
        "kvm_features": ["vmx"],
        "snapshot_format": "v0",
        "vmstate_layout": "legacy",
        "api_version": "2023-01-01",
        "max_vcpus": 8,
        "max_memory_mb": 2048,
        "notes": "Deprecated, snapshot format changed in 1.10",
    },
    "1.8.0": {
        "status": "legacy",
        "kvm_features": ["vmx"],
        "snapshot_format": "v0",
        "vmstate_layout": "legacy",
        "api_version": "2022-10-01",
        "max_vcpus": 8,
        "max_memory_mb": 2048,
        "notes": "Very old, not recommended",
    },
}


def parse_version(version_str: str) -> FirecrackerVersion:
    """Parse a Firecracker version string."""
    # Handle prerelease versions like "1.12.0-alpha.1"
    match = re.match(r'^(\d+)\.(\d+)\.(\d+)(?:-([a-zA-Z0-9.]+))?$', version_str)
    if not match:
        raise ValueError(f"Invalid version format: {version_str}")
    
    return FirecrackerVersion(
        major=int(match.group(1)),
        minor=int(match.group(2)),
        patch=int(match.group(3)),
        prerelease=match.group(4) or "",
    )


def check_version_compatibility(version_str: str, 
                                required_features: Optional[List[str]] = None) -> Tuple[bool, str]:
    """Check if a Firecracker version is compatible."""
    version = parse_version(version_str)
    
    # Find the closest matching version in matrix
    matrix_version = None
    for compat_version in COMPATIBILITY_MATRIX.keys():
        compat = parse_version(compat_version)
        if compat <= version:
            if matrix_version is None or compat > parse_version(matrix_version):
                matrix_version = compat_version
    
    if matrix_version is None:
        return False, f"No compatible version found for {version_str}"
    
    info = COMPATIBILITY_MATRIX[matrix_version]
    
    # Check status
    if info["status"] == "legacy":
        return False, f"{version_str} is deprecated: {info['notes']}"
    
    # Check features
    if required_features:
        available = info.get("kvm_features", [])
        for feature in required_features:
            if feature not in available:
                return False, f"{version_str} doesn't support feature: {feature}"
    
    return True, f"{version_str} is compatible. {info.get('notes', '')}"


def validate_template_manifest(manifest_path: Path) -> List[str]:
    """Validate that template manifest's Firecracker version is compatible."""
    errors = []
    
    with open(manifest_path, 'r') as f:
        manifest = json.load(f)
    
    # Check for firecracker version
    fc_version = manifest.get("firecracker_binary_sha256", "") or manifest.get("firecracker_version", "")
    if not fc_version:
        errors.append("No firecracker version specified in manifest")
        return errors
    
    # Parse version from filename or explicit field
    # The hash might contain version info in the filename
    if not re.match(r'^\d+\.\d+\.\d+', str(fc_version)):
        errors.append(f"Cannot parse Firecracker version from: {fc_version}")
        return errors
    
    compatible, msg = check_version_compatibility(fc_version)
    if not compatible:
        errors.append(msg)
    
    return errors


def list_versions():
    """List all known Firecracker versions and their status."""
    print("Firecracker Compatibility Matrix")
    print("=" * 70)
    print(f"{'Version':<12} {'Status':<12} {'Snapshot':<10} {'Max vCPUs':<10} Notes")
    print("-" * 70)
    
    for version_str, info in sorted(COMPATIBILITY_MATRIX.items(), 
                                    key=lambda x: parse_version(x[0]), 
                                    reverse=True):
        snapshot = info.get("snapshot_format", "N/A")
        max_vcpus = info.get("max_vcpus", "N/A")
        print(f"{version_str:<12} {info['status']:<12} {snapshot:<10} {max_vcpus:<10} {info.get('notes', '')}")


def main():
    parser = argparse.ArgumentParser(description="Firecracker Compatibility Matrix")
    parser.add_argument("--check-version", help="Check if version is compatible")
    parser.add_argument("--required-features", nargs="+", 
                        help="Required KVM features (vmx, sgx, snapshot-restore)")
    parser.add_argument("--list-versions", action="store_true", help="List all versions")
    parser.add_argument("--validate-template", type=Path, 
                        help="Validate template manifest")
    
    args = parser.parse_args()
    
    if args.list_versions:
        list_versions()
        return
    
    if args.check_version:
        compatible, msg = check_version_compatibility(
            args.check_version, 
            args.required_features
        )
        print(msg)
        sys.exit(0 if compatible else 1)
    
    if args.validate_template:
        errors = validate_template_manifest(args.validate_template)
        if errors:
            for error in errors:
                print(f"ERROR: {error}")
            sys.exit(1)
        print("Template manifest is compatible")
        return
    
    parser.print_help()


if __name__ == "__main__":
    main()