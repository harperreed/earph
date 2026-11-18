# Local engine owner

The owner controls one private runtime directory.
It owns the shared `Session`, queue, revisions, receipts, and operational checkpoint.
It exposes local playback through the host's available native adapter.
Output selection grants no signal permission and does not open a device.
See [playback integration](PLAYBACK.md) for consent, gain, transport, and live queue rules.
Adapter availability does not establish physical-route qualification or DAC preservation.

Run the owner in the foreground:

```sh
cargo run --locked -p earph-engine -- owner --runtime /tmp/earph-private
```

Choose an absolute runtime path whose resolved form contains at most 60 bytes.
Its parent must already exist.
The owner creates a new runtime directory with mode `0700`.
It rejects an existing directory unless the current user owns it with mode `0700`.
It also rejects writable ancestors that permit another user to replace the directory.
Root-owned sticky directories, including the usual `/tmp`, are allowed.
It never changes permissions on existing directories.
The owner pins the validated resolved path before creating its lock, socket, or checkpoint.
Changing a directory alias cannot redirect an existing owner's writes.

A second invocation queries the existing owner and exits.
It does not start a competing owner for that runtime.
All controllers for one engine must use the same runtime directory.

Use `earph-engine import-fir --runtime PRIVATE_DIRECTORY correction.wav` to store a qualified correction WAV.
The command returns the immutable asset reference used by a convolution graph node.
It preserves source files and rejects conflicting existing objects.
See [correction filters](../../docs/correction-filters.md) for supported formats, resource bounds, and tail policies.

Send one JSON command line through the client:

```sh
printf '%s\n' '{"schema_version":1,"request_id":"inspect-1","zone_id":"main","owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":"state.get"}}' |
  cargo run --locked -p earph-engine -- request --runtime /tmp/earph-private
```

The client prints the exact JSON reply.
Exit status `0` means accepted or applied.
Exit status `1` means a remote command or protocol failure.
Exit status `2` means invalid CLI arguments.
Exit status `3` means a local transport, input, output, or operational failure.
Accepted does not mean audible playback occurred.

Read `state.get` to obtain the current owner ID and revision.
Include both values in mutating command envelopes.
Queue commands do not require an active playback session.
The current queue methods are `queue.get`, `queue.append`, `queue.append_directory`, `queue.remove`, `queue.reorder`, and `queue.select`.
Use `owner.shutdown` for an orderly stop.
These commands use the same core definitions as embedded controllers.
Output methods are `output.select` and `output.consent`.
Volume methods are `volume.set`, `volume.caps.set`, and `volume.arm_fixed`.
Local playback uses the existing `sessions.start`, pause, resume, seek, stop, and mute methods.

`queue.get` requires `offset` and `limit`.
The limit must be between 1 and 64.
The response contains entries, selected identity, offset, and total entry count.
Its receipt identifies the revision.
If revisions change between pages, start the read again.
Pagination bounds replies even when valid paths require extensive JSON escaping.

`queue.append_directory` reads one directory without recursion or symlink traversal.
It scans at most 4096 entries and admits at most 256 matching regular files.
It accepts WAV, FLAC, AIFF, and AIF extensions without regard to ASCII case.
It sorts paths before appending them in one transaction.
A scan error, invalid path, or capacity failure rejects the whole append.
Import does not prove that a file can decode.
Entries remain unprepared until a source adapter prepares them.

## IPC and recovery

The transport uses Unix stream sockets on Linux and macOS.
Each connection carries one command frame and one reply frame.
A frame starts with a four-byte unsigned big-endian byte count.
The payload contains UTF-8 JSON and cannot exceed 1 MiB.
Connect, frame read, and frame write each have a one-second deadline.
Trickling bytes cannot extend a frame deadline.
The owner parses connections serially on a separate bounded IPC thread.
Playback and control observations continue while a client frame is incomplete.
It never listens on a TCP interface.

The directory restricts access to its user.
Operational files and sockets have mode `0600`.
A persistent `owner.lock` file uses `File::try_lock` for exclusive ownership.
The OS releases its advisory lock when the process exits.
The lock file remains in place.
See the [Rust file-lock contract](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).

Each owner binds a fresh socket name and atomically publishes `endpoint.json`.
It preserves previous socket nodes and abandoned temporary files.
It does not remove files during startup or shutdown.
Repeated crashes can leave artifacts that need a separately authorized cleanup.

A versioned `queue.json` checkpoint contains queue identities, paths, selection, counter, revision, output selection, and independent volume caps.
The file cannot exceed 2 MiB.
Restoration validates the schema, paths, capacity, identities, counter, and selection.
It resets preparation results and restores paused, muted state without opening a source or output.
Restoration drops signal consent and fixed-output arming.
A malformed checkpoint stops startup and remains available for inspection.

Mutations stage a copy of the authoritative session.
The owner writes and syncs a private temporary checkpoint before atomic rename and directory sync.
It records pending and settled recovery states around checkpoint replacement.
Startup reconciles the recovery record before restoring graph intent.
Only a successful durable commit publishes the staged session and an applied receipt.
A storage failure returns a failed receipt and stops the owner.
The persisted outcome can be uncertain if the final directory sync fails.
Reinspect the checkpoint after restarting before issuing another mutation.
See [checkpoint recovery](../../docs/operational-storage.md) for the bounded uncertainty contract.

The session retains at most 1024 command receipts in memory.
Use a fresh request ID for each fresh read.
Reusing an ID replays its retained reply, including query results.
This window does not survive owner restart.
Each restart exposes a fresh owner ID and rejects prior-owner mutations.
Compare revisions only within the owner ID that produced them.
After a crash, transient event revisions may be newer than the last durable checkpoint.
Clients reconcile an uncertain result through fresh state and queue queries.
The service does not promise durable exactly-once execution across crashes.

The automated tests exercise Linux and native macOS IPC and executable crash recovery.
Framed reads drain buffered replies even after the sending owner closes its connection.
Physical-route qualification remains separate from generated file-to-null acceptance.
