# API Reference

## Endpoints

| Endpoint | Method | Description |
|---|---|---|
| `/v1/exec` | POST | Execute one snippet in a fresh forked VM |
| `/v1/exec/batch` | POST | Execute a bounded batch |
| `/ready` | GET | Startup verification and quarantine state |
| `/health` | GET | Cached deep guest probe |
| `/v1/metrics` | GET | Prometheus-style metrics |

## POST /v1/exec

```json
{
  "code": "print(1 + 1)",
  "language": "python",
  "timeout_seconds": 5,
  "stdin": ""
}
```

### Request limits

These are enforced server-side. Defaults can be changed with environment variables.

- request body: `ZEROBOOT_MAX_REQUEST_BODY_BYTES` (default 256 KiB)
- code size: `ZEROBOOT_MAX_CODE_BYTES` (default 128 KiB)
- stdin size: `ZEROBOOT_MAX_STDIN_BYTES` (default 64 KiB)
- timeout: `ZEROBOOT_MAX_TIMEOUT_SECS` (default 30)
- batch size: `ZEROBOOT_MAX_BATCH_SIZE` (default 16)

### Response

```json
{
  "id": "019cf684-1fd5-73c0-9299-52253f9aa79c",
  "stdout": "2\n",
  "stderr": "",
  "exit_code": 0,
  "fork_time_ms": 0.75,
  "exec_time_ms": 7.2,
  "total_time_ms": 8.0,
  "runtime_error_type": "ok",
  "stdout_truncated": false,
  "stderr_truncated": false
}
```

`runtime_error_type` is one of:

- `ok`
- `runtime`
- `timeout`
- `protocol`
- `validation`
- `fork`
- `transport`
- `internal`

If a template was quarantined at startup, execution now returns a validation error with the quarantine detail instead of a vague missing-template error.

## POST /v1/exec/batch

```json
{
  "executions": [
    {"code": "print(1)", "language": "python"},
    {"code": "console.log(2)", "language": "node"}
  ]
}
```

Batch execution remains bounded. Oversized batches are rejected before work starts.

## GET /ready

`/ready` reports startup verification only. It does not run guest code.

Example:

```json
{
  "status": "degraded",
  "templates": {
    "python": {"ready": true, "detail": "startup verification ok"},
    "node": {"ready": false, "detail": "quarantined: template manifest missing snapshot_mem_sha256"}
  }
}
```

## GET /health

Health runs a real guest probe per template that passed startup verification:

- Python: `print("ok")`
- Node: `console.log("ok")`

Results are cached for `ZEROBOOT_HEALTH_CACHE_TTL_SECS`.

## Authentication

Two modes exist:

- `ZEROBOOT_AUTH_MODE=dev` — missing keys file is allowed
- `ZEROBOOT_AUTH_MODE=prod` — startup fails without a readable keys file

Keys file format:

```json
[
  {
    "id": "key_ab12cd34",
    "prefix": "zb_live_ab12cd34",
    "hash": "8f7b9b0d9c1a...",
    "created_at": 1711200000000,
    "disabled_at": null,
    "label": "deploy-1"
  }
]
```

Use the standard bearer header:

```text
Authorization: Bearer zb_live_ab12cd34.secret-material
```

Generate records with:

```bash
python3 scripts/make_api_keys.py --pepper-file /etc/zeroboot/pepper --output /etc/zeroboot/api_keys.json
```

The script prints bearer tokens once and stores only hashed records on disk.

## Rate limits and overload behavior

- invalid or missing bearer token: `401`
- tenant rate limit exceeded: `429`
- execution queue full: `429`
- runtime admission refused because disk or inode watermarks are below threshold: `503`

Timeout responses return:

- `exit_code: -1`
- `runtime_error_type: "timeout"`
- a stderr message explaining the timeout

## Proxy handling

Forwarded headers are ignored unless the direct peer IP appears in `ZEROBOOT_TRUSTED_PROXIES`.

## Logging

Requests are logged to `/var/lib/zeroboot/requests.jsonl` by default.
Code content is not logged unless `ZEROBOOT_LOG_CODE=true`.

## Metrics

Additional metrics now include:

- `zeroboot_template_quarantines`
- `zeroboot_template_ready{language=...}`
- `zeroboot_language_executions_total{language=...,result=...}`
- `zeroboot_execution_slots_available`, `zeroboot_execution_slots_used`, `zeroboot_execution_slots_capacity`
- `zeroboot_memory_usage_bytes`
- queue wait histogram
