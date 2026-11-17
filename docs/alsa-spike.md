# Linux ALSA spike

T05 adds explicit Linux output outside the shared core.
It does not complete M0, M1, or hardware playback qualification.
See [the decision](adr/0002-alsa-output.md) and [the implementation plan](superpowers/plans/2026-11-17-alsa-spike.md).

## Commands

Install the host ALSA development package and `pkg-config` before building on Linux.
Use the repository's pinned Rust toolchain.

List playback endpoints without opening a PCM stream:

```sh
cargo run --locked -p earph-alsa -- devices
```

Hints are advertised capabilities, not verified formats.
The command may query ALSA control devices for their names.

Open the example discard route, inspect its exact format, and close without submitting samples:

```sh
cargo run --locked -p earph-alsa -- open --request docs/alsa-request.example.json
```

The example selects `null` deliberately.
Create a separate request when selecting a real endpoint from discovery.
Set `route` to `direct_hardware` for a qualified `hw:` path, or `system` for a compatibility path.
The request must name one endpoint and the exact format and buffer settings.
The adapter never falls back or terminates another audio service.

To exercise a fixture, obtain approval for signal emission on the selected route first.
The following example uses the discard route and sends nothing to speakers:

```sh
cargo run --locked -p earph-alsa -- exercise \
  --request docs/alsa-request.example.json \
  --fixture target/fixtures/signed24-stereo-48000-boundary.json \
  --allow-signal
```

Generate the fixture bundle with the existing harness command from the main README.
Boundary fixtures contain full-scale values; never select physical output casually.
The spike has no integrated listening-volume policy.
Exercises reject format mismatches before opening and limit inputs to 20 seconds.
Transfer and consumption waits share a deadline of fixture duration plus two seconds.
Native discovery, opening, and configuration remain control-thread operations.

Commands return JSON and exit with status 2 on failure.
Failures after opening preserve the negotiated report, submitted count, and final state.
`DEVICE_UNAVAILABLE` includes the native errno for access, missing, or busy endpoints.
`FORMAT_UNSUPPORTED` rejects unsupported exact parameters without conversion.
`FIDELITY_POLICY_CONFLICT` rejects direct routes that actually open plugins.
`SAFETY_POLICY_BLOCKED` means no signal permission was supplied.

## State and evidence

The adapter owns `prepared`, `running`, `stopped`, and `failed` states.
Denied permission, invalid spans, stale epochs, and format mismatches submit no samples.
Accepted writes advance only the accepted frames.
`would_block` leaves the span pending for the caller.
Fatal backend errors stop and close the stream.
Stopped or failed streams require a new explicit open.

Reports separate the requested format from installed ALSA client parameters.
They include the PCM type, hardware identity when available, runtime version, and build provenance.
An open report establishes only the `opened` evidence level.
Exercise reports establish `exercised` at the ALSA client boundary.
Only an independent capture can establish measured hardware preservation.
The file-plugin matrix measures bytes at that plugin, not at a DAC.

## Verification

Run the canonical check:

```sh
scripts/check
```

The real plugin matrix covers 72 fixtures at block sizes 1, 64, and 256.
It compares all 216 captured payloads against their real fixture files.
Tests also cover real write failures, missing devices, conversion plugins, masquerading aliases, and owned cleanup.
They exercise caller-selected partial spans, but do not force native short acceptance or `EAGAIN`.
Those backpressure branches remain a backend qualification gap.
Expected native error messages are captured and asserted in child test processes.
Tests retain owned artifacts under ignored `target/alsa-tests` directories.

Reproduce the matrix and print its retained directory:

```sh
cargo test --locked -p earph-alsa --test cli \
  matrix_captures_all_fixture_formats_with_real_alsa_plugins -- --exact --nocapture
```

Recheck a saved matrix without opening any audio stream:

```sh
cargo run --locked -p earph-alsa -- matrix-check --record PATH/matrix.json
```

The check reads raw payloads, captures, requests, configuration hashes, and reports.
It checks complete case coverage, build consistency, provenance, and the plugin measurement boundary.
It rejects corrupt captures, escaping artifact paths, and claims of physical or PipeWire qualification.
It does not authenticate the author or independently measure a DAC.

The optional hardware ownership test requires one explicitly selected subdevice.
It opens silently, verifies `EBUSY` on a second open, closes, and reopens.
Set `EARPH_ALSA_TEST_DEVICE` only for this test:

```sh
EARPH_ALSA_TEST_DEVICE='hw:CARD=SELECTED,DEV=0,SUBDEV=0' \
  cargo test --locked -p earph-alsa --test hardware -- --ignored --nocapture
```

Replace `SELECTED` with the actual card ID before running.
The default suite leaves this host-specific test ignored.

## Observed host and remaining gates

Silent opening passed on the host's ALC700 Analog hardware and PipeWire route.
Both installed 48 kHz, stereo, signed 16-bit PCM with 256-frame periods and a 1024-frame buffer.
Direct ownership rejected the second subdevice open with `EBUSY`, then reopened after closing.
These checks submitted zero samples.
The runtime reported Linux x86_64, kernel 7.0.0-38-generic, and ALSA 1.2.15.3.

Physical playback, unplug, hardware payload capture, server-rate changes, coexistence, and endurance remain unqualified.
Core Audio remains T04; decoding and ownership remain T06–T07.
Volume policy and persistent device evidence remain T08.
These limits keep AC-21 partial and AC-22/AC-23 unqualified.
