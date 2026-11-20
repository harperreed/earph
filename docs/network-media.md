# Network media reader

`earph-network` supplies an admitted HTTP(S) byte reader.
The library does not enable discovery, renderer controls, or another listener service.
The engine owner exposes explicit HTTP-FLAC playback through its existing private command socket.
Existing staged URLs remain unadmitted and perform no network I/O.

The local owner constructs `SourceBinding`, `OriginGrant`, and `FetchPermit`.
A binding names the owner, session, staged media, source generation, and grant generation.
Permit construction compares the complete binding with the owner's current binding.
All grants belong to that owner and grant generation.
A permit and its grants remain immutable after construction.
The owner fences submission and publication against its committed live state.
This library has no mutable grant registry or independent owner authority.
The owner's permits borrow pins and DER bytes from exact immutable grant-document entries.
They retain the authoritative shared allocation instead of copying its certificate payload.
Owned library construction uses the same policy and accessor implementation.
Per-hop clients still allocate parsed URLs, certificate objects, and TLS root stores.
Their root input remains capped at 64 certificates and 1 MiB per origin.
Those transport/parser allocations remain separate from Core's 16 MiB retained-authority accounting.
Sharing grant payloads does not establish a total TLS heap bound.

`NetworkWorker::start` accepts one permit and returns `MediaReader`.
The worker admits the first origin again before creating a thread.
One owned OS thread runs a current-thread Tokio runtime.
Its retained task runs the concrete Reqwest transport.
Every redirect checks the exact origin grant and monotonic expiry before connecting.
Redirect references receive raw authority checks before URL normalization.
Relative paths and query values containing `@` remain valid.
Replacement requires successful retirement of the preceding job.

The reader implements synchronous `Read` over four queued chunks.
Each queued chunk contains at most 16 KiB.
The reader and producer share the queue, terminal result, and condition variable.
No unbounded byte channel or body collection sits between them.
`Observation` retains the complete binding, phase, received bytes, queue length, and terminal result.
The byte count measures bytes admitted to the pipe, rather than wire bytes.

Terminal results distinguish complete HTTP bodies, cancellation, and transport failure.
The producer records a terminal result before waking the reader.
Only a verified body end returns EOF after queued bytes drain.
Failure and cancellation discard queued bytes and take precedence over reads.
An exited producer without a terminal result fails the read.
Cancellation uses `ConnectionAborted`, because standard Rust helpers retry `Interrupted` indefinitely.
HTTP completion does not establish that an unknown-length recording contains its original complete content.

`Cancellation::cancel` wakes blocked readers and the runtime supervisor.
The supervisor aborts the retained transport task and awaits its handle.
The runtime then drops its internal connection tasks and sockets.
`retire` cancels unfinished work and waits up to one second for thread completion.
A cleanup timeout retains the thread handle and keeps replacement blocked.
The caller can retry retirement and must retain that worker after a timeout.
Dropping the worker cancels and joins its thread; that final ownership safeguard can block.
Dropping an unfinished reader also requests cancellation.

## Admission and transport bounds

| Input or operation | Bound |
| --- | --- |
| URL | 8192 UTF-8 bytes; HTTP(S); no userinfo, fragments, controls, whitespace, or backslashes |
| Origin | Canonical scheme, hostname, effective port |
| Grant set | Eight exact origins; no duplicate origins |
| Address pins | Eight addresses per grant; no system DNS fallback |
| Explicit roots | 64 DER certificates and 1 MiB per grant |
| Redirects | Three followed hops; every destination requires a matching unexpired grant |
| Connect | Three seconds per hop, including TLS setup |
| Headers | Five seconds per hop |
| Startup | Fifteen seconds shared across redirects |
| Parsed response headers | 64 fields; 32 KiB of retained names, values, and delimiters |
| Pending body read | Five seconds, restarted only for the next body read |
| Consumer pipe | Four chunks of at most 16 KiB |
| Finite collection helper | 1 MiB of observed body bytes |
| Continuous stream | No invented total byte limit |

A full consumer pipe suspends body reads and their progress deadline.
This prevents a slow consumer from appearing to be a stalled server.

IPv4 policy rejects unspecified, loopback, link-local, multicast, reserved, transition, benchmarking, and documentation ranges.
IPv6 policy admits ordinary global unicast and ULA, excluding special transition and documentation ranges.
IPv4-mapped IPv6 addresses receive the IPv4 checks.
Explicit RFC1918 and ULA origins remain available for authorized LAN HTTP.
IP-literal origins require pins equal to that literal.
Hostname pins preserve the URL hostname for Host, SNI, and certificate checks.
A fail-closed resolver rejects every lookup absent an explicit override.

