# Committed Sendspin credentials

`CommittedCredentials` keeps permanent device identity, permanent pairing PSK, catalog, and last-playback server together.
The existing `RecordCatalog` owns pairing records and superseded identifiers.
Restoration preserves each record's use order, including counter exhaustion.
The document supports the selected driver's five-record catalog.

`encode_secret()` returns redacted `EncodedCredentials`.
Its explicit `as_secret_bytes()` method exposes plaintext for the credential adapter.
Never log those bytes or write them to a normal application checkpoint.
The document and its encoded wrapper redact Debug output.
Errors contain fixed classifications without input values.
The codec makes no protected-memory or secure-erasure claim.

`decode_secret()` accepts at most 65536 bytes, including whitespace.
It rejects malformed JSON, duplicate fields, unknown fields, unknown versions, invalid key lengths, and invalid catalog state.
Every field must exist; use explicit `null` for an absent last-playback server.
A missing field cannot silently reset stored history.
The permanent pairing PSK cannot equal Sentinel or a retained long-term PSK.
Decoding never generates identity or repairs corrupt data.

This format is a secret-storage boundary without an OS backend.
The tests use synthetic keys, retained private files, real catalog operations, and a real authenticated Noise exchange.
Their plaintext files are test fixtures and must never become a production fallback.
Production storage still requires explicit native provider selection, bounded operation ownership, exact commit readback, and uncertainty recovery.
Unavailable, locked, ambiguous, corrupt, or uncertain storage must never trigger identity regeneration.
No production startup or credential access follows from this codec.
