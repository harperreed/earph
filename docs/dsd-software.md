# DSD files and typed DoP

Earph provides a bounded software reader and transport codec for stereo DSD64 and DSD128.
This library unit supports DSF and uncompressed DFF.
The Linux `DopOutput` library submits typed carrier bytes to ALSA.
Engine playback and device transitions require later integration and qualification.

## Reader contract

Open a file with `earph_media::dsd::DsdFile::open(path, epochs, budget)`.
Read with `next_block(max_bytes_per_channel)`, using a limit from one through 4096.
Each `DecodedDsdFrame` owns a `DsdBlock`, sequence and source position.
Source positions count one-bit samples per channel.
The canonical payload interleaves channel bytes, with the oldest bit at bit seven.

DSF admission checks the fixed headers, declared size, stereo layout, rate and bit order.
It reverses valid LSB-first bytes and skips verified zero container padding.
It checks metadata placement and caps trailing tags at 64 KiB.
DFF admission walks the complete chunk tree, including trailing chunks and odd padding.
It rejects compressed DST, conflicting required properties and unsupported layouts.
The scan caps chunk count at 256 and aggregate nonaudio bytes at 1 MiB.
Both readers require nonzero source lengths divisible by sixteen bits per channel.

Seek with an aligned source-bit position and explicit epochs.
The exact end returns EOF.
A failed read or seek makes the reader terminal and closes its descriptor.
Cancellation closes the descriptor too.
Create a fresh typed transport boundary after seek or cancellation.
Keep source block ordering and epochs explicit in the consuming worker.

## Transport contract

Construct `earph_core::dop::DopPacker` with a `DopStream` and memory budget.
Its explicit boundary starts at sequence zero and marker `0x05`.
Push canonical `DsdBlock` fragments in source order.
The packer retains an incomplete frame across calls and alternates markers across emitted frames.
Call `finish()` at EOF; an unfinished frame is an error.
Errors, cancellation and processing requests retire the packer.

For stereo bytes `[0x12, 0x34, 0x56, 0x78]`, the first packed frame is `[0x56, 0x12, 0x05, 0x78, 0x34, 0x05]`.
Each carrier word contains exactly three little-endian bytes.
Carrier rates are 176400 and 352800 frames per second.
`DopBlock` keeps validated bytes, format, epochs, sequence and frame position private.
`DopUnpacker` checks stream identity, sequence, position and marker continuity before producing canonical source bytes.

The shared working allowance charges 24580 bytes for reader scratch, source, carrier and fixed remainder.
This allowance describes those owned buffers, rather than total process memory or caller-retained blocks.
The codec validates public `DsdBlock` fields on every push.

## Typed ALSA submission

On Linux, open `earph_alsa::DopOutput` with an `OpenRequest` and explicit `DopStream`.
The request must match stereo packed `Signed24` at the stream's carrier rate.
The source starts at an aligned bit position, sequence zero, and marker `0x05`.
The writer accepts `DopBlock` directly, without a `PcmBlock` or numeric conversion.

Each write requires granted signal permission and the current stream epochs.
The writer checks format, sequence, marker phase, position, period budget, and the offered span before native I/O.
Partial writes retain the current block and its exact bytes in the existing period scratch.
Retry only the unaccepted suffix with the same block.
Rejected work changes neither the accepted count nor the stream cursor.
Native errors close the handle and preserve the accepted prefix without recovery or replay.

Source positions count bits per channel.
`submitted_frames()` and the ALSA progress report count carrier frames.
Each carrier frame contains sixteen source bits per channel and six transport bytes.
`audio_storage_bytes()` charges the actual native period scratch separately from the codec's 24580-byte allowance.
No additional native owner, thread, or transport buffer is created.

`stop()` and Drop close without drain, PCM filler, or suffix replay.
The typed writer exposes no generic PCM suspend, drain, or silence operation.
Opening verifies exact ALSA parameter readback and leaves payload preservation `unverified`.
A file-to-null software route establishes no physical DSD capability.

## Safety and evidence

`DsdBlock` and `DopBlock` have separate types from `PcmBlock`.
Compile-fail examples also exercise the actual PCM graph boundary.
Software gain, exact zero, EQ, limiting and resampling requests refuse at the typed transport boundary.
No generic DSD silence, PCM filler or implicit conversion belongs to this codec.

The software tests generate independent DSF and DFF containers from known source bits.
Production readers and packers write carrier files, which an independent consumer reopens and verifies.
Coverage includes both rates, bit orders, fragmented reads, seeks, malformed files, budgets and descriptor cleanup.
The typed native tests use child-local ALSA configs with explicit file-to-null sinks.
The Engine library E2E follows `DsdFile` through `DopPacker` and `DopOutput` into actual ALSA capture.
An independent parser checks retained capture bytes, channel order, source suffixes, and marker continuity.
These tests do not start Engine playback or change its PCM-only observations.

Cursor unit tests cover zero and partial progress without claiming native short-write or `EAGAIN` evidence.
File-to-null may accept every offered span; caller-selected small spans do not establish native short writes.
The real `/dev/full` fixture covers terminal native error cleanup.
Stop-based captures prove accepted software bytes, without establishing safe physical EOF, pause, or mute.
The software pipeline establishes no DAC carriage, acoustic behavior or safe device transition.
PLAY-07 and SAFE-04 remain partial.
AC-13 requires actual qualified routes on both launch operating systems.

The [Linux codec receipt](evidence/dsd/software-linux-20261120.json) binds the earlier reader, codec, file pipeline and combined check.
That receipt predates the typed ALSA boundary and does not qualify it or a physical endpoint.