Reqwest defaults are disabled, with only its `rustls` feature enabled.
The client disables ambient proxies, automatic redirects, retries, referers, and HTTP/2.
No cookie, compression, authentication, or blocking-transport feature is enabled.
Requests advertise identity encoding; responses containing other encodings fail.
Each hop constructs a fresh client, so no credentials or connection pool cross origins.
HTTPS uses only the grant's explicit roots through `tls_certs_only`.
Certificate chain and hostname checks remain enabled.
This unit does not qualify native system-trust onboarding.

Hyper 1.12.0 separately caps its HTTP/1 parser buffer at 417792 bytes.
Its initial buffer size is 8192 bytes.
Reqwest exposes the header-count setting, but not Hyper's parser-buffer setting.
The 32 KiB check runs after parsing; it does not claim to cap parser allocation.
The body path can also retain one transport chunk while waiting for pipe capacity.
The four-chunk limit describes the application pipe, not total TLS and HTTP memory.
See Hyper's `src/proto/h1/io.rs` and the exact locked dependency inventory.

## Qualification and dependencies

The socket tests select a concrete private local interface through routing without sending a discovery datagram.
They bind only that address and construct exact grants for generated ports and hostnames.
Production admission contains no loopback or test bypass.
TLS fixtures generate temporary CA and server certificates under the ignored task target directory.
Their private keys never enter source control.
The TLS server checks Host and SNI and rejects mismatched names and chains.
An independent child process runs the same public reader and retires its real transport.

Run `cargo test -p earph-network --locked` for focused qualification.
The tests require a routed private IPv4 interface and OpenSSL for generated TLS fixtures.
Run `scripts/check` for canonical workspace verification.

The exact dependency graph, feature sets, and license hashes live in `docs/dependency-lock.json`.
`THIRD_PARTY_NOTICES.txt` retains the generated license texts, including AWS-LC's bundled terms.
Those terms include ISC, Apache-2.0, MIT, BSD-3-Clause, and permitted alternatives.
The Android verifier crate omits a license in its package.
Its pinned upstream MIT license and provenance live under `docs/dependency-licenses`.
No upstream implementation code was copied.

## Admitted FLAC composition

`earph_engine::network_decode::NetworkFlacWorker` composes this transport with the existing `StreamingFlac` decoder.
It accepts a checked permit and an independently captured current `SourceBinding`.
It compares every binding field before transport starts.
`inspect` and `take_frame` require the current binding again.
Their caller must serialize adoption with live owner mutations in the same actor turn or authority gate.
A binding copied from the permit does not establish that authority.

The worker owns one transport thread and one decoder thread.
The decoder exclusively owns its `MediaReader` and `StreamingFlac` instance.
The caller retains independent transport observation, cancellation, completion reception, and both retirement responsibilities.
Observer clones neither cancel transport on drop nor own retirement.

One slot retains one `DecodedPcmFrame`.
The decoder waits for an empty slot before decoding another packet.
The slot holds at most 65,536 frames, or 524,288 stereo sample payload bytes.
This bound excludes decoder storage, conversion scratch space, allocator overhead, and upstream parser allocations.
It establishes neither a total heap bound nor a CPU budget.
Readiness retains the measured format and `Option<u64>` duration.
Unknown duration remains `None` after successful completion.
Accepted frames retain their immutable source binding and original positions and sequences.
The worker assigns no queue entry, epoch, time domain, graph acknowledgement, or native output authority.

The lock order is decode state, then the transport pipe.
`FetchObserver::publication_guard` rejects stale binding, cancellation, failure, and exited transport without a terminal result.
It permits completed transport while decoded frames drain.
`take_frame` checks owner cancellation and job failure before moving the frame under both locks.
That move defines this worker's acceptance point.
Readiness and successful completion also require the transport publication guard.
No decoding, waiting, retirement, callbacks, or output I/O runs under the publication guard.
Never reenter transport, cancel it, or drop its reader while holding that guard.

A transport failure published first prevents a later frame handoff.
A frame accepted first can precede a subsequent failure.
Later output integration must fence authority again at its own acceptance point.
This worker cannot retract an earlier native submission or prove stale hardware playback impossible.

