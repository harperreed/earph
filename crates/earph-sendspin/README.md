# Sendspin protocol components

This crate implements the player framing contract pinned by `SPEC_REVISION` in `src/lib.rs`.
The Noise responder uses Snow 0.10.0 with the selected Curve25519, ChaChaPoly, SHA256, and OS RNG primitives.
The application does not advertise or start Sendspin from this crate.

Call `decode_player_audio` only after authenticating and reassembling one ID-4 plaintext message.
Pass the admitted byte limit, including the thirteen-byte header.
The result borrows its payload and preserves signed server microseconds and unsigned send-ahead microseconds.
Saturated send-ahead values remain observations; they do not define playback deadlines.
Debug output exposes the payload length, rather than audio bytes.

The parser supports the pinned thirteen-byte layout only.
It does not infer a legacy protocol version from arbitrary payload bytes.
The future authenticated dispatcher must enforce the version lock.

The binary fixture comes from actual aiosendspin 10.2.1 producer calls.
Its provenance records the exact upstream commit, producer source hash, interpreter, and fixture hash.
The installed producer matched the inspected pinned source byte-for-byte.
No upstream source code is copied into this crate.

## Bounded encrypted responder

`noise::Responder` implements the client side of `Noise_KKpsk2_25519_ChaChaPoly_SHA256`.
The server always acts as the initiator.
Pass exact raw `client/init` and `server/init` UTF-8 message bodies into `Responder::new`.
The constructor validates their defined fields and concatenates their original bytes in that order.
It never rebuilds their JSON for the prologue.
Unknown payload fields are ignored, including fields inside the first encrypted handshake payload.

This unit accepts complete raw Noise records.
The caller must bound and decode the cleartext base64url `noise/handshake` envelope before passing its record.
Wrap the returned second record in the specified envelope without changing its bytes.
Its encrypted plaintext is the exact literal two bytes `{}`.
WebSocket frames, socket buffering, deadlines, and text/binary message admission belong to the future transport adapter.

The first authenticated handshake payload selects a typed credential by SHA256 identifier and category.
A matched long-term credential must bind the exact `server/init` public key.
A lookup miss falls back to the public Sentinel only for this initial handshake.
Pairing credentials remain distinct from long-term credentials and Sentinel.
This API accepts supplied credentials; it does not create or persist pairing records.
Generate new static private keys with `generate_static_private_key`, which uses Snow's OS CSPRNG.
Snow generates each handshake's ephemeral key.

The lifecycle is `Handshake` → `PendingConfirmation` → `Established` → `Closed`.
Writing the second handshake record enters `PendingConfirmation`.
It does not prove the server possesses the matching PSK.
Only successful authenticated server transport traffic confirms that possession.
Application writes require this confirmation.
This bounded API therefore also defers the optional pre-confirmation encrypted `client/goodbye`.
`Established` describes cryptographic readiness only; it grants no pairing, role, playback, or owner authority.
Sentinel never implies paired access.

Every operation failure closes the channel permanently and drops its owned cryptographic state.
This includes wrong phases, malformed selection, key misbinding, budget errors, tag failures, replay, and ordering failures.
`close` cancels any phase and drops state immediately.
The channel cannot be cloned, and an advanced failed state cannot be retried.
Debug and errors omit keys, PSKs, plaintext, ciphertext, and tokens.
Dropping buffers or library state makes no secret-memory erasure claim.

`Limits` bounds each init body, handshake plaintext, record, application plaintext, and returned output.
The protocol caps each record at 65535 bytes, including its sixteen-byte tag.
The largest application plaintext record is 65519 bytes, including its message ID.
The responder checks sizes before allocating record or plaintext buffers.
It rejects empty authenticated application plaintext.
It returns complete authenticated plaintext records without dispatching their application messages.

The caller must validate JSON envelopes, message direction, initial hello/activate sequencing, role authority, and fragmentation.
It must authenticate discarded records and preserve one fragmentation sequence per direction.
No record can reach the player parser through this library without the caller's explicit dispatch.
The responder owns no listener, output source, native output, pairing UI, token presentation, or identity persistence.

The `pairing_token` module implements version-zero Pairing PSK token export and decoding.
The `pairing` module plans protected records and reduces authenticated Pairing PSK events.
It ends at a durably stored record while the current session remains unpaired.
The [pairing contract](../../docs/sendspin-pairing.md) defines its ordered writer and commit adapter obligations.
Persistence runs only in test support with synthetic credentials and explicit private fixture directories.
Production identity storage, the secret-manager adapter, and deliberate token presentation remain later gates.
The full in-band re-handshake also remains a next gate.
It must retain stream and clock state, enforce the quiet period, and authenticate permitted discarded old-key traffic.
It must use the prior handshake hash as prologue and require `server/activate` first under the new keys.
`handshake_hash` exposes that hash for future composition; it does not implement or authorize a re-handshake.
The current constructor always validates a fresh init exchange and supplies initial-handshake Sentinel fallback.
It must not be reused as a shortcut for the stricter re-handshake rules.

Portable Rust tests use a real Snow initiator.
The ignored `noise_python` test requires `EARPH_SENDSPIN_PEER` to name an explicitly pinned Python interpreter.
Install aiosendspin 10.2.1 in that isolated environment before running it.
Run `cargo test --locked -p earph-sendspin --test noise_python -- --ignored --nocapture`.
The peer uses actual `NoiseSession` producer and consumer calls with fresh synthetic test identities.
It covers both directions, categories, fallback, wrong PSK, raw prologue tampering, wrong static key, corruption, replay, and order.
It also checks the exact 65535-byte record boundary in both directions.
The peer opens no network connection and emits no audio.
The [Linux qualification receipt](../../docs/evidence/sendspin/noise-linux-20261120.json) binds the integrated source and real peer results.

The selected Snow dependency has default features disabled.
Only `use-curve25519`, `use-chacha20poly1305`, `use-sha2`, and `use-getrandom` are enabled.
Workspace SHA256 helpers retain sha2 0.11.1; Snow retains its published sha2 0.10.9 dependency.
MIT is selected wherever offered; the inventory retains applicable BSD, Unicode, and other license texts.

Discovery, arbitration, decoding, scheduling, and native output remain outside this crate.
Actual Music Assistant interoperability and independent-clock capture remain separate required gates.
Doctor Biz approved the pinned specification licence for this implementation.
The exact terms are retained at the repository root in `SENDSPIN-SPEC-LICENSE.md`.
Earph's existing MIT licence remains in place.

See the [integrated pairing receipt](../../docs/evidence/sendspin/pairing-linux-20261120.json) for source-bound checks and limits.
