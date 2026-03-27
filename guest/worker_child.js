#!/usr/bin/env node
/**
 * Subprocess-based Node.js worker executor (child).
 *
 * This script is spawned as a child process for each request, providing
 * strong isolation between requests. It runs once and exits.
 */

const vm = require('vm');
const os = require('os');

const MAX_STDOUT = 64 * 1024;
const MAX_STDERR = 64 * 1024;
const FLAG_STDOUT_TRUNCATED = 1;
const FLAG_STDERR_TRUNCATED = 2;

function truncate(buf, max) {
  if (buf.length <= max) return [buf, false];
  return [buf.subarray(0, max), true];
}

function writeResponse(requestId, exitCode, errorType, stdout, stderr, flags) {
  const header = `WRK1R ${requestId.length} ${exitCode} ${errorType} ${stdout.length} ${stderr.length} ${flags}\n`;
  process.stdout.write(header);
  process.stdout.write(requestId);
  process.stdout.write(stdout);
  process.stdout.write(stderr);
}

function main() {
  // Get execution parameters from environment
  const code = process.env.ZEROBOOT_EXEC_CODE || '';
  const stdinData = process.env.ZEROBOOT_EXEC_STDIN || '';
  const timeoutMs = parseInt(process.env.ZEROBOOT_EXEC_TIMEOUT_MS || '30000', 10);
  
  // Generate a request ID for this execution
  const requestId = Buffer.from(process.env.ZEROBOOT_REQUEST_ID || 'child');
  
  const stdoutParts = [];
  const stderrParts = [];
  let exitCode = 0;
  let errorType = 'ok';
  
  const sandbox = {
    console: {
      log: (...args) => stdoutParts.push(args.join(' ') + '\n'),
      error: (...args) => stderrParts.push(args.join(' ') + '\n'),
    },
    TextEncoder,
    TextDecoder,
    stdin: stdinData,
    setTimeout: setTimeout,
    setInterval: setInterval,
    clearTimeout: clearTimeout,
    clearInterval: clearInterval,
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
    WeakMap,
    WeakSet,
    Promise,
    Symbol,
    Error,
    TypeError,
    SyntaxError,
    RangeError,
    ReferenceError,
  };
  
  try {
    const script = new vm.Script(code, { filename: '<zeroboot>' });
    script.runInNewContext(sandbox, { timeout: Math.max(timeoutMs, 1) });
  } catch (err) {
    exitCode = /Script execution timed out/.test(String(err)) ? -1 : 1;
    errorType = exitCode === -1 ? 'timeout' : 'runtime';
    stderrParts.push((err && err.stack) ? err.stack + '\n' : String(err) + '\n');
  }
  
  let stdout = Buffer.from(stdoutParts.join(''), 'utf8');
  let stderr = Buffer.from(stderrParts.join(''), 'utf8');
  let flags = 0;
  
  let tmp;
  [stdout, tmp] = truncate(stdout, MAX_STDOUT); if (tmp) flags |= FLAG_STDOUT_TRUNCATED;
  [stderr, tmp] = truncate(stderr, MAX_STDERR); if (tmp) flags |= FLAG_STDERR_TRUNCATED;
  
  writeResponse(requestId, exitCode, errorType, stdout, stderr, flags);
}

main();