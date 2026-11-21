// ABOUTME: Checks the bounded owner startup parser with real retained log files.
// ABOUTME: Covers incomplete records, owner identity, malformed records, and read bounds.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from 'node:test';
import { readOwnerAnnouncement } from './ffi-owner-readiness.mjs';

function log(bytes) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'earph-ffi-ready-'));
  const filename = path.join(directory, 'owner.stdout.log');
  fs.writeFileSync(filename, bytes, { flag: 'wx', mode: 0o600 });
  return filename;
}

test('waits for one complete startup record before exposing the announced owner', () => {
  const filename = log('');
  assert.equal(readOwnerAnnouncement(filename), null);
  fs.appendFileSync(filename, JSON.stringify({ status: 'owner', owner_id: 'observed-owner' }));
  assert.equal(readOwnerAnnouncement(filename), null);
  fs.appendFileSync(filename, '\n');
  assert.equal(readOwnerAnnouncement(filename), 'observed-owner');
});

test('rejects a complete startup record without the production status and owner identity', () => {
  for (const value of [{ status: 'other', owner_id: 'observed-owner' }, { status: 'owner' }, { status: 'owner', owner_id: '' }]) {
    assert.throws(() => readOwnerAnnouncement(log(JSON.stringify(value) + '\n')), /startup record/);
  }
  assert.throws(() => readOwnerAnnouncement(log('{broken}\n')), SyntaxError);
});

test('rejects an unfinished startup record above the retained prefix budget', () => {
  assert.throws(() => readOwnerAnnouncement(log('x'.repeat(8193))), /startup record exceeds/);
});

test('reads only the first complete record even when later output exceeds the prefix budget', () => {
  const filename = log(JSON.stringify({ status: 'owner', owner_id: 'first-owner' }) + '\n' + 'x'.repeat(16384));
  assert.equal(readOwnerAnnouncement(filename), 'first-owner');
});
