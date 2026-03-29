"""Resource limit application for guest workers.

This module centralizes all resource limit enforcement in one place.
Limits are applied via Python's resource module, not shell ulimit.
"""

import os
from typing import List

try:
    import resource
except ImportError:  # pragma: no cover
    resource = None


def limit_profile() -> str:
    """Get the limit profile from environment."""
    return os.environ.get("ZEROBOOT_CHILD_LIMIT_PROFILE", "guest").strip().lower() or "guest"


def apply_limits(
    memory_bytes: int,
    nofile: int = 64,
    nproc: int = 16,
    fsize_bytes: int = 2 * 1024 * 1024,
) -> List[str]:
    """Apply resource limits to the current process.
    
    Args:
        memory_bytes: Memory limit in bytes (address space)
        nofile: Maximum number of open file descriptors
        nproc: Maximum number of processes
        fsize_bytes: Maximum file size in bytes
        
    Returns:
        List of failure messages (empty if all succeeded)
    """
    if resource is None:
        return []
    
    failures: List[str] = []
    profile = limit_profile()
    
    def clamp_and_set(kind: int, desired: int) -> None:
        """Set a resource limit, clamping to hard limits."""
        soft, hard = resource.getrlimit(kind)
        if hard == resource.RLIM_INFINITY:
            target = desired
        else:
            target = min(desired, hard)
        if soft == resource.RLIM_INFINITY:
            target_soft = target
        else:
            target_soft = min(target, soft) if soft < target else target
        try:
            resource.setrlimit(kind, (target_soft, target))
        except (ValueError, OSError) as exc:
            failures.append(f"{kind}:{exc}")
    
    # Memory limit (address space) - skip in compat mode
    if profile != "compat" and hasattr(resource, "RLIMIT_AS"):
        clamp_and_set(resource.RLIMIT_AS, max(1, memory_bytes))
    
    # File descriptor limit
    clamp_and_set(resource.RLIMIT_NOFILE, max(1, nofile))
    
    # Process limit - skip in compat mode
    if profile != "compat" and hasattr(resource, "RLIMIT_NPROC"):
        clamp_and_set(resource.RLIMIT_NPROC, max(1, nproc))
    
    # File size limit
    clamp_and_set(resource.RLIMIT_FSIZE, max(1, fsize_bytes))
    
    # Disable core dumps
    if hasattr(resource, "RLIMIT_CORE"):
        try:
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        except (ValueError, OSError):
            pass  # Non-fatal
    
    return failures
