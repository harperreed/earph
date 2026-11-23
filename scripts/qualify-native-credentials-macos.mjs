#!/usr/bin/env node
// ABOUTME: Qualifies the credential crate and an explicit synthetic keychain on a hosted Mac.
// ABOUTME: Preserves private build artifacts, logs, keychain files, and owned-child receipts.
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile } from 'node:fs/promises';
import { join, dirname, isAbsolute } from 'node:path';
import { randomBytes, createHash } from 'node:crypto';

const cargo = process.argv[2];
if (process.platform !== 'darwin' || process.env.GITHUB_ACTIONS !== 'true' || !isAbsolute(process.env.RUNNER_TEMP ?? '') || !cargo || !isAbsolute(cargo) || process.argv.length !== 3) {
  throw new Error('usage on a disposable hosted Mac: qualify-native-credentials-macos.mjs ABS_CARGO_1_98_1');
}
const root = await mkdtemp(join(process.env.RUNNER_TEMP, 'earph-credentials-'));
const nativeRoot = join(root, 'native');
const processRoot = join(root, 'process');
for (const path of [nativeRoot, processRoot]) await mkdir(path, { mode: 0o700 });
const token = randomBytes(24).toString('hex');
await writeFile(join(nativeRoot, 'owner-marker'), token, { mode: 0o600, flag: 'wx' });
const env = { ...process.env, CARGO: cargo, RUSTC: join(dirname(cargo), 'rustc'),
  PATH: `${dirname(cargo)}:${process.env.PATH}`, CARGO_TARGET_DIR: join(root, 'target'),
  CARGO_INCREMENTAL: '0', CARGO_CACHE_AUTO_CLEAN_FREQUENCY: 'never', CARGO_BUILD_JOBS: '1',
  EARPH_CREDENTIAL_TEST_ROOT: processRoot, EARPH_CREDENTIAL_TEST_KEYCHAIN_ROOT: nativeRoot,
  EARPH_CREDENTIAL_TEST_KEYCHAIN_TOKEN: token };
const receipt = { schema_version: 1, root, operations: [], result: 'incomplete' };
const run = async (name, executable, args, timeout) => {
  const child = spawn(executable, args, { env, stdio: ['ignore', 'pipe', 'pipe'] });
  const chunks = [];
  const stdoutChunks = [];
  let bytes = 0;
  let killTimer;
  const stop = () => {
    if (killTimer) return;
    child.kill('SIGTERM');
    killTimer = setTimeout(() => child.kill('SIGKILL'), 2000);
  };
  for (const pipe of [child.stdout, child.stderr]) pipe.on('data', (chunk) => {
    const remaining = Math.max(0, 1024 * 1024 - bytes);
    if (remaining) {
      const kept = chunk.subarray(0, remaining);
      chunks.push(kept);
      if (pipe === child.stdout) stdoutChunks.push(kept);
    }
    bytes += chunk.length;
    if (bytes > 1024 * 1024) stop();
  });
  const timer = setTimeout(stop, timeout);
  const result = await new Promise((accept) => {
    child.once('error', () => accept({ code: null, signal: 'spawn-error' }));
    child.once('exit', (code, signal) => accept({ code, signal }));
  });
  clearTimeout(timer);
  clearTimeout(killTimer);
  const data = Buffer.concat(chunks);
  await writeFile(join(root, `${name}.log`), data, { mode: 0o600, flag: 'wx' });
  receipt.operations.push({ name, executable, args, pid: child.pid, ...result, output_bytes: bytes });
  if (result.code !== 0) throw new Error(`${name} failed; inspect its private log`);
  return Buffer.concat(stdoutChunks).toString();
};
try {
  const version = await run('cargo-version', cargo, ['--version'], 10000);
  if (!version.startsWith('cargo 1.98.1 ')) throw new Error('the supplied Cargo version differs from 1.98.1');
  await run('clippy', cargo, ['clippy', '-p', 'earph-credentials', '--all-targets', '--locked', '--', '-D', 'warnings'], 1800000);
  await run('ordinary-tests', cargo, ['test', '-p', 'earph-credentials', '--locked'], 1800000);
  const build = await run('native-build', cargo, ['test', '-p', 'earph-credentials', '--test', 'native_macos', '--no-run', '--locked', '--message-format=json'], 1800000);
  const artifact = build.split('\n').filter(Boolean).map((line) => JSON.parse(line)).find((value) => value.reason === 'compiler-artifact' && value.target.name === 'native_macos' && value.executable);
  if (!artifact) throw new Error('the native test executable was not reported');
  const helper = join(root, 'target', 'debug', 'earph-credential-helper');
  const digest = async (path) => createHash('sha256').update(await readFile(path)).digest('hex');
  receipt.helper_sha256 = await digest(helper);
  receipt.test_sha256 = await digest(artifact.executable);
  await run('isolated-keychain', artifact.executable, ['--ignored', '--exact', 'isolated_keychain', '--nocapture'], 60000);
  receipt.result = 'passed';
} catch (error) {
  receipt.result = 'failed';
  receipt.error = error instanceof Error ? error.message : 'qualification failed';
  process.exitCode = 1;
} finally {
  await writeFile(join(root, 'receipt.json'), JSON.stringify(receipt, null, 2), { mode: 0o600, flag: 'wx' });
}
console.log(JSON.stringify({ result: receipt.result, retained_root: root }));
