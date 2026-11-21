// ABOUTME: Selects fixed native-keychain stage and error labels for safe CI reports.
// ABOUTME: Keeps arbitrary log text, paths, credentials, and unknown values out of output.
const stages = new Set(['test-started', 'root-validated', 'interaction-disabled', 'keychain-created', 'store-opened', 'missing-confirmed', 'first-commit', 'second-commit', 'readback-confirmed', 'native-item-opened', 'corrupt-written', 'corruption-refused', 'keychain-locked', 'locked-refused']);
const errors = new Set(['Missing', 'Locked', 'Unavailable', 'Ambiguous', 'InvalidDocument', 'Uncertain', 'Busy', 'Cancelled']);
export function keychainDiagnostics(log) {
  const result = {};
  for (const line of log.split('\n')) {
    const record = /^earph-keychain-(stage|error)=([A-Za-z-]+)$/.exec(line);
    if (record?.[1] === 'stage' && stages.has(record[2])) result.last_keychain_stage = record[2];
    if (record?.[1] === 'error' && errors.has(record[2])) result.last_store_error = record[2];
  }
  return result;
}
