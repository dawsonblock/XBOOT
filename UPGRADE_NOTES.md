# Upgrade Notes

This pass focuses on **artifact integrity** and **startup trust boundaries**.

## What changed

The service now treats template verification as a first-class startup step.
A template is only loaded when its manifest matches the artifact set on disk.
Bad templates are quarantined and surfaced through `/ready` and metrics.

### New startup checks

- manifest JSON must parse
- manifest language must match the configured template language
- snapshot sizes must match manifest values
- snapshot sha256 values must match
- protocol version must match the server expectation
- optional Firecracker version check can be enforced with `ZEROBOOT_ALLOWED_FIRECRACKER_VERSION`

### New readiness model

- `/live` only checks process liveness
- `/ready` reports startup verification state only
- `/health` runs deep probes only for templates that already passed startup verification

## Why this matters

This closes a real gap in the prior pass: the server could load a template with matching sizes but tampered bytes. It can now refuse that template before serving traffic.

## Still missing for a full production bar

- warm pool
- live KVM integration CI
- signed/promoted artifact workflow
- hashed API key storage
