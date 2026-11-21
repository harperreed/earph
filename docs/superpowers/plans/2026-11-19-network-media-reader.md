# Bounded Network Media Reader Implementation Plan

> Use subagent-driven-development or executing-plans for implementation and verification.

**Goal:** Read explicitly admitted HTTP(S) media through bounded memory with observable cancellation and cleanup.

**Architecture:** Add one source adapter library, `earph-network`, beside `earph-media`.
The local owner supplies exact origin grants and immutable source bindings.
One owned current-thread Tokio worker runs the concrete Reqwest transport.
Its synchronous consumer receives a bounded byte pipe for later decoder integration.

**Tech Stack:** Rust 1.98, Reqwest 0.13.5, Tokio 1.54.1, existing Core identities.

**Spec:** `docs/SPEC.md` sections 6.2 and 16.2; SRC-06; applicable AC-20 controls.

## Scope and assumptions

Doctor Biz authorized autonomous completion and parallel work.
Root owns dependency adoption, shared interfaces, integration and publication.
Implement this unit on `feat/network-media-reader`, starting at commit 7167861.
The identical starting commit passed the canonical Linux check with 1157 passes and five existing skips.
Reuse that verified baseline; run the full canonical check after this unit changes code.
Preserve every existing worktree, artifact and private media file.

This unit adds the reader library and real socket qualification.
Owner commands, grant onboarding, DNS discovery, renderer services and audio integration follow this unit.
Existing staged URLs remain unadmitted and cause no network I/O.
Do not advertise T14, Music Assistant interoperability or complete network security qualification yet.

## Files and ownership

- Create `crates/earph-network/Cargo.toml` and `src/lib.rs` for the adapter boundary.
- Create `src/policy.rs` for grants, bindings, URL admission and destination checks.
- Create `src/fetch.rs` for the owned worker, byte pipe, terminal state and cleanup.
- Add real socket integration tests under `crates/earph-network/tests`.
- Add a small process fixture only if needed to verify independent process cleanup.
- Update workspace membership, `Cargo.lock`, `docs/dependency-lock.json` and required license notices together.
- Document the actual lifecycle in `docs/network-media.md` and `docs/state-machines.md`.

The implementation owner may split tests by scenario when that improves readability.
Keep each source file focused; avoid a transport plugin trait or a second owner authority.

## Contracts and budgets

Use `reqwest::Url` for URL parsing.
Reject userinfo, fragments, unsupported schemes, control characters and URLs over 8192 UTF-8 bytes.
An origin includes canonical scheme, host and effective port.
A grant includes exact origin, explicit addresses, expiry, owner identity and grant generation.
A fetch binding includes owner, session, staged media identity and source generation.
Keep URI and certificate bytes out of Debug output and error text.
Validate complete bindings and grant expiry before starting each hop.
Retain binding identity with observations and completion.

Allow at most eight grants and eight addresses per grant.
Keep HTTP available for explicitly authorized LAN origins.
Reject loopback, unspecified, multicast, link-local and other prohibited special destinations.
Apply IPv4 rules to IPv4-mapped IPv6 destinations.
Use explicit address pins and a fail-closed resolver with no system DNS fallback.
Preserve the URL hostname for Host, SNI and certificate validation.
Disable ambient proxies, redirects, retries, cookies, authentication forwarding, referers and HTTP/2.
Follow at most three redirects manually after admitting each destination against the supplied grants.
Use explicit CA roots for this unit's HTTPS qualification; retain chain and hostname checks.
Limit roots to 64 certificates and 1 MiB per grant.

Use a three-second connect deadline and five-second header deadline per hop.
Apply one shared 15-second startup deadline across redirects.
Limit parsed header count to 64 and retained header bytes to 32 KiB.
Document Hyper's separate parser allocation ceiling from the resolved source.
Accept identity Content-Encoding only.
Use four queued chunks of at most 16 KiB.
Apply a fresh five-second progress deadline only while a body read is pending.
Do not count a full consumer pipe as a server stall.
Do not invent a total byte limit for continuous streams.
Bound any finite collection helper to 1 MiB of observed bytes.

Record success, cancellation or transport failure before closing the byte pipe.
Return EOF only for verified body completion.
Channel loss without a terminal result is an error.
Cancellation must wake both a stalled producer and a blocked consumer.
Abort the async task and join its retained handle before retirement.
Keep unfinished OS-thread ownership when a one-second cleanup deadline expires.
Expose failed cleanup rather than claiming success or silently detaching work.
Never use uninterruptible blocking DNS or network reads.
Allow one active fetch per worker; require retirement before replacement.

## Task 1: Admit exact origins and source bindings

