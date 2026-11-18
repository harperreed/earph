# Scoped hardware acceptance

Doctor Biz approved hardware playback and evidence for this Earph task on November 17, 2026.
The integration owner relayed that approval at 05:53 UTC on November 18.
This supersedes the earlier restriction to never-started Core Audio tests.

The approval covers a short owned generated signal through the previously selected explicit device UID.
The signal must use software attenuation of at least 40 dB and smooth ramps.
The acceptance uses the current float32 compatibility route, rate, and buffer size.
It exercises start, pause, resume, finish, stop, and close.
It records callback progress, SDK output timestamps, faults, and unchanged device settings.

The approval does not cover device-property changes, hardware gain changes, rate sweeps, or default-device selection.
It does not cover microphones, unrelated audio capture, private-media transfer, deletion, or global tool cleanup.
Raw UIDs, device names, and native reports remain in the approved private task folder.
Public evidence must exclude those identities and include source and build provenance.

SDK output timestamps and callback delivery do not prove DAC preservation or acoustic presentation.
Hardware drain and pause continuity remain unknown unless the collected evidence establishes them separately.

The test is ignored by every ordinary workspace check.
It also requires three explicit environment values.
The request file must contain the saved `device_uid` from the prior private inspection.
The report path must be new and remain in a private folder.
The test creates raw and `.public.json` reports with mode 0600.

```sh
EARPH_COREAUDIO_PLAYBACK_APPROVAL=granted \
EARPH_COREAUDIO_PLAYBACK_REQUEST=/absolute/private/request.json \
EARPH_COREAUDIO_PLAYBACK_REPORT=/absolute/private/run-ID.json \
cargo test --locked -p earph-coreaudio --test hardware_playback \
  owned_signal_float32_lifecycle_acceptance -- --ignored --exact --nocapture
```

The generated signed24 signal peaks at 32768, at least 48.16 dB below full scale.
Two ramped 440 Hz tone windows surround a prepared silent pause interval.
The test reads the current device clock only while its owned IOProc runs.
It does not capture audio.
It closes the output after observing two callbacks beyond application-ring drain.
Those extra callbacks do not establish a hardware drain or presentation guarantee.

## Verified lifecycle run

The approved native run passed on November 18, 2026.
It used the saved explicit UID at the observed 48000 Hz stereo rate and 512-frame buffer.
It delivered all 36352 generated frames through 73 callbacks with no fault.
Callback delivery and callback count held during the 100 ms pause.
Start, pause, resume, finish, stop, and close completed successfully.
The inspected settings stayed unchanged.
The SDK supplied valid output timestamps and current device sample time.
No audio capture occurred.

The [redacted receipt](evidence/hardware-lifecycle-20261118.json) includes the stimulus hash and build provenance.
That receipt records the exact source digest and truthfully marks the tested build dirty.
The raw report and native command log remain in the approved private Mac task folder.

The clock observations did not establish hardware drain or DAC preservation.
The device sample clock still preceded the final payload buffer end after two following callbacks.
Immediately after start and resume, its sample epoch also differed from the later IOProc epoch.
These observations forbid treating callback equality or a fixed callback count as presentation proof.
