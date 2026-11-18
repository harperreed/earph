# Graph timing observations

Read `timing_kind` before using a graph's timing fields.
Graph previews and playback processing observations use explicit timing variants.

`same_rate` retains the existing `latency_frames` and `tail_frames` numeric fields.
Those counts use one shared processing rate.
Previews also retain `tail_policy`, `terminal_output_bounds`, and `node_tails` at their existing JSON locations.
The new tag adds context without changing those values.
Filter scheduling latency still does not establish an exact audible source position.

`streaming` exposes the shared `stages` array.
It does not publish aggregate latency, tail, terminal bounds, tail policy, or node tails.
Each stage names its original graph nodes and its input and output formats.
Its peak bounds and kernel facts apply in those declared rate domains.
Same-rate islands retain their local latency and emitted tail facts.
SRC stages retain explicit guard, padding, parameter, fingerprint, and bounded finish facts.
An optional truncated library delay describes an instantiation observation, not exact group delay.
Never sum frame counts across different rate domains as an exact source latency.

`GraphTiming` is the preview's Rust timing type.
Its scalar latency and tail helpers return `None` for streaming timing.
`PlaybackTiming` keeps same-rate progress compact.
`GraphTiming::playback_timing()` shares the same immutable stage array for streaming progress.
It does not clone stage vectors on each progress update.

Preview validation checks bounded metadata, finite per-channel facts, endpoint formats, and adjacent stage contracts.
It checks SRC layout and rate facts against each stage's declared formats.
It checks that downstream peak admission covers the preceding stage's output bound.
It rejects intermediate bounded-IIR islands and invalid terminal bounds.
It does not replace compiler topology, numerical proofs, or resource admission.

This DTO change does not install a live SRC graph path.
The existing worker preserves same-rate rewind and filter position behavior.
Streaming timing has no scalar source projection.
The old source-rewind path rejects that timing until the parent installs the streaming playback path.
Offline graph capture remains available through `render-graph`.

`graph.validate` now compiles actual serial SRC cuts and exposes their streaming timing facts.
Validation creates no prepared token or runtime pipeline and leaves playback unchanged.
Live SRC `graph.prepare` remains explicitly unsupported until its native worker path is installed.

Backend observations include actual Rust-owned adapter `audio_storage_bytes`.
Graph resource inspection separates those resident bytes from `backend_audio_peak_bytes`.
Core Audio's reopen peak includes old and replacement rings together.
ALSA retains one packing scratch.
These counts exclude metadata, allocator overhead, RSS, and opaque SDK or kernel storage.
Storage affects resource admission rather than graph identity.
