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

### `guest/worker_supervisor.py`

Python worker supervisor. Spawns a fresh child executor for each request, providing strong isolation between requests.

### `guest/worker_child.py`

Python child executor - runs once per request in a fresh process, then exits. Ensures no state bleeds between requests.

### `guest/worker_supervisor.js` / `guest/worker_child.js`

Node.js equivalent of the Python subprocess isolation model.

## Production Security Model

### Trust Chain (Prod Mode)
- **Startup fail-closed**: Server refuses to start without all required security config
- **Template verification**: All artifacts must have SHA256 hashes
- **Signature verification**: Manifest must be signed by a trusted key
- **Release channel**: Template must be promoted to configured channel (default: "prod")
- **Schema version**: Only schema v1 supported in prod mode

### Guest Isolation
- **Subprocess model**: Each request runs in a fresh child process inside the same guest VM
- **Process exit**: Child exits after each request, minimizing in-memory state reuse
- **Filesystem behavior**: All requests share the same guest filesystem; any per-request scratch or cleanup is handled in user space
- **Process isolation**: Isolation between requests is provided by the OS process model within a single guest VM, not by separate VMs or mount/user namespaces

## Current boundaries

This repo still depends on external pinned artifacts that are not committed here:

- kernel image
- Python rootfs
- Node rootfs
- Firecracker binary/version pin

The manifests and scripts added in this branch document those boundaries but do not remove them.


## Worker recycle policy

Guest workers now request recycle after timeouts, runtime exceptions, output truncation, or after a bounded number of requests. The guest supervisor restarts them before the next execution.
