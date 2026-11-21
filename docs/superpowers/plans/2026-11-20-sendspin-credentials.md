# Sendspin committed credentials

Doctor Biz authorized the full build and independent technical decisions.
Root implements this bounded unit while the Network and renderer owners finish their separate units.

## Contract

Keep permanent identity, permanent pairing PSK, catalog, and last-playback server in one versioned secret document.
Use the existing `RecordCatalog` as the only catalog owner.
Restore its records, superseded identifiers, capacity, and checked use order without reinterpretation.
Support the selected driver's five-record capacity.
Bound encoded and decoded documents to 65536 bytes.
Reject unknown versions, unknown fields, duplicate fields, malformed keys, and invalid catalog state.
Keep keys, encoded bytes, and catalog contents out of Debug and error output.
Expose encoding only through an explicit secret-byte API.
Never generate identity as a side effect of decoding.

## Implementation and evidence

- [x] Add failing public-boundary tests for exact secret roundtrip and independent JSON inspection.
- [x] Implement `credentials` in `earph-sendspin` with a private strict wire representation.
- [x] Prove malformed, oversized, ambiguous, and unsupported documents fail without yielding replacement credentials.
- [x] Exercise restored records through the actual Noise credential selector and checked catalog operations.
- [x] Run the scoped crate tests, formatting, and strict Clippy under the shared qualification lock.
- [x] Obtain one independent review before integration publication.
- [x] Run the combined canonical gate after concurrent dependency changes are reconciled.

The [combined Linux receipt](../../evidence/sendspin/runtime-linux-20261121.json) binds the merged source, products, and explicit peer runs.

## Ownership and limits

Root owns the new module, tests, documentation, and the crate's direct Serde edge.
The separate Network owner retains its transport dependencies and complete own-only patch.
Reconcile their Cargo lock changes and regenerate the canonical dependency inventory together.
This unit implements a secret document, without accessing any OS store or provisioning identity.
Production storage, bounded helper ownership, recovery readback, and startup remain required later work.
The current keyring providers can unlock or prompt; their synchronous APIs cannot run in the Network owner.
No default keyring, plaintext fallback, production credential access, or native permission change is authorized here.
