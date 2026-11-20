# Sendspin protocol framing

This crate implements the player framing contract pinned by `SPEC_REVISION` in `src/lib.rs`.
It adds no transport or cryptography dependency.
The application does not advertise or start Sendspin from this crate.

Call `decode_player_audio` only after authenticating and reassembling one ID-4 plaintext message.
Pass the admitted byte limit, including the thirteen-byte header.
The result borrows its payload and preserves signed server microseconds and unsigned send-ahead microseconds.
Saturated send-ahead values remain observations; they do not define playback deadlines.
Debug output exposes the payload length, rather than audio bytes.

The parser supports the pinned thirteen-byte layout only.
It does not infer a legacy protocol version from arbitrary payload bytes.
The future authenticated dispatcher must enforce the version lock.

The binary fixture comes from actual aiosendspin 10.2.1 producer calls.
Its provenance records the exact upstream commit, producer source hash, interpreter, and fixture hash.
The installed producer matched the inspected pinned source byte-for-byte.
No upstream source code is copied into this crate.

This slice does not implement pairing, encrypted sessions, discovery, arbitration, decoding, scheduling, or native output.
Actual Music Assistant interoperability and independent-clock capture remain separate required gates.
Doctor Biz approved the pinned specification licence for this implementation.
The exact terms are retained at the repository root in `SENDSPIN-SPEC-LICENSE.md`.
Earph's existing MIT licence remains in place.
