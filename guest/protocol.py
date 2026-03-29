"""Protocol framing for guest worker communication.

Handles encoding/decoding of the WRK1R response format.
"""

import json
from typing import Dict, Any, Tuple


def encode_response(
    request_id: bytes,
    exit_code: int,
    error_type: str,
    stdout: bytes,
    stderr: bytes,
    flags: int = 0,
) -> bytes:
    """Encode a worker response in WRK1R format.
    
    Format: WRK1R <request_id_len> <exit_code> <error_type> <stdout_len> <stderr_len> <flags>\n<request_id><stdout><stderr>
    
    Args:
        request_id: Unique request identifier bytes
        exit_code: Process exit code (0 for success, -1 for timeout/signal, >0 for error)
        error_type: Classification - "ok", "timeout", "runtime", "protocol", "internal"
        stdout: Standard output bytes
        stderr: Standard error bytes
        flags: Bitmask flags (1=stdout truncated, 2=stderr truncated)
        
    Returns:
        Complete framed response as bytes
    """
    header = f"WRK1R {len(request_id)} {exit_code} {error_type} {len(stdout)} {len(stderr)} {flags}\n"
    return (
        header.encode("utf-8")
        + request_id
        + stdout
        + stderr
    )


def decode_payload(data: bytes) -> Dict[str, Any]:
    """Decode JSON payload from stdin.
    
    Args:
        data: Raw JSON bytes from stdin
        
    Returns:
        Parsed payload dict with defaults applied
        
    Raises:
        json.JSONDecodeError: If payload is invalid JSON
    """
    payload = json.loads(data.decode("utf-8"))
    
    # Apply defaults
    return {
        "request_id": str(payload.get("request_id", "error")),
        "timeout_ms": max(1, int(payload.get("timeout_ms", 30000))),
        "code": str(payload.get("code", "")),
        "stdin": str(payload.get("stdin", "")),
        "limits": payload.get("limits", {}),
    }


def parse_response(data: bytes) -> Tuple[bytes, int, str, bytes, bytes, int]:
    """Parse a WRK1R response from bytes.
    
    Args:
        data: Raw response bytes
        
    Returns:
        Tuple of (request_id, exit_code, error_type, stdout, stderr, flags)
        
    Raises:
        ValueError: If response format is invalid
    """
    newline = data.find(b"\n")
    if newline < 0:
        raise ValueError("invalid response: no header newline")
    
    header = data[:newline].decode("utf-8", "replace").strip().split()
    if len(header) != 7 or header[0] != "WRK1R":
        raise ValueError(f"malformed response header: expected WRK1R, got {header[0] if header else 'empty'}")
    
    request_id_len = int(header[1])
    exit_code = int(header[2])
    error_type = header[3]
    stdout_len = int(header[4])
    stderr_len = int(header[5])
    flags = int(header[6])
    
    payload = data[newline + 1:]
    minimum = request_id_len + stdout_len + stderr_len
    if len(payload) < minimum:
        raise ValueError(f"truncated payload: got {len(payload)}, need {minimum}")
    
    request_id = payload[:request_id_len]
    body = payload[request_id_len:]
    stdout = body[:stdout_len]
    stderr = body[stdout_len:stdout_len + stderr_len]
    
    if not request_id:
        raise ValueError("missing request id in response")
    
    return request_id, exit_code, error_type, stdout, stderr, flags
