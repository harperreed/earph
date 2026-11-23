# Bounded encrypted Sendspin session

`earph_sendspin::session::ClientSession` owns one client connection's protocol state.
It controls message order, fragmentation, Noise nonces, writes, pairing, admission, and key installation.
It uses the pinned Sendspin specification and the existing Pairing PSK reducer.
It starts no listener, socket, background task, application session, clock filter, or native output.

Create `SessionConfig` with a static private identity, permanent Pairing PSK, committed catalog, and connection generation.
Supply a hello payload with the client's name and supported roles.
The session advertises exactly `supported_pair_methods: {"pairing_psk": {}}` and disables unpaired access.
It checks the complete hello's 16384-byte and depth-10 limits before emission.
The caller supplies valid support objects and role capabilities.
Production identity persistence and catalog storage require a separately qualified credential adapter.

Call `next_write(now_ms)` to obtain a `TransportWrite`.
Write that entire text or binary transport message.
Report its opaque `WriteId` through `complete_write` exactly once.
Use `WriteResult::Complete` only when the entire transport message finished writing.
Report a failed, partial, uncertain, or cancelled write as `Failed` or `Uncertain`.
Either result closes the connection permanently.
Only one write result may remain outstanding.
WebSocket frame assembly and control frames belong to the transport adapter.

Call `receive(kind, bytes, arrival)` with one complete transport message and a checked `ReceiveStamp`.
Sample its final-byte monotonic microseconds before calling the session or decrypting it.
Keep that exact stamp through deferred ciphertext, decrypted messages, and final-fragment reassembly.
Derive milliseconds from the same domain for completions, `tick`, and the existing deadline policies.
Never reconstruct receive microseconds from truncated milliseconds.
Call `tick` when idle so fixed deadlines can expire.
The adapter must bound its own socket buffers and transport message assembly.
The library cannot bound memory allocated before its receive call.

The session emits client init, preserves its exact bytes, and binds the exact server init bytes.
It requires cleartext handshake, authenticated server hello, completed client hello, and initial activation in order.
It rejects recognized messages with the wrong direction or sequence.
It validates every ID-0 envelope before ignoring an unknown type.
Unknown types and unimplemented IDs become ignorable only after initial activation.
IDs 2 and 3 remain reserved IDs and never become legacy fragments.

`SessionEvent::Admission` carries a privately constructed `AdmissionProposal`.
Inspect its activities, proposed roles, and optional `PairedIdentity`.
Return `admit(proposal.id(), accepted, now_ms)` for that exact proposal.
A stale proposal cannot change state or advance the clock.
The existing pairing reducer provides the single activation validator and canonical sticky role list.
A validated activation retires the previous unacknowledged attempt before waiting for admission.
Its unstarted flight messages cannot begin while admission waits or after admission rejects it.
Only accepted admission applies roles or requests a pairing candidate.
A method-only rejection sends ordered `pair/abort` and waits for a valid activation.
A close rejection stays suppressed until its ordered goodbye completes and the channel closes.

`PairedIdentity` requires authenticated transport under the matched, server-bound long-term credential.
It carries the exact server public key, matched credential identifier, and handshake generation.
The shared Noise selector supplies that identifier; the session performs no second credential selection.
Cryptographic readiness, paired identity, admission, clock readiness, and output authority remain distinct.
Neither an admission result nor a paired identity grants a native output lease.
This crate produces no clock-ready or output-ready token.

Use `send(ApplicationMessage, now_ms)` for caller-validated application messages.
Generic application sends reject caller-stamped `client/time`.
Use `time_probe_write(ReceiveStamp)` only after transport readiness and earlier session output drain.
That dedicated writer serializes and encrypts the sampled T1 through the existing write ownership path.
`clock_context()` returns authenticated binding facts, actual send suppression, and ordered delivery blocking.
Those facts grant no clock-ready or output-ready token.
A refusal returns the original message in `SendRejected`.
Only one producer message may wait or run at a time.
The owner generates one fragment at a time and encrypts only when obtaining the next write.
It never queues producer ciphertext for a future epoch.
`SessionEvent::Unsent` returns an unstarted producer when a re-handshake begins.
The caller may revalidate it after later admission.
A message whose first record already started finishes before the re-handshake reply.

`SessionEvent::RequestCandidate` carries the reducer's complete `AttemptId`.
Return a fresh candidate through `candidate` for that attempt.
The session orders pairing init and finalize around real complete-write results.
The writer requires positive reducer authorization before starting each init or finalize message.
The reducer owns the attempt deadline and first-send timestamp.
Timeout, cancellation, and supersession remove revoked unstarted flight messages.
A started message finishes its fragments through the same writer, while revoked completions cannot commit.
An uncertain transport-message result still closes the connection.
It defers a server acknowledgment received before the local finalize write completes.
`SessionEvent::CommitRecord` transfers a durable-storage operation to the adapter.
The adapter must finish acknowledged storage work even when the channel closes.
Keep the closed owner reachable and report `commit_succeeded` or `commit_failed` for the original attempt.
An uncertain storage outcome requires explicit recovery; it never becomes a successful commit.
A new handshake cannot turn unacknowledged staging into committed credentials.

