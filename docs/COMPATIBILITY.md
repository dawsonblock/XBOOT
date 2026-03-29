# Compatibility Matrix

The first hardened release supports one deployment matrix only:

- host OS: Ubuntu 22.04 x86_64
- init system: systemd
- virtualization: KVM with `/dev/kvm`
- Firecracker: `1.12.0`
- guest networking: offline-only
- guest template languages: Python and Node.js

Pinned artifact sources for that matrix:

- Firecracker binary:
  `https://github.com/firecracker-microvm/firecracker/releases/download/v1.12.0/firecracker-v1.12.0-x86_64.tgz`
- Firecracker binary sha256:
  `6ba205fa2f1ccad95848515deaee59e7750d38b7a0a49c5c805cd3097ab9f368`
- Guest kernel:
  `http://spec.ccfc.min.s3.amazonaws.com/firecracker-ci/v1.10/x86_64/vmlinux-5.10.223`
- Guest kernel sha256:
  `22847375721aceea63d934c28f2dfce4670b6f52ec904fae19f5145a970c1e65`
- Base Ubuntu 22.04 rootfs:
  `http://spec.ccfc.min.s3.amazonaws.com/firecracker-ci/v1.10/x86_64/ubuntu-22.04.ext4`
- Base Ubuntu 22.04 rootfs sha256:
  `040927105bd01b19e7b02cd5da5a9552b428a7f84bd5ffc22ebfce4ddf258a07`
- Python runtime:
  `/usr/bin/python3` from the Ubuntu 22.04 base rootfs, version `3.10.12`
- Node runtime:
  `https://nodejs.org/dist/v20.20.2/node-v20.20.2-linux-x64.tar.xz`
- Node runtime sha256:
  `df770b2a6f130ed8627c9782c988fda9669fa23898329a61a871e32f965e007d`

Upstream note:

- Firecracker upstream publishes the `1.12.x` binary release on GitHub.
- The Firecracker CI bucket no longer exposes an Ubuntu 22.04 artifact set under the `v1.12` prefix.
- This repo therefore pins Firecracker `1.12.0` together with the last official Ubuntu 22.04 base image and 5.10 kernel still published in the Firecracker CI bucket (`v1.10/x86_64`).
- That matrix is explicit and repo-owned. It must be validated on the target Ubuntu 22.04 KVM hosts before production promotion.

Startup verification fails closed when any of these conditions are not met:

- `/dev/kvm` missing
- unified cgroup v2 missing
- Firecracker version mismatch
- Firecracker binary sha256 mismatch
- template manifest verification failure
- disk or inode watermarks below `ZEROBOOT_MIN_FREE_BYTES` / `ZEROBOOT_MIN_FREE_INODES`
