# Streaming FLAC

`earph-media::StreamingFlac` decodes raw FLAC from a nonseekable worker-side reader.
It qualifies signed 16-bit and signed 24-bit PCM with mono or stereo channels.
It rejects streaming 32-bit FLAC until a genuine producer passes exact-sample qualification.
`LocalFile` keeps its existing 32-bit support and public API.

```rust,ignore
let mut stream = StreamingFlac::open(reader)?;
let format = stream.format();
let declared_frames = stream.total_frames();
while let Some(frame) = stream.next_frame()? {
    let position = frame.frame_position();
    let sequence = frame.sequence();
    let frames = frame.frame_count();
    let samples = frame.into_samples();
    // The engine must validate its source binding before publishing these samples.
}
```

`DecodedPcmFrame` owns checked integer samples and exposes read-only span getters.
It carries no epochs, queue identity or time domain.
The engine must assign those facts after its own publication fence.
The media crate has no production dependency on network types.
Its network development dependency exercises real admitted HTTP readers.

`total_frames()` preserves STREAMINFO's optional count.
An unknown count stays `None`, including after observed EOF.
The reader exposes no seek operation.
Any failed decode remains failed on every later call.

## Admission and byte accounting

Admission requires raw `fLaC` and first STREAMINFO of exactly 34 bytes.
It rejects duplicate STREAMINFO and reserved metadata type 127.
It checks these limits before reading or allocating each block body:

| Input | Limit |
| --- | ---: |
| Metadata blocks | 256 |
| Individual metadata body | 65,536 bytes |
| Complete metadata prefix, including headers | 1,048,576 bytes |
| Symphonia media source buffer | 65,536 bytes |
| Retained raw audio mirror | 2,097,152 bytes |
| Live audio read allowance per public demux call | 2,097,152 bytes |
| Accepted packet | 1,048,576 bytes |
| Decoded packet | 65,536 frames |

The admitted prefix replays once through the direct pinned `FlacReader` constructor.
Only subsequent live audio reads enter the fixed mirror.
Constructor read-ahead uses the initial allowance and remains in the mirror.
Later public calls reset the read allowance without clearing unmatched bytes.
The source guard releases its mutex before calling the underlying reader.

The guard permanently retains the first observed source error or input budget failure.
The wrapper checks that guard after constructor and packet results, including library errors and apparent EOF.
Each returned packet must match the next raw mirror prefix exactly.
The wrapper never searches forward to hide discarded bytes.
Accepted packets require the expected track, zero trimming and continuous PTS.
Decoded duration must match packet duration.

The pinned Symphonia 0.6.1 decoder does not check consumed frame extent itself.
The wrapper first decodes a borrowed packet slice missing its final three bytes.
That attempt must fail with an I/O error.
It then decodes the slice missing its final two CRC bytes.
That attempt must succeed before the wrapper accepts PCM.
The first failed attempt does not update the pinned decoder's PCM MD5.
Both attempts use one decoder and borrow existing packet storage.

At EOF, the wrapper requires an observed source EOF and an empty mirror.
It also requires the exact declared count when available.
It enables available PCM MD5 verification and rejects a negative final result.
Absent MD5 remains unavailable verification.
Shared conversion retains local integer handling, DoP rejection and seek discard rules.

## Qualification evidence

The focused tests generate owned WAV signals and encode genuine FLAC with FFmpeg.
They check every returned integer, position, sequence and frame count.
The matrix covers 16/24-bit mono/stereo, constant, verbatim, fixed and LPC signals.
It independently varies known/unknown total, present/absent MD5 and source read fragmentation.
Additional forced left/side fixtures exercise padding and seven-frame final packets.

Both CRC-valid adversaries pass the ordinary decoder and fail before this wrapper exposes PCM.
Tests also reject CRC damage, partial tails, discarded bytes, excessive metadata and exhausted input budgets.
Unit tests independently exhaust the mirror and read allowance and verify persistent failures.
The EOF regression reports clean end when its mirror check is removed.
Restoring that check makes the regression pass.

The large genuine 24-bit stereo fixture contains 400,001 frames and 2,437,462 raw audio bytes.
Its largest observed packet is 399,399 bytes.
Exact sample checks span the fixed ring's wrap boundary.
At the metadata input limit, the observed prefix vector capacity is 1,049,312 bytes.
The raw mirror's boxed storage remains exactly 2,097,152 bytes.
These are measured capacities for this pinned toolchain and fixture, not general heap bounds.

Real HTTP tests cover Content-Length, chunked bodies, slow consumers, transport truncation and a partial media tail.
They also cover body stalls and cancellation using the admitted network reader.
Every test retires its owned worker and joins its server thread.
The tests activate no renderer or physical output.

## Limits and later integration

The mirror bounds retained raw input, not every upstream allocation or CPU cost.
Symphonia also allocates parser scan buffers, fragments, merge buffers, metadata objects and decoder PCM.
Nested metadata and parser paths have their own allocation behavior.
Input caps and post-parse packet limits do not establish a complete allocator budget.
The capacity observations above do not measure those private allocations.
The two decoding attempts also need separate CPU qualification.

A generic reader exposes only errors that a read actually observes.
Buffered PCM can remain decodable before the next read observes transport cancellation or failure.
This wrapper cannot provide the later engine's live transport publication fence.
The existing network reader has no cloneable observation handle beyond its reader accessors.
Engine integration must resolve that ownership contract and fence publication against fresh binding and terminal state.
At media EOF, that integration must also require the same HTTP source's successful transport completion.

An unknown-count stream without MD5 may end cleanly at any valid final frame boundary.
The wrapper cannot prove missing original frames after that boundary.
HTTP completion proves the response body completed according to HTTP framing.
It does not prove original-recording completeness or content identity.
This unit does not qualify network gapless playback, Music Assistant interoperability or physical output.

The scoped test logs and numeric exits remain under `target/streaming-flac` in the task worktree.
The combined canonical check and independent review remain Root integration gates until their actual results exist.