- [x] Write policy tests before implementation and retain the expected failing run.
- [x] Cover canonical origins, explicit LAN HTTP, userinfo, malformed schemes and oversized input.
- [x] Cover forbidden addresses, mapped IPv6, missing pins, expiry and mismatched owner/source generations.
- [x] Implement typed private fields with checked constructors and redacted errors.
- [x] Verify denied input cannot construct a transport permit.
- [x] Run focused policy tests and strict Clippy.

## Task 2: Read and retire real HTTP(S) jobs

- [x] Write real socket scenarios before implementing the reader.
- [x] Verify exact known-length and unknown-length chunked bodies with a slow consumer.
- [x] Verify authorized redirects and refusal before connecting to ungranted destinations.
- [x] Verify hostname pins, Host/SNI, trusted TLS and certificate mismatch refusal.
- [x] Verify no system resolver fallback occurs without a pin.
- [x] Verify malformed chunks, truncation, compression, oversized headers and stalled reads fail distinctly.
- [x] Verify cancellation during startup, headers, body and full-pipe backpressure wakes both sides.
- [x] Verify peer closure, task join, thread retirement and replacement binding isolation.
- [x] Implement the smallest concrete worker and pipe that pass these scenarios.
- [x] Keep real fixtures on explicitly admitted local interface addresses; add no production loopback exception.

## Task 3: Qualify and integrate

- [x] Resolve the exact dependency closure and inspect every new license obligation.
- [x] Add accurate inventory and required notice files before publication.
- [x] Pass unit, integration and an independent process usage scenario using real sockets.
- [ ] Pass `scripts/check` with the canonical child environment and retained logs.
- [x] Obtain one independent main-model security, ownership and concurrency review of the actual diff.
- [ ] Fix material findings and rerun only affected checks.
- [ ] Record evidence, commit the reviewed unit and integrate through Root.
- [ ] Continue owner onboarding and streaming decoder integration toward T14.

## Review focus

Confirm redirect admission occurs before any connection.
Confirm client errors cannot disclose URLs, credentials or certificate bytes.
Confirm full-pipe cancellation does not strand a reader or a worker.
Confirm failed transport closure cannot appear as a successful media EOF.
Confirm expired or replaced grants cannot publish replacement job data.

## Primary references

- Reqwest 0.13.5 ClientBuilder: https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html
- Tokio owned tasks: https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinHandle.html
- Prepared source comparisons: Root's private `target/network-next-research/t14-preparation-01` packet.

The Context7 lookup reached its monthly quota during Root's verification.
Root retrieved the exact Reqwest version's primary documentation directly instead.
Cached source fingerprints identify inspected bytes and do not claim fresh upstream byte equality.

## Execution evidence — network reader unit

The unit remains uncommitted for Root integration.
Doctor Biz authorized implementation, parallel work, and routine design choices.
No publication, remote access, permission changes, or deletion ran.

- Initial policy RED: `target/network-reader/policy-red.log` records five expected failures.
- Initial transport RED: `target/network-reader/transport-red.log` records eight expected failures.
- Cancellation regression RED: `target/network-reader/cancellation-helper-red.log` proves standard helpers retried `Interrupted`.
- Address boundary RED: `target/network-reader/address-boundary-red.log` proves the initial special-prefix check overreached.
- R1 RED: `target/network-reader/redirect-r1-red.log` records three raw-redirect failures against reviewed production bytes.
- R1 fix validates raw references before normalization and preserves ordinary relative paths and query values.
- Focused GREEN: `target/network-reader/network-green-06.log` records four unit, seven policy, and seventeen transport tests.
- The independent reader process also passed within that transport suite.
- The cancellation fixture now waits for an actual server-ready signal instead of sleeping.
- Strict workspace Clippy passed in `target/network-reader/clippy-workspace-03.log`.
- Final formatting, catalog, and dependency checks passed in their `*-02.log` records.
- Final focused GREEN has no compiler warnings or errors.
- Dependency review verified 80 added metadata records and 127 exact license hashes.
- `target/network-reader/dependency-review.json` preserves that comparison.
- The independent review and hash receipt remain under `target/network-reader-review/round-01`.
- R1 was the review's sole material finding; its targeted closure receipt remains pending.

Canonical01 joined with exit 101.
Its result receipt lists 951 passes, 237 failures, and five existing ignores, including nested result lines.
The log and receipt remain under `target/network-reader/canonical-01*`.
Existing desktop, engine, and FFI fixtures failed after `/tmp` exhausted all 1048576 inodes.
Those fixtures hardcode `/tmp`, so task-local `TMPDIR` does not recover them.
One network cancellation fixture also raced request completion; the server-ready signal fixes that owned failure.
The initial freeze and failed canonical record remain intact.
A full green canonical run still requires Root-directed inode recovery.

Generated notices preserve six upstream trailing-whitespace lines.
The handwritten-source whitespace check passes when that generated file is excluded.
No upstream license bytes were rewritten.

Owner onboarding, live binding publication fences, streaming decoder integration, and T14 remain later Root-owned units.
