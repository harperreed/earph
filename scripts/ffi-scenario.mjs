// ABOUTME: Compares actual foreign binding replies with the real owner and request CLI.
// ABOUTME: Preserves private transcripts, numeric process exits, and immutable artifact hashes.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';

const options = Object.fromEntries(process.argv.slice(2).reduce((pairs, value, index, all) => {
  if (index % 2 === 0) pairs.push([value.replace(/^--/, ''), all[index + 1]]);
  return pairs;
}, []));
for (const field of ['engine', 'library', 'language', 'output']) assert(options[field], `missing --${field}`);
assert(['kotlin', 'swift'].includes(options.language), 'language must be kotlin or swift');
fs.mkdirSync(options.output, { mode: 0o700 });
const output = path.resolve(options.output);
const write = (name, bytes) => fs.writeFileSync(path.join(output, name), bytes, { flag: 'wx', mode: 0o600 });
const save = (name, value) => write(name, JSON.stringify(value, null, 2) + '\n');
const hash = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const engineHash = hash(options.engine), libraryHash = hash(options.library);
const runtime = fs.realpathSync(fs.mkdtempSync('/tmp/effi-'));
fs.chmodSync(runtime, 0o700);
assert(Buffer.byteLength(runtime) <= 60, 'runtime exceeds shared path limit');
save('runtime-private.json', { runtime });
let sequence = 0, ownerExit;
const envelope = (payload, state = null, id = `ffi-driver-${process.pid}-${++sequence}`) => ({
  schema_version: 1, request_id: id, zone_id: 'main', owner_id: state?.owner_id ?? null,
  session_id: state?.session_id ?? null, expected_revision: state?.revision ?? null, payload,
});
function cli(bytes, label) {
  const result = spawnSync(options.engine, ['request', '--runtime', runtime], {
    input: Buffer.concat([bytes, Buffer.from('\n')]), timeout: 8000, maxBuffer: 4 * 1024 * 1024,
  });
  save(`${label}-cli-private.json`, { exit: result.status, signal: result.signal,
    error: result.error?.message ?? null, stdout: result.stdout?.toString(), stderr: result.stderr?.toString() });
  assert([0, 1].includes(result.status), `request CLI failed: ${label}`);
  const reply = result.stdout;
  assert(reply.at(-1) === 10, 'CLI omitted its reply terminator');
  return reply.subarray(0, reply.length - 1);
}
const queryState = label => JSON.parse(cli(Buffer.from(JSON.stringify(envelope({ method: 'state.get' }))), label)).receipt.result.value;
const ownerFiles = ['owner.stdout.log', 'owner.stderr.log'].map(name => fs.openSync(path.join(output, name), 'wx', 0o600));
const owner = spawn(options.engine, ['owner', '--runtime', runtime], { stdio: ['ignore', ...ownerFiles] });
save('owner-command-private.json', { program: options.engine, args: ['owner', '--runtime', runtime], pid: owner.pid });
const exited = new Promise(resolve => {
  owner.once('exit', (code, signal) => { ownerExit = { code, signal }; resolve(ownerExit); });
  owner.once('error', error => { ownerExit = { code: null, signal: null, error: error.message }; resolve(ownerExit); });
});
async function waitExit() {
  let timer;
  try { return await Promise.race([exited, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('owner shutdown deadline exceeded')), 8000); })]); }
  finally { clearTimeout(timer); }
}
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let failure, summary;
try {
  const deadline = Date.now() + 8000;
  while (!fs.existsSync(path.join(runtime, 'endpoint.json'))) {
    assert(!ownerExit, 'owner exited before readiness'); assert(Date.now() < deadline, 'owner readiness deadline exceeded'); await sleep(10);
  }
  const initial = queryState('initial');
  assert.equal(initial.muted, true); assert.equal(initial.output, null); assert.equal(initial.shutdown_requested, false);
  const cases = [];
  const add = (name, command) => cases.push({ name, bytes: Buffer.isBuffer(command) ? command : Buffer.from(JSON.stringify(command)) });
  add('state', envelope({ method: 'state.get' }));
  add('capabilities', envelope({ method: 'capabilities.get' }));
  const append = envelope({ method: 'queue.append', path: 'owned-雪.wav' }, initial, 'ffi-append');
  add('append', append); add('duplicate', append);
  add('stale_revision', envelope({ method: 'queue.append', path: 'other.wav' }, initial));
  add('stale_owner', { ...envelope({ method: 'queue.append', path: 'wrong.wav' }, { ...initial, revision: 1 }), owner_id: 'different-owner' });
  add('unsupported', envelope({ method: 'future.operation' }, { ...initial, revision: 1 }));
  add('malformed', Buffer.from([0xff]));
  add('queue', envelope({ method: 'queue.get', offset: 0, limit: 16 }));
  add('after_dispose', envelope({ method: 'state.get' }));
  const commands = path.join(output, 'commands-private.tsv');
  write('commands-private.tsv', cases.map(c => `${c.name}\t${c.bytes.toString('hex')}\n`).join(''));
  let program, args, env = { ...process.env };
  if (options.language === 'kotlin') {
    for (const name of ['java', 'jar', 'jna', 'jna-native']) assert(options[name], `missing --${name}`);
    const native = JSON.parse(fs.readFileSync(path.join(options['jna-native'], 'dispatcher.json')));
    assert.equal(hash(options.jna), native.jar_sha256);
    const dispatcher = path.resolve(options['jna-native'], native.filename);
    assert.equal(hash(dispatcher), native.dispatcher_sha256);
    const javaTemp = path.join(output, 'java-tmp');
    fs.mkdirSync(javaTemp, { mode: 0o700 });
    program = options.java;
    args = ['--enable-native-access=ALL-UNNAMED', '-Djna.nounpack=true', '-Djna.noclasspath=true', '-Djna.nosys=true',
      `-Djna.boot.library.path=${path.dirname(dispatcher)}`, `-Djna.tmpdir=${javaTemp}`, `-Djava.io.tmpdir=${javaTemp}`,
      `-Djna.library.path=${path.dirname(path.resolve(options.library))}`,
      '-cp', `${options.jar}${path.delimiter}${options.jna}`, 'SmokeKt', runtime, commands, dispatcher];
    save('dispatcher-private.json', { ...native, loaded_path_assertion: dispatcher });
  } else {
    assert(options.program, 'missing --program'); program = options.program; args = [runtime, commands];
    env.DYLD_LIBRARY_PATH = [path.dirname(path.resolve(options.library)), env.DYLD_LIBRARY_PATH].filter(Boolean).join(':');
  }
  const consumer = spawnSync(program, args, { env, timeout: 30000, encoding: 'utf8', maxBuffer: 4 * 1024 * 1024 });
  save('consumer-command-private.json', { program, args, pid: consumer.pid });
  write('consumer.stdout.log', consumer.stdout ?? ''); write('consumer.stderr.log', consumer.stderr ?? '');
  save('consumer-exit.json', { code: consumer.status, signal: consumer.signal, error: consumer.error?.message ?? null });
  assert.equal(consumer.status, 0, 'foreign consumer failed; inspect preserved logs');
  assert.equal(consumer.stderr, '', 'foreign consumer produced unexpected diagnostics');
  const rows = new Map(consumer.stdout.trim().split('\n').map(line => line.split('\t')));
  assert.equal(rows.size, cases.length + (options.language === 'kotlin' ? 5 : 4), 'foreign consumer result count differs');
  if (options.language === 'kotlin') {
    assert.equal(rows.get('private_dispatcher'), 'verified');
    const native = JSON.parse(fs.readFileSync(path.join(options['jna-native'], 'dispatcher.json')));
    assert.equal(hash(path.join(options['jna-native'], native.filename)), native.dispatcher_sha256);
  }
  const results = {};
  for (const c of cases) {
    const hex = rows.get(c.name); assert(hex && /^(?:[a-f0-9]{2})+$/.test(hex), `missing reply: ${c.name}`);
    const actual = Buffer.from(hex, 'hex'); assert(actual.equals(cli(c.bytes, c.name)), `binding changed reply: ${c.name}`);
    results[c.name] = JSON.parse(actual);
  }
  assert.equal(results.append.receipt.status, 'applied'); assert.equal(results.append.receipt.revision, 1);
  assert.deepEqual(results.duplicate, results.append);
  assert.equal(results.stale_revision.receipt.error.code, 'STALE_REVISION');
  assert.equal(results.stale_owner.receipt.error.code, 'STALE_SESSION');
  assert.equal(results.unsupported.error.code, 'CAPABILITY_UNSUPPORTED');
  assert.equal(results.malformed.error.code, 'ASSET_INVALID');
  assert.equal(results.queue.receipt.result.value.entries.length, 1);
  assert.equal(results.queue.receipt.result.value.entries[0].path, 'owned-雪.wav');
  const after = results.after_dispose.receipt.result.value;
  for (const key of ['owner_id', 'muted', 'software_gain_db', 'output', 'output_consented', 'volume_policy', 'shutdown_requested']) {
    assert(Object.hasOwn(initial, key) && Object.hasOwn(after, key), `missing state field: ${key}`);
    assert.deepEqual(after[key], initial[key], key);
  }
  for (const name of ['preflight_empty', 'preflight_oversized', 'path_oversized']) assert.equal(rows.get(name), '!RESOURCE_BUDGET_EXCEEDED');
  assert.equal(rows.get('path_empty'), '!ASSET_INVALID');
  save('literal-and-cli-results-private.json', results);
  summary = { schema_version: 1, status: 'pass', language: options.language, actual_foreign_commands: cases.length,
    exact_cli_comparisons: cases.length, typed_local_failures: 4, owner_survives_handle_release: true,
    reported_safety_unchanged: true, native_output_opened: false, signal_exercises: 0, cancellation_qualified: false,
    subscriptions_qualified: false, private_jna_dispatcher_verified: options.language === 'kotlin',
    engine_sha256: engineHash, library_sha256: libraryHash };
} catch (error) { failure = error; }
finally {
  if (!ownerExit) {
    try {
      const state = queryState('cleanup-state');
      const reply = JSON.parse(cli(Buffer.from(JSON.stringify(envelope({ method: 'owner.shutdown' }, state))), 'shutdown'));
      assert.equal(reply.receipt.status, 'applied');
      await waitExit();
    } catch (error) {
      failure ??= error; owner.kill('SIGTERM');
      try { await waitExit(); } catch (last) { failure ??= last; }
    }
  }
  for (const fd of ownerFiles) fs.closeSync(fd);
  save('owner-exit.json', ownerExit ?? { code: null, signal: null });
  try {
    assert.equal(ownerExit?.code, 0, 'owner did not exit normally');
    assert.equal(hash(options.engine), engineHash, 'engine changed during execution');
    assert.equal(hash(options.library), libraryHash, 'FFI library changed during execution');
  } catch (error) { failure ??= error; }
  save('summary.json', { ...summary, status: failure ? 'fail' : 'pass', owner_exit_code: ownerExit?.code ?? null,
    owner_signal: ownerExit?.signal ?? null, error: failure?.message ?? null });
}
if (failure) throw failure;
console.log(JSON.stringify(summary));
