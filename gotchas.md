# Gotchas

- On the tested macOS 27.2.1 host, registrations sharing one callback and a null client context collide with HAL `nope`.
  Keep a unique owned context for each registration, including silent probes.
  See [the Core Audio decision](docs/adr/0003-coreaudio-spike.md) and the real native ownership tests.

- On the tested Mac, changing a Unix read timeout after peer close fails despite unread buffered bytes.
  Use nonblocking I/O with one absolute polling deadline for each frame.
  See `frame_reader_drains_buffered_reply_after_sender_closes` and [the playback review](docs/local-playback-review.md).

- Symphonia 0.6.1 metadata options do not cap every embedded FLAC metadata reader.
  Keep the container preflight budget before probing files.
  See [local file admission](crates/earph-media/src/admission.rs).
