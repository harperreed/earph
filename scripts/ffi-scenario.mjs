// ABOUTME: Compares actual foreign binding replies with the real owner and request CLI.
// ABOUTME: Preserves private transcripts, numeric process exits, and immutable artifact hashes.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import http from 'node:http';
import os from 'node:os';
import { spawn, spawnSync } from 'node:child_process';
import { readOwnerAnnouncement } from './ffi-owner-readiness.mjs';

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
function foreign(cases, label) {
  const directory = path.join(output, `foreign-${label}`);
  fs.mkdirSync(directory, { mode: 0o700 });
  const write = (name, bytes) => fs.writeFileSync(path.join(directory, name), bytes, { flag: 'wx', mode: 0o600 });
  const save = (name, value) => write(name, JSON.stringify(value, null, 2) + '\n');
  const commands = path.join(directory, 'commands-private.tsv');
  write('commands-private.tsv', cases.map(c => `${c.name}\t${c.bytes.toString('hex')}\n`).join(''));
  let program, args, env = { ...process.env };
  if (options.language === 'kotlin') {
    for (const name of ['java', 'jar', 'jna', 'jna-native']) assert(options[name], `missing --${name}`);
    const native = JSON.parse(fs.readFileSync(path.join(options['jna-native'], 'dispatcher.json')));
    assert.equal(hash(options.jna), native.jar_sha256);
    const dispatcher = path.resolve(options['jna-native'], native.filename);
    assert.equal(hash(dispatcher), native.dispatcher_sha256);
    const javaTemp = path.join(directory, 'java-tmp');
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
  const lines = consumer.stdout.trim().split('\n').map(line => line.split('\t'));
  const rows = new Map(lines);
  assert.equal(rows.size, lines.length, 'foreign consumer repeated a result name');
  assert.equal(rows.size, cases.length + (options.language === 'kotlin' ? 5 : 4), 'foreign consumer result count differs');
  if (options.language === 'kotlin') {
    assert.equal(rows.get('private_dispatcher'), 'verified');
    const native = JSON.parse(fs.readFileSync(path.join(options['jna-native'], 'dispatcher.json')));
    assert.equal(hash(path.join(options['jna-native'], native.filename)), native.dispatcher_sha256);
  }
  for (const name of ['preflight_empty', 'preflight_oversized', 'path_oversized']) assert.equal(rows.get(name), '!RESOURCE_BUDGET_EXCEEDED');
  assert.equal(rows.get('path_empty'), '!ASSET_INVALID');
  return rows;
}
function replyBytes(rows, name) {
  const hex = rows.get(name);
  assert(hex && /^(?:[a-f0-9]{2})+$/.test(hex), `missing reply: ${name}`);
  return Buffer.from(hex, 'hex');
}
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
  const startupStarted = Date.now();
  const deadline = Date.now() + 8000;
  let announcedOwner = null;
  while (announcedOwner === null) {
    assert(!ownerExit, 'owner exited before readiness'); assert(Date.now() < deadline, 'owner readiness deadline exceeded'); await sleep(10);
    announcedOwner = readOwnerAnnouncement(path.join(output, 'owner.stdout.log'));
  }
  const initial = queryState('initial');
  assert.equal(initial.owner_id, announcedOwner, 'State belongs to a different owner than the startup announcement');
  save('owner-readiness-private.json', { announced_owner_id: announcedOwner, observed_owner_id: initial.owner_id,
    elapsed_ms: Date.now() - startupStarted, startup_deadline_ms: 8000, endpoint_alone_is_not_readiness: true });
  assert.equal(initial.muted, true); assert.equal(initial.output, null); assert.equal(initial.shutdown_requested, false);
  assert.equal(initial.software_gain_zero, false, 'owner must report its exact-zero setting');
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
  const rows = foreign(cases, 'baseline');
  const results = {};
  for (const c of cases) {
    const actual = replyBytes(rows, c.name); assert(actual.equals(cli(c.bytes, c.name)), `binding changed reply: ${c.name}`);
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
  for (const key of ['owner_id', 'muted', 'software_gain_db', 'software_gain_zero', 'output', 'output_consented', 'volume_policy', 'shutdown_requested']) {
    assert(Object.hasOwn(initial, key) && Object.hasOwn(after, key), `missing state field: ${key}`);
    assert.deepEqual(after[key], initial[key], key);
  }
  save('literal-and-cli-results-private.json', results);
  for (const key of ['local_volume_policy', 'configured_profile']) assert(Object.hasOwn(initial, key), `missing integrated state: ${key}`);
  assert.equal(initial.configured_profile, null);
  const capabilities = results.capabilities.receipt.result.value.profiles;
  for (const key of ['exact_association_backends', 'application', 'package']) assert(Object.hasOwn(capabilities, key), `missing integrated capability: ${key}`);
  assert.equal(capabilities.package.format, 'directory');
  assert.equal(capabilities.package.automatic_privacy_proof, false);
  const advertisedActivity = results.capabilities.receipt.result.value.output_activity;
  assert(advertisedActivity, 'missing output activity capability');
  assert.equal(advertisedActivity.scope, 'output_instance_across_native_reopens');
  assert.equal(advertisedActivity.measurement_boundary, 'core_audio_sdk_entry_calls');
  assert.deepEqual(advertisedActivity.backends, process.platform === 'darwin' ? ['core_audio'] : []);
  const phases = [];
  let expectedForeignOutput = null;
  const readForeign = (bytes, label) => {
    const rows = foreign([{ name: 'request', bytes }, {
      name: 'after_dispose', bytes: Buffer.from(JSON.stringify(envelope({ method: 'state.get' }))),
    }], label);
    const after = JSON.parse(replyBytes(rows, 'after_dispose')).receipt.result.value;
    assert.equal(after.owner_id, initial.owner_id);
    assert.equal(after.muted, true); assert.deepEqual(after.output, expectedForeignOutput); assert.equal(after.output_consented, false);
    return replyBytes(rows, 'request');
  };
  const controlCall = (domain, name, payload, state = null, expected = {}) => {
    const bytes = Buffer.isBuffer(payload) ? payload : Buffer.from(JSON.stringify(envelope(payload, state)));
    const reply = readForeign(bytes, `${domain}-${name}`);
    assert(reply.equals(cli(bytes, `${domain}-${name}`)), `${domain} binding changed reply: ${name}`);
    const document = JSON.parse(reply);
    assert.equal(document.receipt.status, expected.status ?? 'applied', name);
    if (expected.code) assert.equal(document.receipt.error?.code, expected.code, name);
    return { bytes, reply, value: document.receipt.result?.value };
  };
  async function phase(name, payload, query = false, status = 'applied') {
    const state = query ? null : queryState(`${name}-fresh-state`);
    const command = envelope(payload, state);
    const bytes = Buffer.from(JSON.stringify(command));
    const firstBytes = readForeign(bytes, `${name}-first`), first = JSON.parse(firstBytes);
    assert.equal(first.receipt.request_id, command.request_id);
    assert.equal(first.receipt.zone_id, command.zone_id);
    assert.equal(first.receipt.session_id, command.session_id);
    const deadline = Date.now() + 8000;
    let polls = 0, terminalBytes, terminal;
    do {
      assert(Date.now() < deadline, `terminal receipt deadline exceeded: ${name}`);
      terminalBytes = cli(bytes, `${name}-poll-${++polls}`);
      terminal = JSON.parse(terminalBytes);
      if (terminal.receipt.status === 'accepted') await sleep(5);
    } while (terminal.receipt.status === 'accepted');
    assert.equal(terminal.receipt.status, status, name);
    // The first Accepted frame can differ from its later terminal replay.
    if (first.receipt.status !== 'accepted') assert(firstBytes.equals(terminalBytes), `first terminal differs: ${name}`);
    const replay = readForeign(bytes, `${name}-terminal-replay`);
    assert(replay.equals(terminalBytes), `foreign terminal differs from CLI: ${name}`);
    assert(replay.equals(cli(bytes, `${name}-stable-cli-replay`)), `terminal replay changed: ${name}`);
    phases.push({ name, first_status: first.receipt.status, terminal_status: terminal.receipt.status,
      cli_polls: polls, exact_terminal_bytes: true, request_sha256: crypto.createHash('sha256').update(bytes).digest('hex') });
    save(`${name}-phase-private.json`, phases.at(-1));
    return terminal.receipt.result?.value;
  }
  const port = { domain: 'pcm', sample_rate_hz: 48000, channels: 'stereo', precision: 'signed16', max_quantum_frames: 64 };
  const graph = { graph: { schema_version: 1, output: 'out', nodes: [
    { id: 'in', kind: { kind: 'input' }, format: port }, { id: 'out', kind: { kind: 'output' }, format: port },
  ], edges: [{ from: 'in', to: 'out', from_port: 0, to_port: 0 }] }, processing: { kind: 'integer_bypass' } };
  const draft = { name: 'Generated foreign profile', source_policy: 'local_file', fidelity: 'compatible_conversion',
    output: { backend: 'alsa', route: 'system', device_id: 'ffi-generated-output', manual_setup: 'Generated saved intent' },
    volume: { mode: 'software', software_cap_millidecibels: -20000, hardware_cap_millidecibels: null },
    latency: { mode: 'balanced', max_resident_bytes: 268435456, max_preparation_bytes: 268435456, max_latency_frames: null }, corrections: [] };
  const saved = await phase('profile-create', { method: 'profiles.create', id: 'ffi-generated', draft, graph });
  assert.equal(saved.profile.id, 'ffi-generated'); assert.equal(saved.profile.revision, 1);
  const reference = { id: saved.profile.id, revision: saved.profile.revision, profile_sha256: saved.profile_sha256 };
  assert.deepEqual(await phase('profile-get', { method: 'profiles.get', id: reference.id }, true), saved);
  const page = await phase('profile-list', { method: 'profiles.list', offset: 0, limit: 16 }, true);
  assert.equal(page.entries.length, 1); assert.equal(page.entries[0].profile_sha256, reference.profile_sha256);
  const resolution = await phase('profile-resolve', { method: 'profiles.resolve', manual_profile_id: reference.id, output: draft.output }, true);
  assert.equal(resolution.status, 'selected'); assert.equal(resolution.profile.id, reference.id); assert.equal(resolution.reason, 'manual');
  const preview = await phase('package-preview', { method: 'profiles.package_preview', profile: reference });
  assert.equal(preview.operation, 'previewed'); assert.equal(preview.manifest.objects.length, 2);
  const directory = path.join(output, 'generated-package');
  const exportPayload = { method: 'profiles.export', directory, profile: reference, expected_manifest_sha256: preview.manifest_sha256,
    export_review: 'reviewed_exact_candidate', content_review: 'reviewed_no_private_data', redistribution: 'refuse_measurement_bytes' };
  const unreviewed = { ...exportPayload }; delete unreviewed.content_review;
  // The parser refuses a missing explicit privacy review before any publication.
  const refusedBytes = Buffer.from(JSON.stringify(envelope(unreviewed, queryState('export-review-state'))));
  const refused = readForeign(refusedBytes, 'package-missing-review');
  assert.equal(JSON.parse(refused).error.code, 'ASSET_INVALID');
  assert(refused.equals(cli(refusedBytes, 'package-missing-review')));
  assert(!fs.existsSync(directory), 'unreviewed export published a directory');
  const exported = await phase('package-export', exportPayload);
  assert.equal(exported.operation, 'exported');
  const inspected = await phase('package-inspect', { method: 'profiles.package_inspect', directory });
  assert.deepEqual(inspected.manifest, preview.manifest);
  assert.equal(inspected.manifest_sha256, preview.manifest_sha256);
  for (const object of inspected.manifest.objects) {
    const suffix = object.kind === 'profile' ? 'profile.json' : 'graph.json';
    assert(['profile', 'graph'].includes(object.kind), 'unexpected generated package object');
    const name = `${object.sha256}.${suffix}`;
    assert.equal(hash(path.join(directory, name)), object.sha256);
    assert(fs.readFileSync(path.join(directory, name)).equals(fs.readFileSync(path.join(runtime, 'profiles', name))));
  }
  await phase('package-import-collision', { method: 'profiles.import_package', directory, profile: reference,
    expected_manifest_sha256: inspected.manifest_sha256 }, false, 'rejected');
  const missing = await phase('package-inspect-missing', { method: 'profiles.package_inspect', directory: path.join(output, 'absent-package') }, false, 'failed');
  for (const key of ['reason', 'object', 'filesystem_state']) assert(Object.hasOwn(missing, key), `missing package failure: ${key}`);
  const networkAddress = Object.values(os.networkInterfaces()).flat().find(address => {
    if (address.family !== 'IPv4' || address.internal) return false;
    const [first, second] = address.address.split('.').map(Number);
    return first === 10 || (first === 172 && second >= 16 && second <= 31) || (first === 192 && second === 168);
  })?.address;
  assert(networkAddress, 'network control fixture requires an owned private IPv4 interface');
  let networkConnections = 0, networkRequests = 0, networkCommands = 0, rendererCommands = 0;
  const fixture = http.createServer((_request, response) => { networkRequests++; response.writeHead(500).end(); });
  fixture.on('connection', socket => { networkConnections++; socket.destroy(); });
  try {
    await new Promise((resolve, reject) => { fixture.once('error', reject); fixture.listen(0, networkAddress, resolve); });
    const origin = `http://${networkAddress}:${fixture.address().port}/`;
    const networkCall = (...args) => { const result = controlCall('network', ...args); networkCommands++; return result; };
    const rendererCall = (...args) => { const result = controlCall('renderer', ...args); rendererCommands++; return result; };
    const firstNetwork = networkCall('initial', { method: 'sources.network.get' }).value;
    assert.equal(firstNetwork.enabled, true); assert.equal(firstNetwork.grant_generation, 0); assert.equal(firstNetwork.origin_count, 0);
    assert.equal(firstNetwork.active, null); assert.equal(firstNetwork.lifecycle, null);
    const grant = networkCall('grant', { method: 'sources.grants.replace', expected_generation: 0,
      grants: { origins: [{ origin, pins: [networkAddress], roots_der: [] }] }, lifetime_ms: 60000 }, queryState('network-grant-state'));
    const granted = networkCall('granted', { method: 'sources.network.get' }).value;
    assert.equal(granted.grant_generation, 1); assert.equal(granted.origin_count, 1);
    assert(Number.isSafeInteger(granted.expires_monotonic_ns) && granted.expires_monotonic_ns > 0);
    const privateMarker = 'generated-foreign-private-query';
    networkCall('stage', { method: 'sources.stage_uri', uri: `${origin}owned.flac?proof=${privateMarker}`,
      metadata: 'Generated foreign controller metadata' }, queryState('network-stage-state'));
    const staged = networkCall('sources', { method: 'sources.get' }).value;
    assert.equal(staged.staged.admission, 'unadmitted');
    assert(!JSON.stringify(staged).includes(privateMarker), 'foreign source snapshot leaked the URI query');
    networkCall('revoke', { method: 'sources.grants.revoke', expected_generation: 1 }, queryState('network-revoke-state'));
    const revoked = networkCall('revoked', { method: 'sources.network.get' }).value;
    assert.equal(revoked.grant_generation, 2); assert.equal(revoked.origin_count, 0); assert.equal(revoked.expires_monotonic_ns, null);
    const replay = networkCall('grant-replay', grant.bytes);
    assert(replay.reply.equals(grant.reply), 'grant replay changed its original receipt');
    const replayed = networkCall('replayed', { method: 'sources.network.get' }).value;
    assert.equal(replayed.grant_generation, 2); assert.equal(replayed.origin_count, 0); assert.equal(replayed.expires_monotonic_ns, null);
    const rendererState = queryState('renderer-before');
    const renderer = rendererCall('initial', { method: 'renderer.get' }).value;
    assert.equal(renderer.enabled, false); assert.equal(renderer.grant_generation, 0); assert.equal(renderer.controller_count, 0);
    const unsupported = { status: 'rejected', code: 'CAPABILITY_UNSUPPORTED' };
    const deniedGrant = rendererCall('grant', { method: 'renderer.grants.replace', expected_generation: 0, lifetime_ms: 60000,
      grants: { controllers: [{ id: 'foreign-controller', peers: [networkAddress], playback: null, rights: ['stage'] }] } }, rendererState, unsupported);
    rendererCall('revoke', { method: 'renderer.grants.revoke', expected_generation: 0,
      controller_id: 'foreign-controller' }, queryState('renderer-revoke-state'), unsupported);
    rendererCall('untrusted', { method: 'renderer.request', controller_id: 'foreign-controller', grant_generation: 1,
      action: { operation: 'stop', binding: null } }, queryState('renderer-untrusted-state'), { status: 'rejected', code: 'AUTH_DENIED' });
    assert(rendererCall('grant-replay', deniedGrant.bytes, null, unsupported).reply.equals(deniedGrant.reply));
    const rendererAfter = rendererCall('after', { method: 'renderer.get' }).value;
    for (const key of ['enabled', 'grant_generation', 'controller_count', 'controllers', 'staged', 'active', 'closing']) {
      assert.deepEqual(rendererAfter[key], renderer[key], `disabled renderer changed authority: ${key}`);
    }
    assert.deepEqual(queryState('renderer-after-state'), rendererState, 'disabled renderer changed the actual owner state');
    save('renderer-control-private.json', { commands: rendererCommands, exact_cli_comparisons: rendererCommands,
      disabled: true, grant_and_revoke_refused: true, untrusted_ingress_denied: true,
      exact_rejected_grant_replay: true, owner_state_unchanged: true });
    await new Promise(setImmediate);
    assert.equal(networkConnections, 0, 'control-only foreign calls connected to the real HTTP fixture');
    assert.equal(networkRequests, 0, 'control-only foreign calls fetched from the real HTTP fixture');
    save('network-control-private.json', { origin, commands: networkCommands, exact_cli_comparisons: networkCommands,
      grant_request_sha256: crypto.createHash('sha256').update(grant.bytes).digest('hex'),
      exact_grant_replay: true, final_grant_generation: replayed.grant_generation, final_origin_count: replayed.origin_count,
      final_lease_expiry: replayed.expires_monotonic_ns, staged_uri_redacted: true, fixture_connections: networkConnections,
      fixture_requests: networkRequests });
  } finally {
    if (fixture.listening) await new Promise((resolve, reject) => fixture.close(error => error ? reject(error) : resolve()));
  }
  const finalState = queryState('integrated-final');
  assert.equal(finalState.source, null); assert.equal(finalState.transport, 'idle');
  for (const key of ['owner_id', 'muted', 'software_gain_db', 'software_gain_zero', 'output', 'output_consented', 'volume_policy', 'local_volume_policy', 'configured_profile', 'graph_epoch', 'shutdown_requested']) {
    assert(Object.hasOwn(initial, key) && Object.hasOwn(finalState, key), `missing final state field: ${key}`);
    assert.deepEqual(finalState[key], initial[key], `saved intent changed output authority: ${key}`);
  }
  let volumeCommands = 0;
  const volumeCall = (...args) => { const result = controlCall('volume', ...args); volumeCommands++; return result; };
  expectedForeignOutput = { backend: process.platform === 'darwin' ? 'core_audio' : 'alsa', route: 'system',
    endpoint: 'unopened-foreign-volume-fixture', device_id: 'foreign-volume-device', profile_id: 'foreign-volume-profile',
    period_frames: 64, buffer_frames: 256, volume_mode: 'software', fidelity: 'compatible_conversion' };
  volumeCall('output', { method: 'output.select', output: expectedForeignOutput }, finalState);
  const beforeZero = queryState('volume-before-zero');
  const zero = volumeCall('zero', { method: 'volume.silence' }, beforeZero);
  const silent = queryState('volume-silent-state');
  assert.equal(silent.software_gain_zero, true); assert.equal(silent.software_gain_db, beforeZero.software_gain_db);
  assert.equal(silent.muted, beforeZero.muted); assert.equal(silent.gain_ramp_frames, 0);
  const pathReport = volumeCall('signal-path', { method: 'signal_path.get' }).value.report;
  const eventReport = volumeCall('events', { method: 'events.get', cursor: null }).value.report;
  assert.equal(pathReport.requested.software_gain_zero, true);
  assert.equal(eventReport.snapshot.software_gain_zero, true);
  volumeCall('finite', { method: 'volume.set', millidecibels: -60000, ramp_frames: 4 }, silent);
  const finite = queryState('volume-finite-state');
  assert.equal(finite.software_gain_zero, false); assert.equal(finite.software_gain_db, -60);
  assert.equal(finite.muted, beforeZero.muted); assert.equal(finite.gain_ramp_frames, 4);
  assert(volumeCall('zero-replay', zero.bytes).reply.equals(zero.reply));
  assert.deepEqual(queryState('volume-after-replay-state'), finite);
  volumeCall('cap-refusal', { method: 'volume.set', millidecibels: 1, ramp_frames: 0 }, finite,
    { status: 'rejected', code: 'SAFETY_POLICY_BLOCKED' });
  assert.deepEqual(queryState('volume-after-refusal-state'), finite);
  volumeCall('preset', { method: 'volume.factory_defaults' }, finite);
  const preset = queryState('volume-preset-state');
  assert.equal(preset.software_gain_zero, false); assert.equal(preset.software_gain_db, -40);
  assert.equal(preset.muted, true); assert.equal(preset.gain_ramp_frames, 0);
  for (const key of ['owner_id', 'source', 'transport', 'epochs', 'output', 'output_consented', 'volume_policy', 'local_volume_policy', 'graph_epoch']) {
    assert.deepEqual(preset[key], beforeZero[key], `volume changed unrelated state: ${key}`);
  }
  save('volume-control-private.json', { commands: volumeCommands, exact_cli_comparisons: volumeCommands,
    zero_preserves_mute: true, finite_exit: true, exact_zero_replay: true, cap_refusal_atomic: true,
    diagnostic_zero_shared: true, factory_defaults: true, output_consented: false, source_started: false });
  save('integrated-phases-private.json', phases);
  summary = { schema_version: 1, status: 'pass', language: options.language,
    baseline_foreign_commands: cases.length, actual_foreign_commands: cases.length + phases.length * 4 + 2 + (networkCommands + rendererCommands + volumeCommands) * 2,
    exact_cli_comparisons: cases.length + phases.length * 2 + 1 + networkCommands + rendererCommands + volumeCommands, typed_local_failures: 4, owner_survives_handle_release: true,
    integrated_profile_commands: phases.length, exact_terminal_replay_comparisons: phases.length,
    integrated_foreign_frames: phases.length * 4 + 2, accepted_initial_profile_receipts: phases.filter(p => p.first_status === 'accepted').length,
    generated_package_object_hashes_verified: inspected.manifest.objects.length,
    generated_network_control_qualified: true, generated_network_commands: networkCommands,
    generated_network_foreign_frames: networkCommands * 2, generated_network_exact_cli_comparisons: networkCommands,
    network_grant_replay_exact: true, network_lease_revoked_after_replay: true, network_staged_uri_redacted: true,
    network_fixture_connections: networkConnections, network_fixture_requests: networkRequests,
    generated_renderer_disabled_control_qualified: true, generated_renderer_commands: rendererCommands,
    generated_renderer_foreign_frames: rendererCommands * 2, generated_renderer_exact_cli_comparisons: rendererCommands,
    renderer_grants_refused: true, renderer_untrusted_ingress_denied: true, renderer_rejected_grant_replay_exact: true,
    missing_export_review_refused: true, successful_foreign_package_import_qualified: false,
    generated_volume_commands: volumeCommands, generated_volume_foreign_frames: volumeCommands * 2,
    generated_volume_exact_cli_comparisons: volumeCommands, profile_and_network_safety_unchanged: true,
    gain_fixture_output_consented: false, native_output_opened: false, signal_exercises: 0, cancellation_qualified: false,
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
