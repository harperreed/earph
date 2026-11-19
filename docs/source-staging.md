# URI metadata staging

URI staging holds one untrusted descriptor in the existing local owner.
It does not fetch, decode, prepare output, or advertise a playable network source.
Existing command fields, schema version 1, playback snapshots, and volume policy keep their meanings.

Use `sources.stage_uri` through the same JSON command envelope as other local controls.
Supply the current owner identity and expected revision.
If the request names a session, that session must still be current.
Use a fresh request ID for each intended mutation.
Reuse the exact envelope only to reconcile an uncertain result.

```json
{
  "schema_version": 1,
  "request_id": "stage-example",
  "zone_id": "main",
  "owner_id": "current-owner",
  "session_id": null,
  "expected_revision": 0,
  "payload": {
    "method": "sources.stage_uri",
    "uri": "http://example.invalid/fixture.flac",
    "metadata": ""
  }
}
```

An applied receipt carries a `sources` result tied to that exact request:

```json
{
  "kind": "sources",
  "value": {
    "generation": 1,
    "staged": {
      "media_id": "staged-1",
      "admission": "unadmitted",
      "uri_bytes": 35,
      "metadata_bytes": 0
    }
  }
}
```

Use `{"method":"sources.get"}` to inspect the current staging summary.
Its result has the same shape.
The query requires no active playback session.
It does not expose the original URI or metadata.
The general state and event snapshots do not gain staging fields.

The URI limit is 8192 UTF-8 bytes.
The metadata limit is 65536 UTF-8 bytes.
Typed commands must also fit those limits in retained string capacity.
Request identities must fit their existing 128-byte storage bound.
The descriptor uses exact-size immutable strings.
All controllers share the owner's existing bounded receipt history.
There is no per-controller descriptor list or replay cache.

A nonempty URI must have no surrounding whitespace or control characters.
These checks do not establish a valid URL, supported scheme, trusted origin, or permission for I/O.
Metadata remains opaque UTF-8 text; staging does not parse or trust XML.
The command accepts no origin grant or admission flag.
A caller cannot turn its input into an admitted source.

An empty URI with empty metadata clears the descriptor.
A clear advances the staging generation and returns `staged: null`.
Generation zero describes the initial empty query state.
A successful stage or clear always returns a positive generation.
An empty URI with nonempty metadata fails without changing staging.
Generation exhaustion fails without replacing the previous descriptor.

Staging changes only the canonical revision, event sequence, and staging state.
It preserves active playback, local queue, epochs, pending transport operation, graph work, gain, mute, and output consent.
The existing staged transaction hook must succeed before publication.
A failed commit leaves the previous descriptor and generation intact.
The returned error never echoes input text.

Raw input lives only in bounded owner memory and retained command history.
Neither the descriptor nor raw input enters checkpoints.
A restored owner starts with generation zero and no descriptor.
Old owner commands remain invalid after restart.
A staging ID has meaning only within its owner incarnation.

Replay remains bounded by the existing receipt retention policy.
While retained, an exact envelope returns its original result even after another descriptor replaces it.
A reused request ID with changed content fails.
After eviction, callers must rely on owner and revision fences rather than assume permanent deduplication.

This foundation does not qualify T14 or UPnP interoperability.
Network admission, origin policy, streaming, source takeover, protocol services, and controller qualification need later work.

## Verified Linux checkpoint

The integrated workspace passes 1031 tests, with zero failures and five existing skips.
The canonical check includes formatting, strict Clippy, real process tests, catalog validation and dependency validation.
Fourteen integrated client tests include the preserved discovery guards and new staging paths.
The isolated unit also passes twenty scoped tests through real owners, CLI and Rust FFI.
Those counts overlap the canonical suite.
The generation-zero clear regression fails before its matcher fix and passes afterward.
Independent review reports no actionable findings.
See [the source-bound receipt](evidence/network/uri-staging-linux-20261119.json).

This checkpoint does not qualify network playback, UPnP interoperability, Mac staging or new foreign-language scenarios.
