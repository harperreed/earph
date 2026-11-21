# Native committed credentials

`earph-credentials` stores one encoded `CommittedCredentials` document.
`RecordCatalog` remains the only pairing catalog.
The adapter generates no identity and changes no Engine startup behavior.

Create `CredentialStore` on a dedicated supervisor thread.
Supply the absolute helper executable, explicit native configuration, and one private owner lock path.
Keep that lock path stable across all owners of the same native document.
Use an owner-private directory and a lock file with mode `0600`.
The lock contains counters and a pending operation ID, never secret bytes.
Opening syncs both the initialized marker and its containing directory before allowing native operations.
The marker opens relative to that directory handle, so both syncs cover the same entry.
Directory-sync failure returns uncertainty and starts no helper.
This ordering follows the separate directory-sync requirement in [fsync(2)](https://man7.org/linux/man-pages/man2/fsync.2.html).
Use a dedicated credential lock file, separate from Engine's `owner.lock`.
An unrecognized or invalid marker stops opening without rewriting its bytes.
The helper executable must match its recorded hash for every operation.
Its trust checks follow the existing Engine helper convention.

Linux requires an explicit D-Bus address.
The helper resolves and pins one Secret Service owner without activating another service.
It requires an existing unlocked default collection.
It never creates collections, unlocks items, displays prompts, or retries writes.
A returned prompt causes uncertainty and connection teardown, without calling `Prompt`.

macOS requires an explicit existing keychain path.
The helper disables user interaction in its own process before native access.
It checks the selected keychain's unlocked status before searching.
It never changes the default keychain, unlocks a keychain, or sets broader access rules.
The adapter uses legacy Keychain APIs because they support an explicit isolated keychain.
Apple marks these APIs deprecated; hosted Mac compilation and runtime qualification remain release gates.

`Missing` means definitive native document absence in the accessible selected store.
`Locked`, `Unavailable`, `Ambiguous`, and `InvalidDocument` never authorize identity generation.
The adapter refuses to overwrite a corrupt or ambiguous native document.
There is no file fallback, deletion API, or repair path.

A commit persists its operation intent before starting the helper.
Native acknowledgement and fresh exact encoded-byte readback are both required for success.
Secret bytes travel only through anonymous stdin/stdout pipes and native storage calls.
Native diagnostics never enter the public error type.
Cancellation and deadlines kill and reap the exact owned helper.
A failed helper after possible write issuance returns `Uncertain`.

Uncertainty survives owner replacement through the private operation marker.
`load` and `commit` refuse pending uncertainty.
`recover` returns a fresh native observation but preserves the pending operation.
A matching old document cannot prove an outstanding server mutation has finished.
Only the caller can establish terminal native completion outside this API.
After that proof and a successful recovery observation, call `acknowledge_native_completion(operation, cancel)`.
That call performs another fresh cancellable read before clearing the pending operation.
Any failed read preserves uncertainty.
Treat earlier recovery observations as diagnostics while the pending operation remains.
Do not acknowledge completion from a read alone.
If completion cannot be proved, keep the latch and stop new connections or writes.

Native stores supply no compare-and-swap across external writers.
The file lock serializes cooperating Earph owners using the same lock path.
Preflight validates the current document but cannot exclude another native writer afterward.
Exact readback catches observed conflicts, not later external writes.
Use one application owner for this document; do not claim stronger concurrency guarantees.

## Qualification

Ordinary crate tests contact no native store.
Set `EARPH_CREDENTIAL_TEST_ROOT` to a fresh retained private directory for process tests.
The process fixture uses real child execution and synthetic anonymous-pipe documents.
Its native responses are test doubles; isolated native tests cover actual provider behavior.
Run `node crates/earph-credentials/tests/directory_sync.mjs ABS_PROCESS_TEST_BINARY ABS_OUTPUT_DIRECTORY` from the workspace.
That Linux check requires `/usr/bin/strace` and runs only owned fixture processes.
It checks real file/directory/helper ordering and injects a directory `fsync` failure.
The failure cases also verify refusal before helper startup and unchanged existing pending bytes.
Its traces include only `fsync` and `execve`; they omit credential pipe contents.
These checks establish syscall ordering and failure behavior, not simulated power-loss recovery.

Compile the crate and native tests before starting any private native service.
On Linux, Root must byte-review `scripts/qualify-native-credentials.mjs` first.
Run that operator under the shared qualification lock.
Supply absolute helper, native test binary, and private output-directory paths.
The operator retains its fresh `~/.cache/ec-*` root and all owned-process receipts.
It never contacts the workstation session bus or keyring.
The ignored native test checks missing, exact create/update, corruption refusal, ambiguity, locking, and unavailable service.

On a disposable hosted Mac runner, compile all crate targets first.
Create a fresh `0700` directory beneath `RUNNER_TEMP`.
Write a random marker token into `owner-marker` with mode `0600`.
Set `EARPH_CREDENTIAL_TEST_KEYCHAIN_ROOT` and `EARPH_CREDENTIAL_TEST_KEYCHAIN_TOKEN` to that directory and token.
Run only `native_macos` with `--ignored --exact isolated_keychain`.
The test creates and retains one synthetic keychain with an explicit password.
It never selects the runner's existing keychains.
No Mac host, SSH session, device, application controller, or physical output belongs to this qualification.

Use `scripts/qualify-native-credentials-macos.mjs ABS_CARGO_1_98_1` for the hosted Mac gate.
Root must review that operator before its first native run.
It checks the Cargo version, compiles all crate targets, runs ordinary tests, then creates the isolated keychain.
It preserves its private runner directory, logs, binaries, and receipts.
The hosted VM retains those files only for its lifetime.
The workflow publishes an allowlisted summary through `scripts/report-native-credentials-macos.mjs`.
That summary binds Cargo-reported helper and test paths to their SHA256 hashes.
It reports operation exits and native test counts without credential data or private paths.
Downloaded job logs provide durable evidence; they do not preserve the keychain files.

The [hosted Mac receipt](evidence/sendspin/native-credentials-macos-20261121.json) binds two successful isolated keychain runs.
Both runs reached corruption refusal and locked-store refusal with exact helper and test hashes.
These runs predate the dormant Engine service bridge and timing integration.

## Dependency and API basis

The adapter reuses the existing locked packages without adding package versions.
Its direct native edges are `zbus 5.19.0`, `security-framework 3.7.0`, `security-framework-sys 2.17.0`, and `core-foundation 0.10.1`.
`zbus` uses MIT; the other native packages offer MIT or Apache-2.0.
The complete feature and license inventory is regenerated with `earph-harness dependency-lock` in the private qualification packet.
Root must regenerate the canonical combined inventory after integration.

The Linux contract follows the [Secret Service interfaces](https://specifications.freedesktop.org/secret-service/latest/org.freedesktop.Secret.Service.html).
The [collection API](https://specifications.freedesktop.org/secret-service/latest/org.freedesktop.Secret.Collection.html) defines mutation acknowledgement and returned prompts.
The [prompt contract](https://specifications.freedesktop.org/secret-service/latest/prompts.html) requires explicit invocation to display a prompt.
GNOME Keyring can change MIME metadata while preserving exact secret bytes.
The adapter validates the canonical document bytes independently of that metadata.
This behavior appears in GNOME's [native secret encoder](https://github.com/GNOME/gnome-keyring/blob/master/daemon/dbus/gkd-secret-secret.c).
The Mac binding follows Apple's [SecKeychain header](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/SecKeychain.h).
The interaction error constant follows Apple's [SecBase header](https://github.com/apple-oss-distributions/Security/blob/main/base/SecBase.h).
No external implementation was copied.

The dormant Engine [Sendspin services bridge](sendspin-native-services.md) retains this store on a dedicated thread.
It preserves the loaded identity envelope during whole-catalog commits.
A recovery observation never acknowledges native completion or restores connection authority.
