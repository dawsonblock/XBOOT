#!/usr/bin/env node
const fs = require('fs');
const { spawnSync } = require('child_process');

const MAX_STDOUT = 64 * 1024;
const MAX_STDERR = 64 * 1024;
const FLAG_STDOUT_TRUNCATED = 1;
const FLAG_STDERR_TRUNCATED = 2;
const CHILD_TMP_BYTES = 16 * 1024 * 1024;
const CHILD_MEMORY_BYTES = 512 * 1024 * 1024;
const CHILD_NOFILE = 64;
const CHILD_NPROC = 16;
const CHILD_FSIZE_BYTES = 2 * 1024 * 1024;
const TRUNCATION_MARKER = Buffer.from('\n[truncated]\n', 'utf8');

function readExact(n) {
  const chunks = [];
  let total = 0;
  while (total < n) {
    const chunk = Buffer.alloc(n - total);
    const read = fs.readSync(0, chunk, 0, n - total, null);
    if (read === 0) throw new Error('worker stdin closed');
    chunks.push(chunk.subarray(0, read));
    total += read;
  }
  return Buffer.concat(chunks, n);
}

function readLine() {
  const bytes = [];
  const buf = Buffer.alloc(1);
  while (true) {
    const read = fs.readSync(0, buf, 0, 1, null);
    if (read === 0) throw new Error('worker stdin closed');
    if (buf[0] === 10) break;
    if (buf[0] !== 13) bytes.push(buf[0]);
  }
  return Buffer.from(bytes).toString('utf8');
}

function truncateWithMarker(buf, max) {
  if (buf.length <= max) return [buf, false];
  if (max <= TRUNCATION_MARKER.length) return [TRUNCATION_MARKER.subarray(0, max), true];
  return [Buffer.concat([buf.subarray(0, max - TRUNCATION_MARKER.length), TRUNCATION_MARKER]), true];
}

function writeResponse(requestId, exitCode, errorType, stdout, stderr, flags) {
  const header = `WRK1R ${requestId.length} ${exitCode} ${errorType} ${stdout.length} ${stderr.length} ${flags}\n`;
  fs.writeSync(1, Buffer.from(header, 'utf8'));
  fs.writeSync(1, requestId);
  fs.writeSync(1, stdout);
  fs.writeSync(1, stderr);
}

function parseChildResponse(data) {
  const newlineIndex = data.indexOf(0x0a);
  if (newlineIndex === -1) throw new Error('invalid child response');
  const header = data.subarray(0, newlineIndex).toString('utf8').trim().split(/\s+/);
  if (header.length !== 7 || header[0] !== 'WRK1R') throw new Error('malformed child response');
  const requestIdLen = parseInt(header[1], 10);
  const stdoutLen = parseInt(header[4], 10);
  const stderrLen = parseInt(header[5], 10);
  const payload = data.subarray(newlineIndex + 1 + requestIdLen);
  return [
    parseInt(header[2], 10),
    header[3],
    payload.subarray(0, stdoutLen),
    payload.subarray(stdoutLen, stdoutLen + stderrLen),
    parseInt(header[6], 10),
  ];
}

function minimalChildEnv() {
  const env = {
    PATH: process.env.PATH || '/usr/local/bin:/usr/bin:/bin',
    HOME: '/tmp',
    TMPDIR: '/tmp',
    TMP: '/tmp',
    TEMP: '/tmp',
    LANG: 'C.UTF-8',
    LC_ALL: 'C.UTF-8',
    ZEROBOOT_OFFLINE: '1',
  };
  const profile = process.env.ZEROBOOT_CHILD_LIMIT_PROFILE;
  if (profile) {
    env.ZEROBOOT_CHILD_LIMIT_PROFILE = profile;
  }
  return env;
}

function limitProfile() {
  const value = String(process.env.ZEROBOOT_CHILD_LIMIT_PROFILE || 'guest').trim().toLowerCase();
  return value || 'guest';
}

function childCommand(timeoutMs) {
  const childScript = process.env.ZEROBOOT_CHILD_SCRIPT || '/zeroboot/worker_child.js';
  const cpuSeconds = Math.max(1, Math.ceil((timeoutMs + 1999) / 1000));
  const memoryKiB = Math.max(1, Math.floor(CHILD_MEMORY_BYTES / 1024));
  const fileKiB = Math.max(1, Math.floor(CHILD_FSIZE_BYTES / 1024));
  const shell = [
    `ulimit -t ${cpuSeconds}`,
    `ulimit -n ${CHILD_NOFILE}`,
    `ulimit -f ${fileKiB}`,
  ];
  if (limitProfile() !== 'compat') {
    shell.push(`ulimit -v ${memoryKiB}`);
    shell.push(`ulimit -u ${CHILD_NPROC}`);
  }
  shell.push(`exec "${process.execPath}" "${childScript}"`);
  return ['/bin/sh', ['-c', shell.join('; ')]];
}

