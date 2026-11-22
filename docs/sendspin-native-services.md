# Sendspin native credential services

The Engine exposes a dormant credential supervisor for roleless Sendspin.
Callers must construct it explicitly.
Application startup does not select a credential store or open a connection.

`StoreConfiguration` requires the helper executable, native configuration, stable private lock path, and operation timeout.
`CredentialSupervisor::spawn` retains one supervisor thread.
Opening, loading, committing, recovery reads, and random draws run on that thread.
Future polling and dropping never join it.

`load` requires an existing, definitively committed credential document.
Missing returns `BridgeError::Store(StoreError::Missing)`.
No failure generates an identity or permanent pairing PSK.
The session receives the loaded identity, pairing PSK, and bounded catalog.
The supervisor retains only the immutable identity envelope beside its native store.
Each later connection reloads storage.
The reducer owns the only mutable catalog planner.

Each handoff carries one connection-specific services lease.
One connection lease and one operation reservation may exist at a time.
An explicit admission policy separately grants idle and pairing activities.
The default policy denies both.
Every role and other activity is refused.
A connection permit grants no admission or output authority.
One shared completion fence checks successful cancellable values at delivery.
It checks cancellation and retirement for load, admission, candidate, and recovery.
It also checks policy expiry and revocation for admission and candidates.
A computed value stays subject to that fence until its receiver consumes it.
Delivered decisions cannot retract bytes already sent.
The caller owns Network cancellation when ending a live connection.

Each candidate requests fresh system randomness through `getrandom` 0.3.4.
Cancellation is checked before and after that draw.
The operating system randomness call itself cannot be interrupted by this API.
A stalled call can outlive bounded retirement.
Final supervisor destruction joins retained work and may wait without a deadline.
No randomness fallback or static identity generator supplies candidate PSKs.

A commit owns the whole replacement catalog and preserves the loaded identity envelope exactly.
Only exact fresh native encoded-byte readback reports success.
Definitive failures before mutation report `Unchanged` only when no uncertainty remains.
Pending native work, lost accepted completion, or ambiguous results report `Uncertain`.
Dropping the commit future cannot cancel native work.
The owner retains the original attempt and its terminal result.

Retire Network first so its original session consumes acknowledged commit completion.
Then retire the credential supervisor.
A retirement timeout retains its original thread and native lock.
Retry retirement on that same owner after work settles.
Final destruction joins retained work.

A recovery read reports only a fixed diagnostic class.
It never returns recovered secrets to callers or clears native or bridge uncertainty.
No automatic native completion acknowledgment or Network recovery acknowledgment exists here.
Helper exit and a successful read cannot establish daemon mutation completion.
An absent native operation ID also cannot clear bridge uncertainty.

The stable lock serializes cooperating Earph owners using the same path.
It cannot exclude unrelated external native writers and provides no compare-and-swap guarantee.
The command-line player provisions an identity with `earph sendspin init`; it writes through `CredentialStore` only after a definite `Missing`.
Startup integration, explicit recovery acknowledgment, clocks, playback, and output remain separate work.
Music Assistant and macOS native qualification remain explicit release gates.

The process fixture tests use synthetic helper responses.
They are integration evidence, not real native storage evidence.
Actual native qualification must use the reviewed isolated operator and pinned authenticated transport peer.
Preserve its private fixtures, process receipts, source hashes, and exact binary hashes.
