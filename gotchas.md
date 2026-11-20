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
  Keep the u64 little-endian length framing in [build provenance](build_support/provenance.rs).
  The harness and Engine have separate declared input sets in that shared helper.
  Update source-hash tools to read `HARNESS_INPUTS` there; preserve earlier tools with their original source evidence.

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

- Keep writable executable descriptors outside a multithreaded fixture parent's inheritance table.
  Concurrent children can inherit them before exec, even with CLOEXEC, and cause ETXTBSY.
  Publish fixture executables in an isolated child and wait for its exit.
  See [the fixture publisher](crates/earph-engine/tests/support/device_process.rs) and [Linux exec rules](https://man7.org/linux/man-pages/man2/execve.2.html).

- Give live egui request bodies stable request-derived scope IDs.
  Local ID salts do not stabilize every descendant when earlier controls disappear.
  Keep reconciliation disabled during delivery; removing it can prevent complete accessibility reads.
  See [receipt rendering](crates/earph-desktop/src/gui/receipts.rs).

- Fence a read refresh against its fresh Before state, rather than an earlier captured control revision.
  Exact reconciliation can advance that revision before an explicit refresh starts.
  Keep the initial owner and zone checks, full Before/After fence, and complete mutation preflight.
  See [the real-owner regression](crates/earph-desktop/tests/gui_profiles.rs).

- AccessKit TextRun values can repeat their parent label's text.
  Select the semantic node before checking a field's label relationship.
  Reuse the [widget helper](crates/earph-desktop/tests/gui_widgets.rs); tree iteration order does not establish a node's role.

- Successful GUI diagnostic reads retire after adoption into current typed views.
  Inspect those views instead of requiring receipt-history rows.
  Preview commands change candidate authority; replay after receipt eviction can capture again.
  Observer tools must never replay previews.
  See the [diagnostic workflow](docs/diagnostics.md).

- On `ENOSPC`, check `df -i /tmp` alongside free byte space.
  tmpfs charges hard links against its inode limit, so they do not reduce reported inode use.
  Keep large scan fixtures in ignored task storage, outside shared temporary storage.
  Exhausted temporary inodes can break owner, decoder, and foreign-client checks together.
  Preserve evidence and get deletion approval before reclaiming finished fixtures.
  See [tmpfs link accounting](https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/2b66f2cc753243a6adb3e9a5c038b0fc59ba7ec9/mm/shmem.c).

- Engine publishes `endpoint.json` before verifying its discovery helper and entering the IPC loop.
  Treat that file as discovery identity, rather than serving readiness.
  Wait for the owner startup announcement, then require a State reply with its owner identity.
  Reuse the bounded [FFI startup reader](scripts/ffi-owner-readiness.mjs) and the desktop launch handshake.

- Serialize full suites and native owner qualification on a shared build host.
  Two overlapping suites exceeded existing owner-startup and streaming deadlines; unchanged isolated scenarios then passed.
  An unchanged full UPnP check also passed alone.
  Preserve failures and diagnose timing before changing application behavior or deadlines.
  These observations support load sensitivity; they do not establish a root cause.
  See the [Linux wire receipt](docs/evidence/network/upnp-wire-linux-20261120.json).