function spawnChildExecutor(requestId, timeoutMs, code, stdinData) {
  const payload = Buffer.from(JSON.stringify({
    request_id: requestId.toString('utf8'),
    timeout_ms: timeoutMs,
    code,
    stdin: stdinData,
    limits: {
      stdout_bytes: MAX_STDOUT,
      stderr_bytes: MAX_STDERR,
      tmp_bytes: CHILD_TMP_BYTES,
      memory_bytes: CHILD_MEMORY_BYTES,
      nofile: CHILD_NOFILE,
      nproc: CHILD_NPROC,
      fsize_bytes: CHILD_FSIZE_BYTES,
    },
  }), 'utf8');

  const [command, args] = childCommand(timeoutMs);
  const result = spawnSync(command, args, {
    input: payload,
    env: minimalChildEnv(),
    encoding: null,
    timeout: Math.max(timeoutMs + 2000, 5000),
    maxBuffer: MAX_STDOUT + MAX_STDERR + 4096,
  });

  if (result.error && result.error.code === 'ETIMEDOUT') {
    return [-1, 'timeout', Buffer.alloc(0), Buffer.from('execution timed out\n', 'utf8'), 0];
  }
  if (result.error) {
    return [-1, 'internal', Buffer.alloc(0), Buffer.from(String(result.error.message || result.error), 'utf8'), 0];
  }
  if (result.stdout && result.stdout.toString('utf8').startsWith('WRK1R ')) {
    try {
      return parseChildResponse(result.stdout);
    } catch (err) {
      const [stderr, stderrTruncated] = truncateWithMarker(
        Buffer.from(`malformed child response: ${String(err.message || err)}\n`, 'utf8'),
        MAX_STDERR,
      );
      return [-1, 'protocol', Buffer.alloc(0), stderr, stderrTruncated ? FLAG_STDERR_TRUNCATED : 0];
    }
  }

  let flags = 0;
  let stdout;
  let stderr;
  let stdoutTruncated;
  let stderrTruncated;
  [stdout, stdoutTruncated] = truncateWithMarker(result.stdout || Buffer.alloc(0), MAX_STDOUT);
  [stderr, stderrTruncated] = truncateWithMarker(result.stderr || Buffer.alloc(0), MAX_STDERR);
  if (result.signal) {
    const detail = Buffer.from(`child exited by signal ${result.signal}\n`, 'utf8');
    [stderr, stderrTruncated] = truncateWithMarker(Buffer.concat([stderr, detail]), MAX_STDERR);
    if (stdoutTruncated) flags |= FLAG_STDOUT_TRUNCATED;
    if (stderrTruncated) flags |= FLAG_STDERR_TRUNCATED;
    return [-1, 'internal', stdout, stderr, flags];
  }
  if (stdoutTruncated) flags |= FLAG_STDOUT_TRUNCATED;
  if (stderrTruncated) flags |= FLAG_STDERR_TRUNCATED;
  return [result.status || -1, result.status === 0 ? 'ok' : 'internal', stdout, stderr, flags];
}

process.stdout.write('READY\n');

while (true) {
  try {
    const parts = readLine().trim().split(/\s+/);
    if (parts.length !== 5 || parts[0] !== 'WRK1') {
      writeResponse(Buffer.from('error'), -1, 'protocol', Buffer.alloc(0), Buffer.from('invalid worker request header'), 0);
      continue;
    }
    const requestId = readExact(parseInt(parts[1], 10));
    const timeoutMs = parseInt(parts[2], 10);
    const code = readExact(parseInt(parts[3], 10)).toString('utf8');
    const stdinData = readExact(parseInt(parts[4], 10)).toString('utf8');
    const [exitCode, errorType, stdout, stderr, flags] = spawnChildExecutor(requestId, timeoutMs, code, stdinData);
    writeResponse(requestId, exitCode, errorType, stdout, stderr, flags);
  } catch (err) {
    if (/worker stdin closed/.test(String(err))) break;
    const stderr = Buffer.from((err && err.stack) ? err.stack : String(err), 'utf8');
    writeResponse(Buffer.from('error'), -1, 'internal', Buffer.alloc(0), stderr.subarray(0, MAX_STDERR), stderr.length > MAX_STDERR ? FLAG_STDERR_TRUNCATED : 0);
    break;
  }
}
