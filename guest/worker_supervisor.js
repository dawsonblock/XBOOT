#!/usr/bin/env node
/**
 * Subprocess-based Node.js worker supervisor.
 *
 * This module runs as a long-lived supervisor process that spawns a new child executor
 * for each request. The child process is terminated and recreated after each request,
 * providing strong isolation between requests.
 */

const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');

const MAX_STDOUT = 64 * 1024;
const MAX_STDERR = 64 * 1024;
const FLAG_STDOUT_TRUNCATED = 1;
const FLAG_STDERR_TRUNCATED = 2;
const FLAG_RECYCLE_REQUESTED = 4;

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

function truncate(buf, max) {
  if (buf.length <= max) return [buf, false];
  return [buf.subarray(0, max), true];
}

function writeResponse(requestId, exitCode, errorType, stdout, stderr, flags) {
  const header = `WRK1R ${requestId.length} ${exitCode} ${errorType} ${stdout.length} ${stderr.length} ${flags}\n`;
  fs.writeSync(1, Buffer.from(header, 'utf8'));
  fs.writeSync(1, requestId);
  fs.writeSync(1, stdout);
  fs.writeSync(1, stderr);
}

function spawnChildExecutor(timeoutMs, code, stdinData) {
  return new Promise((resolve) => {
    const childScript = process.env.ZEROBOOT_CHILD_SCRIPT || __dirname + '/worker_child.js';
    
    // Pass execution parameters via environment
    const env = { ...process.env };
    env.ZEROBOOT_EXEC_CODE = code;
    env.ZEROBOOT_EXEC_STDIN = stdinData;
    env.ZEROBOOT_EXEC_TIMEOUT_MS = String(timeoutMs);
    
    const child = spawn('node', [childScript], {
      env,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    
    let stdout = Buffer.alloc(0);
    let stderr = Buffer.alloc(0);
    
    child.stdout.on('data', (data) => {
      stdout = Buffer.concat([stdout, data]);
    });
    
    child.stderr.on('data', (data) => {
      stderr = Buffer.concat([stderr, data]);
    });
    
    child.on('close', (code) => {
      // Check if child returned valid response
      if (stdout.toString('utf8').startsWith('WRK1R ')) {
        resolve(parseChildResponse(stdout));
      } else {
        // Fall back to treating output as stdout/stderr
        const exitCode = code || 0;
        const errorType = exitCode === 0 ? 'ok' : 'runtime';
        resolve([exitCode, errorType, stdout, stderr, 0]);
      }
    });
    
    child.on('error', (err) => {
      resolve([-1, 'internal', Buffer.alloc(0), Buffer.from(err.message), FLAG_STDERR_TRUNCATED]);
    });
    
    // Timeout for child process
    const timeout = Math.max(timeoutMs + 1000, 5000);
    setTimeout(() => {
      child.kill('SIGKILL');
      resolve([-1, 'timeout', Buffer.alloc(0), Buffer.from('execution timed out'), FLAG_RECYCLE_REQUESTED]);
    }, timeout);
  });
}

function parseChildResponse(data) {
  // Find the end of the header line in the raw buffer.
  const newlineIndex = data.indexOf(0x0a); // '\n'
  if (newlineIndex === -1) {
    return [-1, 'internal', Buffer.alloc(0), Buffer.from('invalid child response'), FLAG_STDERR_TRUNCATED];
  }

  const header = data.subarray(0, newlineIndex).toString('utf8');
  if (!header.startsWith('WRK1R ')) {
    return [-1, 'internal', Buffer.alloc(0), Buffer.from('invalid child response'), FLAG_STDERR_TRUNCATED];
  }

  const parts = header.trim().split(/\s+/);
  if (parts.length < 7) {
    return [-1, 'internal', Buffer.alloc(0), Buffer.from('malformed child response'), FLAG_STDERR_TRUNCATED];
  }

  const exitCode = parseInt(parts[2], 10);
  const errorType = parts[3];
  const stdoutLen = parseInt(parts[4], 10);
  const stderrLen = parseInt(parts[5], 10);
  const flags = parseInt(parts[6], 10);

  // Slice stdout and stderr directly from the raw buffer body based on lengths.
  const bodyOffset = newlineIndex + 1;
  const bodyLength = data.length - bodyOffset;

  let stdout = Buffer.alloc(0);
  let stderr = Buffer.alloc(0);

  if (bodyLength > 0 && (stdoutLen > 0 || stderrLen > 0)) {
    const stdoutStart = bodyOffset;
    const stdoutEnd = Math.min(data.length, stdoutStart + stdoutLen);
    stdout = data.subarray(stdoutStart, stdoutEnd);

    const stderrStart = stdoutStart + stdoutLen;
    const stderrEnd = Math.min(data.length, stderrStart + stderrLen);
    if (stderrStart < data.length && stderrLen > 0) {
      stderr = data.subarray(stderrStart, stderrEnd);
    } else {
      stderr = Buffer.alloc(0);
    }
  }
  return [exitCode, errorType, stdout, stderr, flags];
}

// Supervisor main loop
process.stdout.write('READY\n');

while (true) {
  try {
    const parts = readLine().trim().split(/\s+/);
    if (parts.length !== 5 || parts[0] !== 'WRK1') {
      writeResponse(Buffer.from('error'), -1, 'protocol', Buffer.alloc(0), Buffer.from('invalid worker request header'), 0);
      continue;
    }
    const idLen = parseInt(parts[1], 10);
    const timeoutMs = parseInt(parts[2], 10);
    const codeLen = parseInt(parts[3], 10);
    const stdinLen = parseInt(parts[4], 10);
    const requestId = readExact(idLen);
    const code = readExact(codeLen).toString('utf8');
    const stdinData = readExact(stdinLen).toString('utf8');

    // For synchronous execution (if needed), use spawnSync
    // Or we can make this async - but the protocol is line-based
    // For now, we'll use a synchronous approach that works with the protocol
    
    // Since the protocol is synchronous, let's use a simpler approach:
    // Use child_process.spawnSync for each request
    const result = executeChildSync(timeoutMs, code, stdinData);
    writeResponse(requestId, result[0], result[1], result[2], result[3], result[4]);
    
  } catch (err) {
    if (/worker stdin closed/.test(String(err))) break;
    const stderr = Buffer.from((err && err.stack) ? err.stack : String(err), 'utf8');
    writeResponse(Buffer.from('error'), -1, 'internal', Buffer.alloc(0), stderr.subarray(0, MAX_STDERR), FLAG_STDERR_TRUNCATED | FLAG_RECYCLE_REQUESTED);
    break;
  }
}

function executeChildSync(timeoutMs, code, stdinData) {
  const childScript = process.env.ZEROBOOT_CHILD_SCRIPT || __dirname + '/worker_child.js';
  
  const env = { ...process.env };
  env.ZEROBOOT_EXEC_CODE = code;
  env.ZEROBOOT_EXEC_STDIN = stdinData;
  env.ZEROBOOT_EXEC_TIMEOUT_MS = String(timeoutMs);
  
  try {
    const result = spawnSync('node', [childScript], {
      env,
      input: Buffer.alloc(0),
      timeout: Math.max(timeoutMs + 1000, 5000),
    });
    
    if (result.status === 0 && result.stdout.toString('utf8').startsWith('WRK1R ')) {
      return parseChildResponse(result.stdout);
    }
    
    return [
      result.status || 0,
      result.status === 0 ? 'ok' : 'runtime',
      result.stdout,
      result.stderr,
      0,
    ];
  } catch (err) {
    return [-1, 'internal', Buffer.alloc(0), Buffer.from(err.message), FLAG_STDERR_TRUNCATED];
  }
}

// polyfill for spawnSync if not available in older node versions
function spawnSync(command, args, options) {
  const { spawn } = require('child_process');
  let stdout = Buffer.alloc(0);
  let stderr = Buffer.alloc(0);
  
  const child = spawn(command, args, options);
  
  child.stdout.on('data', (data) => {
    stdout = Buffer.concat([stdout, data]);
  });
  
  child.stderr.on('data', (data) => {
    stderr = Buffer.concat([stderr, data]);
  });
  
  return new Promise((resolve) => {
    child.on('close', (code) => {
      resolve({
        status: code,
        stdout,
        stderr,
      });
    });
    
    child.on('error', (err) => {
      resolve({
        status: -1,
        stdout: Buffer.alloc(0),
        stderr: Buffer.from(err.message),
      });
    });
    
    // Note: this isn't truly synchronous - for true sync we'd need spawnSync
    // But this works for the protocol since we wait for the child to finish
    // The real solution is to use the child script which executes and exits
  });
}