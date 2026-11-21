// ABOUTME: Extracts one verified JNA dispatcher into a new private artifact directory.
// ABOUTME: Retains exact bytes so child JVMs can disable automatic unpacking and cleanup.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
const [jar, output] = process.argv.slice(2);
assert(jar && output, 'usage: node scripts/prepare-jna.mjs VERIFIED_JAR NEW_DIRECTORY');
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const jarHash = sha(fs.readFileSync(jar));
assert.equal(jarHash, '4fb141dd8ef6b0585ffceea4bc49602fbc6312fa977e2c488794ea3e6aafecae', 'JNA must match the verified 5.19.1 JAR');
const entries = {
  'linux-x64': 'com/sun/jna/linux-x86-64/libjnidispatch.so',
  'darwin-arm64': 'com/sun/jna/darwin-aarch64/libjnidispatch.jnilib',
  'darwin-x64': 'com/sun/jna/darwin-x86-64/libjnidispatch.jnilib',
};
const entry = entries[`${process.platform}-${process.arch}`];
assert(entry, 'JNA dispatcher is unqualified for this host; no fallback is allowed');
const bytes = execFileSync('unzip', ['-p', jar, entry], { maxBuffer: 8 * 1024 * 1024 });
assert(bytes.length > 0, 'verified JAR omitted its selected dispatcher');
fs.mkdirSync(output, { mode: 0o700 });
const filename = path.basename(entry);
const destination = path.join(output, filename);
fs.writeFileSync(destination, bytes, { flag: 'wx', mode: 0o500 });
assert(fs.readFileSync(destination).equals(bytes), 'retained dispatcher differs from verified JAR bytes');
const record = { schema_version: 1, jna_version: '5.19.1', jar_sha256: jarHash, entry, filename,
  dispatcher_bytes: bytes.length, dispatcher_sha256: sha(bytes), automatic_unpacking_allowed: false,
  classpath_fallback_allowed: false, system_fallback_allowed: false };
fs.writeFileSync(path.join(output, 'dispatcher.json'), JSON.stringify(record, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify(record));
