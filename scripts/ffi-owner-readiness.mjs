// ABOUTME: Reads the bounded startup announcement from one owned Engine log.
// ABOUTME: Distinguishes an incomplete record from a valid announced owner identity.
import fs from 'node:fs';
import assert from 'node:assert/strict';

export function readOwnerAnnouncement(filename) {
  const bytes = Buffer.alloc(8193);
  const fd = fs.openSync(filename, 'r');
  let length;
  try { length = fs.readSync(fd, bytes, 0, bytes.length, 0); }
  finally { fs.closeSync(fd); }
  const prefix = bytes.subarray(0, length);
  const newline = prefix.indexOf(10);
  if (newline < 0) {
    assert(length <= 8192, 'owner startup record exceeds retained prefix budget');
    return null;
  }
  const value = JSON.parse(prefix.subarray(0, newline).toString('utf8'));
  assert.equal(value.status, 'owner', 'owner startup record has an unexpected status');
  assert(typeof value.owner_id === 'string' && value.owner_id.length > 0, 'owner startup record lacks identity');
  return value.owner_id;
}
