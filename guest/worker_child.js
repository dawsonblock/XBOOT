#!/usr/bin/env node
const fs = require('fs');
const os = require('os');
const path = require('path');
const vm = require('vm');

const FLAG_STDOUT_TRUNCATED = 1;
const FLAG_STDERR_TRUNCATED = 2;
const TRUNCATION_MARKER = Buffer.from('\n[truncated]\n', 'utf8');

function limitProfile() {
  const value = String(process.env.ZEROBOOT_CHILD_LIMIT_PROFILE || 'guest').trim().toLowerCase();
  return value || 'guest';
}

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
  let requestId = Buffer.from('error', 'utf8');
  let exitCode = -1;
  let errorType = 'internal';
  let maxStdout = 64 * 1024;
  let maxStderr = 64 * 1024;
  let maxTmpBytes = 16 * 1024 * 1024;
  let scratch = null;
  const stdoutParts = [];
  const stderrParts = [];
  const originalEnv = process.env;

  try {
    const payload = JSON.parse(fs.readFileSync(0, 'utf8'));
    requestId = Buffer.from(String(payload.request_id || 'error'), 'utf8');
    const timeoutMs = Math.max(1, Number(payload.timeout_ms || 30000));
    const code = String(payload.code || '');
    const stdinData = String(payload.stdin || '');
    const limits = payload.limits || {};
    maxStdout = Number(limits.stdout_bytes || maxStdout);
    maxStderr = Number(limits.stderr_bytes || maxStderr);
    maxTmpBytes = Number(limits.tmp_bytes || maxTmpBytes);
    scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'zeroboot-'));

    const childEnv = {
      HOME: scratch,
      TMPDIR: scratch,
      TMP: scratch,
      TEMP: scratch,
      ZEROBOOT_TMPDIR: scratch,
      ZEROBOOT_OFFLINE: '1',
      ZEROBOOT_CHILD_LIMIT_PROFILE: limitProfile(),
    };
    process.env = childEnv;

    const sandbox = {
      console: {
        log: (...args) => stdoutParts.push(args.join(' ') + '\n'),
        error: (...args) => stderrParts.push(args.join(' ') + '\n'),
      },
      TextEncoder,
      TextDecoder,
      stdin: stdinData,
      process: {
        env: { ...childEnv },
      },
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

    exitCode = 0;
    errorType = 'ok';
    const script = new vm.Script(code, { filename: '<zeroboot>' });
    script.runInNewContext(sandbox, { timeout: timeoutMs });
    if (directorySize(scratch) > maxTmpBytes) {
      exitCode = 1;
      errorType = 'runtime';
      stderrParts.push('temporary directory quota exceeded\n');
    }
  } catch (err) {
    const text = String(err);
    if (/Script execution timed out/.test(text)) {
      exitCode = -1;
      errorType = 'timeout';
    } else if (errorType === 'ok') {
      exitCode = 1;
      errorType = 'runtime';
    } else {
      exitCode = -1;
      errorType = 'internal';
    }
    stderrParts.push((err && err.stack) ? err.stack + '\n' : text + '\n');
  } finally {
    process.env = originalEnv;
    if (scratch) {
      fs.rmSync(scratch, { recursive: true, force: true });
    }
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
