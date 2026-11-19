# Gotchas

- Use `runtime::OwnerLock` for every `owner.lock` writer.
  A concurrent child inherits the open file description before exec closes it.
  Explicit unlock must precede close, or a retired owner can leave its replacement busy.
  The real pre-exec child regression covers replacement and continued exclusivity on Linux and macOS.

- Serde can emit an f32 as a short decimal directly, then an expanded decimal through a JSON value.
  Compare declared f32 fields at f32 precision when checking CLI and IPC observations.
  Keep f64 fields, integer counters, identities, and fingerprints exact.
  See the [Linux SRC evidence](docs/evidence/dsp/linux-src-native-release-20261118.json).

- GitHub inline threads omit findings embedded in review summaries.
  Fetch review bodies and PR comments alongside threads before claiming all feedback is handled.
  The second graph review included four findings outside its sixteen inline threads.

- The build-input digest sorts Rust path components, rather than whole path strings.
  Match that order when recomputing it in another language; `output/` sorts before `output.rs`.
  Keep the u64 little-endian length framing in [build provenance](crates/earph-harness/build.rs).

- On the tested macOS 27.2.1 host, registrations sharing one callback and a null client context collide with HAL `nope`.
  Keep a unique owned context for each registration, including silent probes.
  See [the Core Audio decision](docs/adr/0003-coreaudio-spike.md) and the real native ownership tests.

- On the tested Mac, changing a Unix read timeout after peer close fails despite unread buffered bytes.
  Use nonblocking I/O with one absolute polling deadline for each frame.
  See `frame_reader_drains_buffered_reply_after_sender_closes` and [the playback review](docs/local-playback-review.md).

- Symphonia 0.6.1 metadata options do not cap every embedded FLAC metadata reader.
  Keep the container preflight budget before probing files.
  See [local file admission](crates/earph-media/src/admission.rs).

- ALSA 1.2.15.3 can reinstall hardware end silence on each nonblocking drain call.
  After native `DRAINING` begins, observe `SETUP` instead of restarting drain or polling ordinary I/O readiness.
  Plugins may need commit work while still prepared or running.
  See [the adapter](crates/earph-alsa/src/output.rs), [native state observation](crates/earph-alsa/src/native.rs), and [the scoped trace](docs/evidence/local-playback/alsa-drain-trace-20261118.json).

- JNA's default loader can clean earlier JNA temporary files before loading its dispatcher.
  The first FFI gate has no before-state receipt proving whether files existed or were removed.
  Use [the private-dispatcher gate](docs/ffi-bindings.md) with child-only no-unpack, no-classpath, and no-system settings.
  Verify the loaded dispatcher path and bytes, and preserve all task artifacts.
  See the [foundation receipt](docs/evidence/ffi/foundation-20261118.json) for the unresolved historical limit.

- AT-SPI root cache clearing follows cached child links and can miss separately queried descendants.
  Refresh the exact failed Text accessible or counted parent, then restart the complete read.
  Keep mutations outside retries and preserve deadlines and process identity checks.
  See the [native desktop checkpoint](docs/superpowers/plans/2026-11-18-desktop-connect-queue.md).
