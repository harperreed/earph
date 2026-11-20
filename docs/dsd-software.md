# DSD files and typed DoP

Earph provides a bounded software reader and transport codec for stereo DSD64 and DSD128.
This library unit supports DSF and uncompressed DFF.
Engine playback, native output and device transitions require later integration and qualification.

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

## Safety and evidence

`DsdBlock` and `DopBlock` have separate types from `PcmBlock`.
Compile-fail examples also exercise the actual PCM graph boundary.
Software gain, exact zero, EQ, limiting and resampling requests refuse at the typed transport boundary.
No generic DSD silence, PCM filler or implicit conversion belongs to this codec.

The software tests generate independent DSF and DFF containers from known source bits.
Production readers and packers write carrier files, which an independent consumer reopens and verifies.
Coverage includes both rates, bit orders, fragmented reads, seeks, malformed files, budgets and descriptor cleanup.
The software pipeline establishes no DAC carriage, acoustic behavior or safe device transition.
PLAY-07 and SAFE-04 remain partial.
AC-13 requires actual qualified routes on both launch operating systems.

The [Linux software receipt](evidence/dsd/software-linux-20261120.json) binds the reviewed codec, real file pipeline and combined check.
