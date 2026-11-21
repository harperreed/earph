#!/usr/bin/env node
// ABOUTME: Emits only allowlisted hosted credential qualification facts.
// ABOUTME: Binds actual Cargo artifacts while keeping private logs and keychains on the runner.
import { readFile, realpath, lstat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { isAbsolute, join, relative, sep } from 'node:path';
import { keychainDiagnostics } from './native-credentials-diagnostics.mjs';

const operatorSha = '5b84299d2aac9d3b6c065cd48aec918d3f3803da03f900c2bac8dd990458ceb9';
const names = ['cargo-version', 'clippy', 'ordinary-tests', 'native-build', 'isolated-keychain'];
let stage = 'input';
try {
  const [log, exit] = process.argv.slice(2);
  const operatorExit = Number(exit);
  if (process.argv.length !== 4 || !isAbsolute(log ?? '') || !/^(0|[1-9][0-9]{0,2})$/.test(exit ?? '') || operatorExit > 255) throw 0;
  if (process.platform !== 'darwin' || process.env.GITHUB_ACTIONS !== 'true' || !isAbsolute(process.env.RUNNER_TEMP ?? '')) throw 0;
  const runner = await realpath(process.env.RUNNER_TEMP);
  const within = (parent, path) => { const part = relative(parent, path); return part && part !== '..' && !part.startsWith('..' + sep) && !isAbsolute(part); };
  const privateFile = async (path, parent, limit) => {
    const resolved = await realpath(path);
    const meta = await lstat(path);
    if (!within(parent, resolved) || !meta.isFile() || meta.uid !== process.getuid() || (meta.mode & 0o077) !== 0 || meta.size > limit) throw 0;
    return readFile(resolved);
  };
  const lines = (await privateFile(log, runner, 1024 * 1024)).toString().trim().split('\n');
  const summary = JSON.parse(lines.at(-1));
  stage = 'receipt';
  const root = await realpath(summary.retained_root);
  const rootMeta = await lstat(root);
  if (!within(runner, root) || !rootMeta.isDirectory() || rootMeta.uid !== process.getuid() || (rootMeta.mode & 0o077) !== 0) throw 0;
  const receipt = JSON.parse(await privateFile(join(root, 'receipt.json'), root, 1024 * 1024));
  if (receipt.schema_version !== 1 || receipt.root !== summary.retained_root || !['passed', 'failed'].includes(receipt.result) || summary.result !== receipt.result) throw 0;
  const operations = receipt.operations;
  if (!Array.isArray(operations) || operations.length > names.length) throw 0;
  const exits = operations.map((op, index) => {
    if (op.name !== names[index] || !(op.code === null || Number.isInteger(op.code) && op.code >= 0 && op.code <= 255) || ![null, 'SIGTERM', 'SIGKILL', 'spawn-error'].includes(op.signal)) throw 0;
    return { name: names[index], code: op.code, signal: op.signal };
  });
  const output = { schema_version: 1, result: receipt.result, operator_exit: operatorExit, operator_sha256: operatorSha, operation_count: exits.length, operations: exits, process_observation: 'exit-event', keychain_retention: 'ephemeral-runner-only' };
  if (receipt.result === 'passed' && (operatorExit !== 0 || exits.length !== names.length || exits.some(op => op.code !== 0 || op.signal !== null))) throw 0;
  if (operations.find(op => op.name === 'native-build')?.code === 0 && receipt.helper_sha256 && receipt.test_sha256) {
    stage = 'artifact-binding';
    const build = (await privateFile(join(root, 'native-build.log'), root, 1024 * 1024)).toString().split('\n').filter(line => line.startsWith('{')).map(line => JSON.parse(line));
    const artifact = (name, kind) => {
      const paths = [...new Set(build.filter(value => value.reason === 'compiler-artifact' && value.target?.name === name && value.target.kind?.includes(kind) && typeof value.executable === 'string').map(value => value.executable))];
      if (paths.length !== 1) throw 0;
      return paths[0];
    };
    const hashArtifact = async (path) => {
      const resolved = await realpath(path);
      const meta = await lstat(path);
      if (!within(join(root, 'target'), resolved) || !meta.isFile() || meta.uid !== process.getuid() || (meta.mode & 0o002) !== 0 || meta.size > 128 * 1024 * 1024) throw 0;
      return createHash('sha256').update(await readFile(path)).digest('hex');
    };
    const helper = await hashArtifact(artifact('earph-credential-helper', 'bin'));
    const test = await hashArtifact(artifact('native_macos', 'test'));
    if (helper !== receipt.helper_sha256 || test !== receipt.test_sha256) throw 0;
    if (operations.some(op => op.name === 'isolated-keychain')) {
      const native = (await privateFile(join(root, 'isolated-keychain.log'), root, 1024 * 1024)).toString();
      Object.assign(output, keychainDiagnostics(native));
      const counts = /^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;/m.exec(native);
      if (counts) output.native_test_counts = { passed: Number(counts[2]), failed: Number(counts[3]), ignored: Number(counts[4]) };
    }
    if (receipt.result === 'passed' && JSON.stringify(output.native_test_counts) !== JSON.stringify({ passed: 1, failed: 0, ignored: 0 })) throw 0;
    output.helper_sha256 = helper;
    output.test_sha256 = test;
    output.artifact_binding = 'cargo-json-paths-rehashed';
  }
  if (receipt.result === 'passed' && output.artifact_binding !== 'cargo-json-paths-rehashed') throw 0;
  console.log(JSON.stringify(output));
  if (receipt.result !== 'passed') process.exitCode = 1;
} catch {
  console.log(JSON.stringify({ schema_version: 1, result: 'failed', safe_report_stage: stage, operator_sha256: operatorSha }));
  process.exitCode = 1;
}
