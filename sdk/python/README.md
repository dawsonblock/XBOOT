# Zeroboot Python SDK

Client for the structured Zeroboot sandbox API.

This SDK is a transport wrapper. It does not guarantee that a given deployment has numpy, pandas,
or any other package unless the server image actually contains them.

## Usage

```python
from zeroboot import Sandbox

sb = Sandbox("zb_live_your_api_key", base_url="http://127.0.0.1:8080")
result = sb.run("print(1 + 1)")
print(result.stdout)
print(result.stderr)
print(result.exit_code)
```

## Result fields

- `stdout`
- `stderr`
- `exit_code`
- `fork_time_ms`
- `exec_time_ms`
- `total_time_ms`
- `runtime_error_type`
- `stdout_truncated`
- `stderr_truncated`
