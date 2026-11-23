#!/usr/bin/env node
// ABOUTME: Runs credential software qualification on a fresh private Linux Secret Service.
// ABOUTME: Retains every file and stops only child processes created by this operator.
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { resolve, isAbsolute, join } from 'node:path';
import { randomBytes, createHash } from 'node:crypto';

const args = process.argv.slice(2);
if (args.length !== 3 || args.some((v) => !isAbsolute(v)) || process.platform !== 'linux') {
  throw new Error('usage: qualify-native-credentials.mjs ABS_HELPER ABS_NATIVE_TEST ABS_OUTPUT_DIRECTORY');
}
const [helper, test, output] = args.map((path) => resolve(path));
const digest = async (path) => createHash('sha256').update(await readFile(path)).digest('hex');
const cache = join(homedir(), '.cache');
await mkdir(cache, { recursive: true });
const root = await mkdtemp(join(cache, 'ec-'));
const token = randomBytes(24).toString('hex');
const home = join(root, 'home');
const runtime = join(root, 'runtime');
const data = join(root, 'data');
const control = join(root, 'control');
for (const path of [home, runtime, data, control, join(root, 'config'), join(root, 'cache')]) {
  await mkdir(path, { mode: 0o700 });
}
await writeFile(join(root, 'owner-marker'), token, { mode: 0o600, flag: 'wx' });
const address = `unix:path=${join(root, 'bus')}`;
const config = '<busconfig><type>session</type>' + `<listen>${address}</listen>` +
  '<auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/>' +
  '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>';
await writeFile(join(root, 'dbus.conf'), config, { mode: 0o600, flag: 'wx' });
const env = { PATH: '/usr/bin:/bin', LANG: 'C', LC_ALL: 'C', HOME: home,
  XDG_RUNTIME_DIR: runtime, XDG_DATA_HOME: data, XDG_CONFIG_HOME: join(root, 'config'),
  XDG_CACHE_HOME: join(root, 'cache'), DBUS_SESSION_BUS_ADDRESS: address,
  EARPH_CREDENTIAL_NATIVE_TEST_ROOT: root, EARPH_CREDENTIAL_NATIVE_TEST_TOKEN: token,
  EARPH_CREDENTIAL_HELPER: helper };
const children = [];
const launch = (executable, argv, options = {}) => {
  const child = spawn(executable, argv, { cwd: root, env, stdio: ['pipe', 'pipe', 'pipe'], ...options });
  const completion = new Promise((accept) => {
    child.once('error', () => accept({ code: null, signal: 'spawn-error' }));
    child.once('exit', (code, signal) => accept({ code, signal }));
  });
  const record = { child, completion, executable, argv, pid: child.pid };
  record.stdout = [];
  record.stderr = [];
  record.stdoutBytes = 0;
  record.stderrBytes = 0;
  for (const name of ['stdout', 'stderr']) {
    child[name]?.on('data', (chunk) => {
      const count = `${name}Bytes`;
      const remaining = Math.max(0, 16384 - record[count]);
      if (remaining) record[name].push(chunk.subarray(0, remaining));
      record[count] += chunk.length;
      if (record[count] > 16384) child.kill('SIGTERM');
    });
  }
  children.push(record);
  return record;
};
const pause = (ms) => new Promise((accept) => setTimeout(accept, ms));
const stop = async (record) => {
  if (record.child.exitCode === null && record.child.signalCode === null) record.child.kill('SIGTERM');
  let settled = false;
  const result = await Promise.race([record.completion.then((v) => { settled = true; return v; }), pause(2000)]);
  if (!settled) { record.child.kill('SIGKILL'); return record.completion; }
  return result;
};
const receipt = { schema_version: 1, root, helper_sha256: await digest(helper), test_sha256: await digest(test),
  operator_sha256: await digest(new URL(import.meta.url)), children: [], result: 'incomplete' };
let native;
try {
  const bus = launch('/usr/bin/dbus-daemon', [`--config-file=${join(root, 'dbus.conf')}`, '--nofork', '--nopidfile']);
  bus.child.stdin.end();
  await pause(100);
  if (bus.child.exitCode !== null) throw new Error('private bus exited before qualification');
  const keyring = launch('/usr/bin/gnome-keyring-daemon', ['--foreground', '--components=secrets', `--control-directory=${control}`, '--unlock']);
  // This fresh keyring password is synthetic. It travels only through the owned child's stdin.
  keyring.child.stdin.end(randomBytes(32).toString('hex'));
  native = launch(test, ['--ignored', '--exact', 'isolated_secret_service', '--nocapture'], { stdio: ['ignore', 'pipe', 'pipe'] });
  let text = '';
  native.child.stdout.on('data', (chunk) => {
    if (text.length + chunk.length > 8192) { native.child.kill('SIGTERM'); return; }
    text += chunk.toString();
  });
  const timer = setTimeout(() => native.child.kill('SIGTERM'), 20000);
  const result = await native.completion;
  clearTimeout(timer);
  await writeFile(join(output, `native-${token}.log`), text, { mode: 0o600, flag: 'wx' });
  receipt.result = result.code === 0 ? 'passed' : 'failed';
} catch (error) {
  receipt.result = 'operator-failed';
  receipt.error = error instanceof Error ? error.message : 'operator failure';
} finally {
  for (const record of children.reverse()) {
    const result = await stop(record);
    for (const name of ['stdout', 'stderr']) {
      await writeFile(join(root, `${record.pid ?? 'spawn-error'}-${name}.log`), Buffer.concat(record[name]), { mode: 0o600, flag: 'wx' });
    }
    receipt.children.push({ executable: record.executable, argv: record.argv, pid: record.pid,
      stdout_bytes: record.stdoutBytes, stderr_bytes: record.stderrBytes, ...result });
  }
  await writeFile(join(output, `native-${token}.json`), JSON.stringify(receipt, null, 2), { mode: 0o600, flag: 'wx' });
}
console.log(JSON.stringify({ result: receipt.result, retained_root: root, receipt: join(output, `native-${token}.json`) }));
if (receipt.result !== 'passed') process.exitCode = 1;
