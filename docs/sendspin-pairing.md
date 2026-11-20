# Bounded Sendspin Pairing PSK

This unit implements version-zero token export, protected record planning, and a pure client pairing reducer.
It follows [spec 9d2f15ee](https://github.com/Sendspin/spec/tree/9d2f15ee8f172c67ce00dc0d9f6603c3b1030d9f).
Peer tests use [aiosendspin 10.2.1](https://github.com/Sendspin/aiosendspin/tree/692831b07bc453a38d5a39a16ee566141547bfa1).
The test peer checks installed source hashes before calling its real producers and consumers.
No upstream implementation code is copied into Earph.

The unit ends in `RecordStored` after durable commit and readback.
The current Noise selection remains `pr`, which remains unpaired.
Only the later long-term PSK re-handshake can produce a paired session.
The existing `noise::Responder` source and API remain unchanged.

## Tokens and secrets

`encode_pairing_token` explicitly exports a credential.
Its output must never serve as a logging helper.
`decode_pairing_token` accepts ASCII, surrounding whitespace, either case, and an optional `SP:` prefix.
It accepts ordinary base32 `2` and transformed `9`.
It rejects wrong versions, malformed payloads, internal separators, and payloads shorter than 64 bytes.
It ignores decoded extension bytes after the first 64 bytes.
The consuming server must compare the decoded public key with the connection's exact `client_id` before pairing.

Tokens, records, candidates, flights, reducer state, and credential-bearing events redact their Debug output.
Wire exports intentionally contain credentials.
Dropping these values makes no memory-erasure claim.

## Catalog ownership

Start with `RecordCatalog::new(5)`.
Use `plan_replace` to build a complete replacement without changing committed state.
Commit insertion, same-server replacement, superseded metadata, and eviction together.
Derive each PSK identifier from its PSK rather than persisting a second mutable identifier.
The identifier uses the same protocol label as the frozen Noise selector.

Protect identifiers backing provisional, admitted, and resolving handshakes.
The owner must keep all concurrently open paired connections below capacity.
That limit counts connections even when several use one credential.
Call `ensure_admission` before admitting an attempt.
An all-protected full catalog is an owner invariant failure, rather than a finalize rejection policy.

Replacement keeps a protected prior version in the same lookup catalog.
It marks that version superseded.
When protection ends, commit `plan_release` to remove superseded versions and apply capacity again.
A protected old version can outlive eviction of its current replacement.
Restore its superseded marker with the committed catalog so later release still removes it.
Eviction selects the least used unprotected record, then its derived identifier as a stable tie-break.
It never selects the newly inserted record.
Use `plan_mark_used` to plan persisted ordering updates after matched handshakes.

## Ordered reducer adapter

Construct `PairingClient` with the confirmed Noise selection, server public key, and both local generations.
Supply only selections confirmed by authenticated server traffic.
Retain the stable device Pairing PSK outside individual attempts.
Supply the owner's committed catalog and current protection set.
Serialize catalog writers and keep that protection set fixed throughout this bounded reducer's lifetime.
The later shared connection owner must reconcile catalogs and protection before admitting concurrent attempts.

Call `handle(event, now_ms)` with a monotonic clock.
Authenticate and reassemble each message before calling the reducer.
Validate direction and the surrounding hello/activate protocol in the later dispatcher.
The reducer validates activation fields and object payloads for `server/pair-finalize`.
It ignores unknown extension fields.

Every pairing activation advances its index before method selection.
Unsupported methods consume an index while leaving the connection open.
The reducer applies activation rejection precedence before requesting candidate material.
Unpaired playback stays disabled.
An admissible `pr` pairing activation requests one fresh candidate.
The credential adapter must supply a fresh CSPRNG PSK for each request.
The reducer rejects Sentinel, the device Pairing PSK, and committed PSK reuse.
It never uses the static-private-key generator as a PSK generator.

Use one ordered owner for writes, activation delivery, and commit completion.
Keep `AttemptId` local and fence queued work with all three fields.
Recheck that an effect still belongs to the current attempt before writing it.
Never send an old queued flight after cancellation or supersession.

Process `SendFlight` as one ordered flight without waiting for a peer response:

1. Write `client/pair-init` with the exact current `pairing_index`.
2. Report `PairingInitSent(id, first_sent_ms)` using the init emission clock.
3. Process any returned abort or close before proceeding.
4. Report `PairingFinalizeStarted(id)` immediately before starting the finalize write.
5. Process any returned abort or close before proceeding.
6. Write `client/pair-finalize` with the flight's unpadded base64url long-term PSK.
7. Report `PairingFlightSent(id, first_sent_ms)` after both writes complete.

The progress events distinguish cancellable staging from a write that may have revealed the credential.
Cancellation before finalize starts sends `user_cancelled` and drops staging.
After finalize starts, cancellation reports `Finishing` because rollback cannot be promised.
The acknowledgment timeout measures elapsed time from init emission, including time spent writing the flight.
Report send failure or disconnect explicitly.
Do not deliver a queued acknowledgment before reporting completed writes.

`server/pair-finalize` emits exactly one `CommitRecord` and enters `Committing`.
No earlier event commits a record.
Complete this acknowledged commit before processing a later activation or disconnect.
`DeferUntilCommitted` leaves the event in the owner's ordered queue for later delivery.
Keep that event's original wire position and supply the current monotonic clock when redelivering it.
Do not run the deferred event concurrently with commit work.
Duplicate finalize messages are protocol errors.
If the channel closes during commit, still deliver its eventual commit completion.

Report `CommitSucceeded` only after durable storage and exact readback.
Report `CommitFailed(Unchanged)` only when storage definitely retains the prior committed catalog.
Report `CommitFailed(Uncertain)` when the adapter cannot establish the committed result.
Uncertainty closes the channel and requires recovery and readback before another session loads credentials.
Never assume rollback or blindly retry an uncertain commit.

A cancelling server activation before acknowledgment discards staging, including after client finalize.
Timeouts and local aborts leave the connection open and discard late pairing traffic until the next activation.
Remote `concurrent_attempt` closes an active attempt.
A late abort after the attempt ends has no effect.
Sentinel fallback never deletes existing records.
Restart restores committed records only and never restores an attempt or infers success from a lost acknowledgment.
Reset the pairing index only when constructing the reducer for a successfully completed new Noise generation.

## Verification boundary

Portable tests cover the published token vector, deterministic protected retention, reducer ordering, and cancellation.
They also cover stale callbacks, malformed messages, duplicate acknowledgments, counter limits, and storage outcomes.
The in-process end-to-end test uses actual Noise records and a real atomic filesystem fixture.
The fixture stages a complete catalog, flushes it, renames it, flushes the directory, and reopens it.
Restart checks cover staged writes, finalize boundaries, and uncertain replacement.
The fixture uses public synthetic credentials in an explicit private directory.
It provides no production persistence API or default vault path.

The ignored `pairing_python` tests require `EARPH_SENDSPIN_PEER` to name the pinned Python launcher.
Run `cargo test --locked -p earph-sendspin --test pairing_python -- --ignored --nocapture`.
They call actual Python token functions in both directions.
They run `run_pairing_psk_client` through real encrypted sessions to produce its reference flight.
They run `run_pairing_psk_server` against Earph's encrypted flight and verify storage precedes acknowledgment.
Their lost-ack case leaves Earph's committed catalog unchanged despite the real peer storing its record.
These are end-to-end tests of this bounded library unit.

The dispatcher, production secret-manager adapter, token presentation, and full re-handshake remain later units.
Later integration must advertise exactly `supported_pair_methods: {"pairing_psk": {}}`.
It must keep unpaired access disabled.
This unit starts no app, listener, native output, pairing window, or code emission.

The [Linux pairing receipt](evidence/sendspin/pairing-linux-20261120.json) records the combined check and real peer qualification.
