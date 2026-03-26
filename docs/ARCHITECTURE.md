# Architecture

## Runtime path

```text
HTTP request
  -> validate auth / limits / queue slot
  -> fork VM from snapshot
  -> send framed request over serial
  -> guest supervisor decodes request
  -> guest supervisor forwards request to Python or Node worker
  -> worker returns structured stdout/stderr/exit_code
  -> guest supervisor returns framed response to host
  -> host validates checksum and lengths
  -> API returns normalized JSON
```

## Host layers

### `src/vmm/kvm.rs`

Restores VM state from a Firecracker snapshot and maps snapshot memory through `MAP_PRIVATE` for copy-on-write isolation.

### `src/vmm/firecracker.rs`

Creates templates and now waits for an explicit guest readiness handshake instead of sleeping blindly.

### `src/protocol.rs`

Defines the serial protocol and validates:

- message framing
- payload length
- checksum
- response id matching
- hex payload decoding

### `src/api/handlers.rs`

Applies:

- auth mode policy
- trusted-proxy IP extraction
- rate limiting
- request size validation
- bounded concurrency
- guest health probes
- metadata-only request logging by default

## Guest layers

### `guest/init.c`

Acts as a supervisor inside the guest. It starts worker processes before snapshotting, prints `ZEROBOOT_READY ...`, and mediates all execution requests.

### `guest/worker.py`

Persistent Python worker. It executes code in-process, captures stdout/stderr, and returns a framed worker response.

### `guest/worker_node.js`

Persistent Node worker using the `vm` module for bounded script execution.

## Current boundaries

This repo still depends on external pinned artifacts that are not committed here:

- kernel image
- Python rootfs
- Node rootfs
- Firecracker binary/version pin

The manifests and scripts added in this branch document those boundaries but do not remove them.


## Worker recycle policy

Guest workers now request recycle after timeouts, runtime exceptions, output truncation, or after a bounded number of requests. The guest supervisor restarts them before the next execution.
