#!/usr/bin/env node
// ABOUTME: Checks marker durability ordering through real file and helper processes.
// ABOUTME: Injects one directory fsync error in owned Linux fixtures and retains every artifact.
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { createHash } from 'node:crypto';
const [binary, output] = process.argv.slice(2);
if (process.platform !== 'linux' || process.argv.length !== 4 || !isAbsolute(binary ?? '') || !isAbsolute(output ?? '')) {
  throw new Error('usage: directory_sync.mjs ABS_PROCESS_TEST_BINARY ABS_EXISTING_PRIVATE_OUTPUT_DIRECTORY');
}
const root = await mkdtemp(join(output, 'directory-sync-'));
const receipt = { schema_version: 1, root, cases: [], result: 'incomplete', binary_sha256: createHash('sha256').update(await readFile(binary)).digest('hex') };
const launch = async (name, directory, mode, injected = false, traced = true) => {
  const trace = join(root, `${name}.trace`);
  const args = traced ? ['-f', '-y', '-e', 'trace=fsync,execve', '-o', trace,
    ...(injected ? ['-e', 'inject=fsync:error=EIO:when=2'] : []), binary, 'directory-sync-probe', mode] : ['directory-sync-probe', mode];
  const child = spawn(traced ? '/usr/bin/strace' : binary, args, {
    cwd: directory, env: { PATH: '/usr/bin:/bin', LANG: 'C', EARPH_CREDENTIAL_TEST_ROOT: directory }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let chunks = [];
  let bytes = 0;
  let killTimer;
  const stop = () => { if (killTimer) return; child.kill('SIGTERM'); killTimer = setTimeout(() => child.kill('SIGKILL'), 2000); };
  for (const pipe of [child.stdout, child.stderr]) pipe.on('data', (chunk) => {
    const left = Math.max(0, 8192 - bytes); if (left) chunks.push(chunk.subarray(0, left)); bytes += chunk.length;
    if (bytes > 8192) stop();
  });
  const timer = setTimeout(stop, 20000);
  const result = await new Promise((accept) => {
    child.once('error', () => accept({ code: null, signal: 'spawn-error' }));
    child.once('exit', (code, signal) => accept({ code, signal }));
  });
  clearTimeout(timer); clearTimeout(killTimer);
  await writeFile(join(root, `${name}.log`), Buffer.concat(chunks), { mode: 0o600, flag: 'wx' });
  chunks = [];
  receipt.cases.push({ name, pid: child.pid, ...result, output_bytes: bytes });
  if (result.code !== 0) throw new Error(`${name} failed; inspect its retained private log`);
  return traced ? readFile(trace, 'utf8') : '';
};
try {
  const failed = await mkdtemp(join(root, 'failed-'));
  const failTrace = await launch('new-marker-sync-error', failed, 'failure', true);
  const directoryFailed = (trace, path) => trace.split('\n').some((line) => line.includes('fsync(') && line.includes(`<${path}>`) && line.includes('EIO'));
  if (!directoryFailed(failTrace, failed) || failTrace.includes('"credential-helper"')) throw new Error('injected directory failure did not refuse helper execution');
  const successful = await mkdtemp(join(root, 'successful-'));
  const successTrace = await launch('successful-order', successful, 'success');
  const lines = successTrace.split('\n');
  const fileSync = lines.findIndex((line) => line.includes('fsync(') && line.includes(`<${successful}/durability.lock>`) && line.endsWith('= 0'));
  const directorySync = lines.findIndex((line) => line.includes('fsync(') && line.includes(`<${successful}>`) && line.endsWith('= 0'));
  const helper = lines.findIndex((line) => line.includes('execve(') && line.includes('"credential-helper"'));
  if (!(fileSync >= 0 && directorySync > fileSync && helper > directorySync)) throw new Error('marker file and parent directory must sync before helper execution');
  const pending = await mkdtemp(join(root, 'pending-'));
  await launch('prepare-pending-marker', pending, 'pending', false, false);
  const before = await readFile(join(pending, 'durability.lock'));
  const pendingTrace = await launch('pending-marker-sync-error', pending, 'failure', true);
  const after = await readFile(join(pending, 'durability.lock'));
  if (!before.equals(after) || pendingTrace.includes('"credential-helper"') || !directoryFailed(pendingTrace, pending)) throw new Error('directory failure changed pending state or allowed a helper');
  receipt.result = 'passed';
} catch (error) {
  receipt.result = 'failed'; receipt.error = error instanceof Error ? error.message : 'directory-sync check failed'; process.exitCode = 1;
} finally {
  await writeFile(join(root, 'receipt.json'), JSON.stringify(receipt, null, 2), { mode: 0o600, flag: 'wx' });
}
console.log(JSON.stringify({ result: receipt.result, receipt: join(root, 'receipt.json') }));
