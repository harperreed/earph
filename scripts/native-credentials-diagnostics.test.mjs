// ABOUTME: Checks the public allowlist boundary for native-keychain diagnostics.
// ABOUTME: Exercises valid stage records and rejects arbitrary private log content.
import test from 'node:test';
import assert from 'node:assert/strict';
import { keychainDiagnostics } from './native-credentials-diagnostics.mjs';

test('retains the last known stage and fixed store classification', () => {
  const log = 'earph-keychain-stage=test-started\nearph-keychain-stage=store-opened\nearph-keychain-error=Uncertain\n';
  assert.deepEqual(keychainDiagnostics(log), { last_keychain_stage: 'store-opened', last_store_error: 'Uncertain' });
});
test('omits unrelated text, unknown values, and forged extra content', () => {
  const log = 'private synthetic data\nearph-keychain-stage=unknown-private-stage\nearph-keychain-error=private-value\nearph-keychain-stage=store-opened private-suffix\n';
  assert.deepEqual(keychainDiagnostics(log), {});
});
test('unknown later records cannot replace safe observations', () => {
  const log = 'earph-keychain-stage=keychain-created\nearph-keychain-error=Locked\nearph-keychain-stage=private-value\n';
  assert.deepEqual(keychainDiagnostics(log), { last_keychain_stage: 'keychain-created', last_store_error: 'Locked' });
});
