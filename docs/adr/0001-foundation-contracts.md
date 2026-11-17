# Foundation contracts

Status: accepted for T01–T03 implementation.

Use two Rust crates. Keep domain and session contracts in `earph-core`.
Keep filesystem access, Cargo inspection, and virtual output in `earph-harness`.
Do not expose adapter types through core interfaces.

Use schema version 1. Reject unsupported versions and required features.
No older product schema exists. Preserve rejected input files unchanged.

Implementation states are `not_started`, `in_progress`, and `implemented`.
Verification and scenario states are `not_run`, `partial`, `pass`, `fail`, and `blocked`.
Partial coverage never qualifies an entire scenario.

Begin sessions muted with −40 dB software-volume intent.
This default does not establish a safe acoustic level.
Do not implement fixed-output arming or hardware volume in the offline foundation.

Use a 256 MiB default audio-buffer budget.
Reject overflow before allocating buffers.
Keep control messages within 1 MiB and catalogs within 2 MiB.
Retain 1024 receipts and 1024 state events per zone.
Keep progress event sequences separate from mutation revisions.
Preserve paused intent when a second seek replaces a pending seek.
Deduplication applies only while a receipt remains retained.
Reject changed payloads reusing a retained request ID.
Report event gaps and include a fresh snapshot.

Record exact dependency versions, features, manifest hashes, and license hashes.
Retain actual dependency license texts in `THIRD_PARTY_NOTICES.txt`.
Generate the inventory with `earph-harness dependency-lock` after intentional dependency changes.
Review the diff before accepting it.
Unselected adapters remain unsupported and unqualified.

Linux GUI, operational storage, and IPC framing remain separate M0 decisions.
Hardware tests need an explicit signal-emission approval and actual device evidence.

Admit raw fixture bytes and typed sample storage as a combined peak.
Read regular files into exact-sized buffers to avoid geometric capacity growth.
Treat a changing input length as an error.

Embed build provenance during compilation.
Include source revision, source digest, lock hash, compiler, target, and profile.
Watch source inputs and Git references when refreshing build metadata.
Keep execution-time checkout observations separate.
Do not relabel an older binary with a newer checkout's identity.
