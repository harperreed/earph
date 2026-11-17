# ALSA spike evidence

Source commit: `ae10b7b3147b71cf809202458bc2994367cca86c`.
All records identify that clean build.
Recorded on November 17, 2026, using Linux x86_64 and ALSA 1.2.15.3.

`matrix.json` records 216 real ALSA file-plugin captures from 72 deterministic fixtures.
The matrix uses block sizes 1, 64, and 256.
Each case retains its request, relative ALSA configuration, raw capture, and executable report.
The fixture directory retains every original payload and manifest.
The matrix measures preservation at the ALSA file plugin only.
It does not measure a physical output.

`host-direct.json` records silent opening of ALC700 Analog through `hw:CARD=PCH,DEV=0`.
`host-system.json` records silent opening through the PipeWire plugin.
Both installed 48 kHz, stereo, signed 16-bit PCM with the requested period and buffer.
Both submitted zero samples, reported zero backend delay, and closed successfully.

`hardware-ownership.json` records a separate silent test of the explicit hardware subdevice.
The test observed `EBUSY` on a second open, then closed and reopened the first stream.
It submitted zero samples.

Recheck the matrix from the repository root:

```sh
cargo run --locked -p earph-alsa -- matrix-check --record docs/evidence/alsa/matrix.json
```

Verify the top-level records from this directory:

```sh
sha256sum --check checksums.sha256
```

The matrix validator checks each raw artifact and its scoped report.
Checksums detect changes; they do not authenticate the author.
The [review](../../alsa-review.md) records checks and the independent review.
The [spike guide](../../alsa-spike.md) explains reproduction and command safety.

Native short acceptance and `EAGAIN` were not forced in these tests.
Physical playback, unplug, hardware payload measurement, endurance, and PipeWire coexistence remain unqualified.
AC-21 remains partial; AC-22 and AC-23 remain unqualified.