Inspection reads live transport state even while PCM fills the slot.
Adopting a transport failure clears that slot and wakes the decoder.
Owner cancellation also clears the slot, wakes blocked reads, and revokes handoff after HTTP completion.
Transport completion alone cannot establish media success.
Success requires media EOF, matching HTTP completion, and an empty PCM slot.
Media failure, transport failure, owner cancellation, and success remain distinct outcomes.
The first recorded genuine failure survives later cancellation and retirement attempts.
Reader destruction during media rejection cannot replace the decoder's failure with owner cancellation.
An absent FLAC MD5 and unknown original completeness retain the decoder's existing limits.

Ownership progresses through `Idle`, `Active`, and `Retiring`.
Retirement first revokes handoff and requests cancellation.
It waits up to one second for decoder completion, then joins the decoder.
Transport retirement has its own one-second wait.
The composition therefore permits two separate bounded waits, rather than one shared second.
A timeout retains the retiring job and every unjoined handle.
Replacement stays blocked until a later retirement attempt succeeds.
If decoder startup fails, the started transport remains owned in `Retiring`.
Final destruction may block while joining unfinished work.

Real HTTP tests cover exact 16-bit and 24-bit PCM, known and unknown duration, and child-process usage.
They cover buffered failures, stale authority, cancellation after completion, full buffers, peer closure, and partial media tails.
An owned decoder gate tests timeout retention without replacing the real transport.
The startup-error test injects the thread creation error result; it does not exhaust system thread resources.
The transport's existing ownership test separately covers its cleanup timeout.
Run `cargo test --locked -p earph-engine --test network_decode` for the public worker scenarios.
Run `cargo test --locked -p earph-engine --lib network_decode` for deterministic ownership scenarios.

## Owner playback

The owner supports qualified 16/24-bit mono/stereo HTTP-FLAC through explicit local commands.
See [the owner control guide](owner-http-playback.md) for literal command payloads and receipt rules.
Staging and granting perform no fetch.
The host validates real origin, pin, certificate, expiry, and source policy before committing authority.
Exact request replay does not renew grants or repeat transport.
Checkpoint restore retains no grant, URI descriptor, network source, or output consent.

One concrete local/HTTP source reader feeds the existing playback worker and native output owner.
Readiness uses received FLAC format and an optional frame total.
Startup Pause retains paused intent while preparation continues and still publishes measured readiness.
Preparation submits no source PCM before explicit graph activation and Unmute.
The network lane requires floating streaming processing, including for same-rate graphs.
Strict preservation, seek, queue advancement, and network profiles remain unavailable.
The local queue and its readiness remain independent of network playback.

Pause and Mute retain bounded pre-volume output through the existing streaming hold/replay path.
The owner never seeks, reopens the URI, or spills network media to disk.
Temporary decoder starvation differs from media EOF.
Source epochs stay immutable while native output and graph epochs advance.
`source_position_frames` stays cumulative for the HTTP source.
Processing counters describe the currently installed graph and retain its separate graph/output epochs.
Graph replacement starts at the actual accepted source frontier without seeking.
If replacement discards held generated output, `replacement_boundary` records its measured count and previous client PCM format.
The record names both output epochs and retains the immutable source binding and source epochs.
It can remain as a historical same-source transition after later Pause or progress.
Zero discard creates no boundary record; local `processing.source_boundary` remains unused for network playback.
Native writes recheck live binding, expiry, cancellation, and transport state before each submission.
These checks hold no decoder or transport lock during native I/O.
An accepted native prefix remains irreversible.

Revocation, grant replacement, expiry, Stop, failure, and shutdown retire the exact owned source.
The IPC actor expires authority during idle and paused turns using its current admission clock.
Native stop and bounded decoder/transport retirement run on the playback worker.
A timeout retains ownership and blocks replacement.
An exact retirement acknowledgement settles submission retirement, rather than instantaneous physical silence.

Real owner tests use the private command socket, genuine FFmpeg FLAC, and libasound file-to-null capture.
They cover formats, nullable totals, graph readiness, FIFO pause/mute/replay, SRC tails, and queue-preserving takeover.
They also cover idle expiry, revocation replay, startup Stop/shutdown, media failure, and native failure.
Run `cargo test --locked -p earph-engine --test service_playback network_owner_` for these scenarios.
No physical audio device is opened by these fixtures.
The tests do not establish Music Assistant interoperability, UPnP, Sendspin, or universal network security qualification.
