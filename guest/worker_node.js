const fs = require('fs');
const vm = require('vm');

const MAX_STDOUT = 64 * 1024;
const MAX_STDERR = 64 * 1024;
const FLAG_STDOUT_TRUNCATED = 1;
const FLAG_STDERR_TRUNCATED = 2;
const FLAG_RECYCLE_REQUESTED = 4;
const MAX_REQUESTS = parseInt(process.env.ZEROBOOT_WORKER_MAX_REQUESTS || '128', 10);
let requestCount = 0;

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

process.stdout.write('READY\n');

while (true) {
  try {
    const parts = readLine().trim().split(/\s+/);
    if (parts.length !== 5 || parts[0] !== 'WRK1') {
      writeResponse(Buffer.from('error'), -1, 'protocol', Buffer.alloc(0), Buffer.from('invalid worker request header'), FLAG_RECYCLE_REQUESTED);
      break;
    }
    const idLen = parseInt(parts[1], 10);
    const timeoutMs = parseInt(parts[2], 10);
    const codeLen = parseInt(parts[3], 10);
    const stdinLen = parseInt(parts[4], 10);
    const requestId = readExact(idLen);
    const code = readExact(codeLen).toString('utf8');
    const stdinData = readExact(stdinLen).toString('utf8');

    const stdoutParts = [];
    const stderrParts = [];
    let exitCode = 0;
    let errorType = 'ok';
    let recycle = false;
    const sandbox = {
      console: {
        log: (...args) => stdoutParts.push(args.join(' ') + '\n'),
        error: (...args) => stderrParts.push(args.join(' ') + '\n'),
      },
      TextEncoder,
      TextDecoder,
      stdin: stdinData,
    };
    try {
      const script = new vm.Script(code, { filename: '<zeroboot>' });
      script.runInNewContext(sandbox, { timeout: Math.max(timeoutMs, 1) });
    } catch (err) {
      exitCode = err && /Script execution timed out/.test(String(err)) ? -1 : 1;
      errorType = exitCode === -1 ? 'timeout' : 'runtime';
      stderrParts.push((err && err.stack) ? err.stack + '\n' : String(err) + '\n');
      recycle = true;
    }

    let stdout = Buffer.from(stdoutParts.join(''), 'utf8');
    let stderr = Buffer.from(stderrParts.join(''), 'utf8');
    let flags = 0;
    let tmp;
    [stdout, tmp] = truncate(stdout, MAX_STDOUT); if (tmp) { flags |= FLAG_STDOUT_TRUNCATED; recycle = true; }
    [stderr, tmp] = truncate(stderr, MAX_STDERR); if (tmp) { flags |= FLAG_STDERR_TRUNCATED; recycle = true; }
    requestCount += 1;
    if (requestCount >= MAX_REQUESTS) recycle = true;
    if (recycle) flags |= FLAG_RECYCLE_REQUESTED;
    writeResponse(requestId, exitCode, errorType, stdout, stderr, flags);
    if (recycle) break;
  } catch (err) {
    if (/worker stdin closed/.test(String(err))) break;
    const stderr = Buffer.from((err && err.stack) ? err.stack : String(err), 'utf8');
    writeResponse(Buffer.from('error'), -1, 'internal', Buffer.alloc(0), stderr.subarray(0, MAX_STDERR), FLAG_RECYCLE_REQUESTED | (stderr.length > MAX_STDERR ? FLAG_STDERR_TRUNCATED : 0));
    break;
  }
}
