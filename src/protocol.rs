use anyhow::{anyhow, bail, Result};

pub const PROTOCOL_VERSION: &str = "ZB1";
pub const REQUEST_PREFIX: &str = PROTOCOL_VERSION;
pub const RESPONSE_PREFIX: &str = "ZB1R";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestRequest {
    pub request_id: String,
    pub language: String,
    pub code: Vec<u8>,
    pub stdin: Vec<u8>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestResponse {
    pub request_id: String,
    pub exit_code: i32,
    pub error_type: String,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub recycle_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFrame {
    pub response: GuestResponse,
    pub frame_start: usize,
    pub frame_end: usize,
}

pub fn fnv1a32(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for &b in data {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

pub fn hex_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for b in data {
        out.push(nibble_to_hex((b >> 4) & 0x0f));
        out.push(nibble_to_hex(b & 0x0f));
    }
    out
}

pub fn hex_decode(data: &[u8]) -> Result<Vec<u8>> {
    if !data.len().is_multiple_of(2) {
        bail!("hex payload has odd length");
    }
    let mut out = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        let hi = hex_to_nibble(chunk[0]).ok_or_else(|| anyhow!("invalid hex digit"))?;
        let lo = hex_to_nibble(chunk[1]).ok_or_else(|| anyhow!("invalid hex digit"))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

pub fn encode_request_frame(req: &GuestRequest) -> Vec<u8> {
    let code_hex = hex_encode(&req.code);
    let stdin_hex = hex_encode(&req.stdin);
    let mut body = Vec::with_capacity(req.request_id.len() + code_hex.len() + stdin_hex.len());
    body.extend_from_slice(req.request_id.as_bytes());
    body.extend_from_slice(code_hex.as_bytes());
    body.extend_from_slice(stdin_hex.as_bytes());
    let checksum = fnv1a32(&body);
    let header = format!(
        "{} {} {} {} {} {} {:08x}\n",
        REQUEST_PREFIX,
        req.request_id.len(),
        req.language,
        req.timeout_ms,
        code_hex.len(),
        stdin_hex.len(),
        checksum
    );
    let mut frame = header.into_bytes();
    frame.extend_from_slice(&body);
    frame
}

#[allow(dead_code)]
pub fn encode_response_frame(resp: &GuestResponse) -> Vec<u8> {
    let stdout_hex = hex_encode(&resp.stdout);
    let stderr_hex = hex_encode(&resp.stderr);
    let mut body = Vec::with_capacity(resp.request_id.len() + stdout_hex.len() + stderr_hex.len());
    body.extend_from_slice(resp.request_id.as_bytes());
    body.extend_from_slice(stdout_hex.as_bytes());
    body.extend_from_slice(stderr_hex.as_bytes());
    let checksum = fnv1a32(&body);
    let flags = (if resp.stdout_truncated { 1 } else { 0 }) | (if resp.stderr_truncated { 2 } else { 0 }) | (if resp.recycle_requested { 4 } else { 0 });
    let header = format!(
        "{} {} {} {} {} {} {} {:08x}\n",
        RESPONSE_PREFIX,
        resp.request_id.len(),
        resp.exit_code,
        resp.error_type,
        stdout_hex.len(),
        stderr_hex.len(),
        flags,
        checksum
    );
    let mut frame = header.into_bytes();
    frame.extend_from_slice(&body);
    frame
}

pub fn find_response_frame(buffer: &[u8]) -> Option<Result<ParsedFrame>> {
    let prefix = RESPONSE_PREFIX.as_bytes();
    let mut start = 0usize;
    while start < buffer.len() {
        let rel = buffer[start..].windows(prefix.len()).position(|w| w == prefix)?;
        let frame_start = start + rel;
        let line_end = match buffer[frame_start..].iter().position(|&b| b == b'\n') {
            Some(pos) => frame_start + pos,
            None => return None,
        };
        let header = match std::str::from_utf8(&buffer[frame_start..line_end]) {
            Ok(v) => v,
            Err(e) => return Some(Err(anyhow!("response header is not utf-8: {}", e))),
        };
        let parts: Vec<&str> = header.split_ascii_whitespace().collect();
        if parts.len() != 8 || parts[0] != RESPONSE_PREFIX {
            start = frame_start + prefix.len();
            continue;
        }
        let id_len: usize = match parts[1].parse() {
            Ok(v) => v,
            Err(e) => return Some(Err(anyhow!("invalid id length: {}", e))),
        };
        let exit_code: i32 = match parts[2].parse() {
            Ok(v) => v,
            Err(e) => return Some(Err(anyhow!("invalid exit code: {}", e))),
        };
        let error_type = parts[3].to_string();
        let stdout_hex_len: usize = match parts[4].parse() {
            Ok(v) => v,
            Err(e) => return Some(Err(anyhow!("invalid stdout length: {}", e))),
        };
        let stderr_hex_len: usize = match parts[5].parse() {
            Ok(v) => v,
            Err(e) => return Some(Err(anyhow!("invalid stderr length: {}", e))),
        };
        let flags: u32 = match parts[6].parse() {
            Ok(v) => v,
            Err(e) => return Some(Err(anyhow!("invalid flags: {}", e))),
        };
        let checksum = match u32::from_str_radix(parts[7], 16) {
            Ok(v) => v,
            Err(e) => return Some(Err(anyhow!("invalid checksum: {}", e))),
        };
        let body_len = id_len + stdout_hex_len + stderr_hex_len;
        let frame_end = line_end + 1 + body_len;
        if buffer.len() < frame_end {
            return None;
        }
        let body = &buffer[line_end + 1..frame_end];
        let got = fnv1a32(body);
        if got != checksum {
            return Some(Err(anyhow!(
                "response checksum mismatch: got {:08x}, expected {:08x}",
                got,
                checksum
            )));
        }
        let request_id = match std::str::from_utf8(&body[..id_len]) {
            Ok(v) => v.to_string(),
            Err(e) => return Some(Err(anyhow!("request id is not utf-8: {}", e))),
        };
        let stdout_hex = &body[id_len..id_len + stdout_hex_len];
        let stderr_hex = &body[id_len + stdout_hex_len..];
        let stdout = match hex_decode(stdout_hex) {
            Ok(v) => v,
            Err(e) => return Some(Err(anyhow!("invalid stdout hex: {}", e))),
        };
        let stderr = match hex_decode(stderr_hex) {
            Ok(v) => v,
            Err(e) => return Some(Err(anyhow!("invalid stderr hex: {}", e))),
        };
        return Some(Ok(ParsedFrame {
            response: GuestResponse {
                request_id,
                exit_code,
                error_type,
                stdout,
                stderr,
                stdout_truncated: (flags & 1) != 0,
                stderr_truncated: (flags & 2) != 0,
                recycle_requested: (flags & 4) != 0,
            },
            frame_start,
            frame_end,
        }));
    }
    None
}

fn nibble_to_hex(v: u8) -> char {
    match v {
        0..=9 => (b'0' + v) as char,
        10..=15 => (b'a' + (v - 10)) as char,
        _ => unreachable!(),
    }
}

fn hex_to_nibble(v: u8) -> Option<u8> {
    match v {
        b'0'..=b'9' => Some(v - b'0'),
        b'a'..=b'f' => Some(v - b'a' + 10),
        b'A'..=b'F' => Some(v - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_frame_round_trip() {
        let resp = GuestResponse {
            request_id: "req-1".into(),
            exit_code: 0,
            error_type: "ok".into(),
            stdout: b"hello\n".to_vec(),
            stderr: vec![],
            stdout_truncated: false,
            stderr_truncated: false,
            recycle_requested: false,
        };
        let encoded = encode_response_frame(&resp);
        let parsed = find_response_frame(&encoded).unwrap().unwrap();
        assert_eq!(parsed.response, resp);
        assert_eq!(parsed.frame_start, 0);
        assert_eq!(parsed.frame_end, encoded.len());
    }

    #[test]
    fn detects_checksum_corruption() {
        let resp = GuestResponse {
            request_id: "req-2".into(),
            exit_code: 1,
            error_type: "runtime".into(),
            stdout: vec![],
            stderr: b"boom".to_vec(),
            stdout_truncated: false,
            stderr_truncated: true,
            recycle_requested: true,
        };
        let mut encoded = encode_response_frame(&resp);
        *encoded.last_mut().unwrap() = b'0';
        let parsed = find_response_frame(&encoded).unwrap();
        assert!(parsed.is_err());
    }
}
