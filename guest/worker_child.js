#!/usr/bin/env node
const fs = require('fs');
const os = require('os');
const path = require('path');
const vm = require('vm');

const FLAG_STDOUT_TRUNCATED = 1;
const FLAG_STDERR_TRUNCATED = 2;
const TRUNCATION_MARKER = Buffer.from('\n[truncated]\n', 'utf8');

function truncateWithMarker(buf, max) {
  if (buf.length <= max) return [buf, false];
  if (max <= TRUNCATION_MARKER.length) return [TRUNCATION_MARKER.subarray(0, max), true];
  return [Buffer.concat([buf.subarray(0, max - TRUNCATION_MARKER.length), TRUNCATION_MARKER]), true];
}

function writeResponse(requestId, exitCode, errorType, stdout, stderr, flags) {
  const header = `WRK1R ${requestId.length} ${exitCode} ${errorType} ${stdout.length} ${stderr.length} ${flags}\n`;
  process.stdout.write(header);
  process.stdout.write(requestId);
  process.stdout.write(stdout);
  process.stdout.write(stderr);
}

function directorySize(root) {
  let total = 0;
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    const full = path.join(root, entry.name);
    if (entry.isDirectory()) {
      total += directorySize(full);
    } else if (entry.isFile()) {
      total += fs.statSync(full).size;
    }
  }
  return total;
}

function main() {
  const payload = JSON.parse(fs.readFileSync(0, 'utf8'));
  const requestId = Buffer.from(String(payload.request_id || ''), 'utf8');
  const timeoutMs = Math.max(1, Number(payload.timeout_ms || 30000));
  const code = String(payload.code || '');
  const stdinData = String(payload.stdin || '');
  const limits = payload.limits || {};
  const maxStdout = Number(limits.stdout_bytes || 64 * 1024);
  const maxStderr = Number(limits.stderr_bytes || 64 * 1024);
  const maxTmpBytes = Number(limits.tmp_bytes || 16 * 1024 * 1024);
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'zeroboot-'));

  let exitCode = 0;
  let errorType = 'ok';
  const stdoutParts = [];
  const stderrParts = [];

  const sandbox = {
    console: {
      log: (...args) => stdoutParts.push(args.join(' ') + '\n'),
      error: (...args) => stderrParts.push(args.join(' ') + '\n'),
    },
    TextEncoder,
    TextDecoder,
    stdin: stdinData,
    setTimeout,
    clearTimeout,
    Math,
    Date,
    JSON,
    Array,
    Object,
    String,
    Number,
    Boolean,
    RegExp,
    Map,
    Set,
    Promise,
    Error,
    TypeError,
    SyntaxError,
    RangeError,
    ReferenceError,
  };

  try {
    process.env = {
      HOME: scratch,
      TMPDIR: scratch,
      ZEROBOOT_TMPDIR: scratch,
      ZEROBOOT_OFFLINE: '1',
    };
    const script = new vm.Script(code, { filename: '<zeroboot>' });
    script.runInNewContext(sandbox, { timeout: timeoutMs });
    if (directorySize(scratch) > maxTmpBytes) {
      exitCode = 1;
      errorType = 'runtime';
      stderrParts.push('temporary directory quota exceeded\n');
    }
  } catch (err) {
    exitCode = /Script execution timed out/.test(String(err)) ? -1 : 1;
    errorType = exitCode === -1 ? 'timeout' : 'runtime';
    stderrParts.push((err && err.stack) ? err.stack + '\n' : String(err) + '\n');
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }

  let stdout = Buffer.from(stdoutParts.join(''), 'utf8');
  let stderr = Buffer.from(stderrParts.join(''), 'utf8');
  let flags = 0;
  let stdoutTruncated;
  let stderrTruncated;
  [stdout, stdoutTruncated] = truncateWithMarker(stdout, maxStdout);
  [stderr, stderrTruncated] = truncateWithMarker(stderr, maxStderr);
  if (stdoutTruncated) flags |= FLAG_STDOUT_TRUNCATED;
  if (stderrTruncated) flags |= FLAG_STDERR_TRUNCATED;
  writeResponse(requestId, exitCode, errorType, stdout, stderr, flags);
}

main();