The complete authenticated re-handshake message one starts send suppression immediately.
This also applies while an acknowledged commit or earlier admission delays its processing.
The session fences old attempt effects immediately and pauses old pairing ticks during the quiet period.
The fixed exchange deadline continues to run.
Ordered processing retires unacknowledged old attempts without generating cancellation controls.
Earlier authenticated acknowledgments and their durable commits retain their wire order.
Pairing abort writes carry their attempt generation; only a current activation response may begin while sends remain suppressed.
The pending Noise responder uses the immediately preceding hash and unchanged identities and suite.
It requires an exact category and identifier match with the carried server binding.
A declared matching Sentinel remains valid; lookup fallback is disabled during rotation.
The owner wraps message two as encrypted ID-0 JSON under the old transport keys.
It installs the pending keys only after that complete transport write succeeds.
It increments the checked handshake generation and resets the pairing index at that boundary.
The connection generation stays unchanged.
Old and resolving credential protections remain until their epochs retire.
The permanent Pairing PSK remains available in every generation.

New-key records received before that local write acknowledgment stay bounded and encrypted in the pending-input queue.
The owner decrypts them only after installing the new epoch.
It never tries both epochs after an authentication failure.
The first complete new-key message must be `server/activate`, including when fragmented.
It sends no init or hello replay.
Application sends remain blocked through validation and admission of that activation.
A second handshake before activation fails.

`SessionEvent::Message` contains an authenticated complete control or implemented binary message.
Its arrival timestamp belongs to the final fragment.
It establishes authentication and sequencing; it does not validate a role's payload or grant role authority.
The caller must validate role activation, role payloads, stream ordering, and resource admission before use.
Call `decode_player_audio` only behind that consumer boundary.
The caller owns stream configuration, buffered chunks, and clock context across key changes.
Apply `ActivationNotice::removed_roles` cleanup once, following the role's ordinary removal rules.
Keep unchanged roles' state intact.
The server must send required stream ends before a rotation that removes a stream role or playback.
This library does not reconstruct streams or implement clock discipline.

`SessionLimits` and `MessageLimits` are explicit local resource policies.
They do not add an application memory allowance or replace caller resource limits.

| Limit | Default | Unit |
| --- | --- | --- |
| Reassembled body | 64 MiB | Original body bytes, excluding message ID |
| Fragment count | 4096 | Records in one fragmented message |
| Assembly deadline | 30000 | Absolute monotonic milliseconds from first fragment |
| Pending input | 64 MiB | Retained activation, queued messages, queued ciphertext, and fragment bodies |
| Pending output | 64 MiB | Producer bodies, control queue, and outstanding transport write bytes |
| Pending messages | 4096 | Retained input or queued control message entries |
| Cleartext transport message | 100000 | Bytes including its JSON envelope |
| Exchange deadline | 30000 | Absolute milliseconds for initial bring-up, rotation, or delayed admission |
| Noise ciphertext record | 65535 | Bytes including the authentication tag |
| Noise plaintext record | 65519 | Bytes including message ID and fragment header |

The unfragmented body holds at most 65518 bytes.
First fragments hold at most 65516 data bytes; continuations hold at most 65517.
The owner checks additions and limits before allocation or copying.
Unknown fragmented IDs retain counters instead of bodies, while enforcing authentication, flags, counts, and deadlines.
Budget, authentication, protocol, clock, deadline, and ambiguous-write failures poison the channel permanently.
Close clears pending crypto, fragment bodies, input, and writes.
Debug and error output omit wire bodies and credentials.
No secret-memory erasure claim is made.

Portable tests use real Snow peers and the synthetic filesystem pairing fixture.
Explicit Python tests use private bounded pipes, with message and output caps, response deadlines, and joined reader threads.
They hash-check nine installed modules from aiosendspin 10.2.1 at commit `692831b07bc453a38d5a39a16ee566141547bfa1`.
The successful peer calls actual handshake, encrypted wire, pairing, and re-handshake drivers.
It proves fixture commit/readback, two rotations, fragmented traffic, sticky roles, timestamps, and retained caller context.
The old-key failure scenario corrupts a real record and requires the actual Python server driver to reject it.
The fixture's `_fixture@v1` role defines a synthetic state object solely for a valid large client message.

Run `cargo test --locked -p earph-sendspin` for portable checks.
Set `EARPH_SENDSPIN_PEER` to the pinned offline Python launcher for explicit peer checks.
Run `cargo test --locked -p earph-sendspin --test noise_python --test pairing_python --test session_python -- --ignored --nocapture`.
Root runs the integrated repository check and independent security review before publication.
Actual Music Assistant, controller interoperability, app startup, production storage, clock synchronization, and playback remain unqualified.

The [Linux session receipt](evidence/sendspin/session-linux-20261120.json) binds the reviewed integration, combined check and six explicit peer tests.
