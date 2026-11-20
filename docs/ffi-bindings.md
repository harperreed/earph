# Native control bindings

`earph-ffi` exposes an attach-only `LocalClient` through UniFFI 0.32.2.
It forwards the existing versioned JSON control documents to the private local owner.
It does not start an owner, open output, or add another command or safety policy.

Create the owner through the existing host workflow before sending requests.
Construct `LocalClient` with the selected runtime directory.
Construction retains at most 60 UTF-8 bytes and discards excess caller string capacity.
The existing Engine request validates the directory, endpoint, ownership, and socket on each call.
An invalid or unavailable endpoint remains an explicit failure.

`request` accepts owned command bytes and returns owned reply bytes.
Those bytes contain control JSON, never PCM buffers.
Use the existing command envelope, request identity, owner/session targets, and expected revisions.
Read fresh state before constructing a mutation.
Do not replace a rejected target or revision inside a client adapter.

Replies retain the exact existing owner JSON.
An accepted receipt still means pending work.
Resend the identical envelope to reconcile its terminal receipt.
Owner parse failures can use a top-level `error`; admitted semantic failures use retained receipts.
The binding does not rewrite either document shape.

Local request failures become `ClientError::Engine` in Rust and Swift, or `ClientException.Engine` in Kotlin.
Its `code` contains Core's stable error spelling.
Its `detail` contains Core's original static message.
The foreign field avoids Kotlin's reserved exception `message` property.
The owner JSON still uses its existing `message` field.

An empty frame or a frame above the shared one-MiB limit fails before endpoint lookup.
The raw Engine request currently discovers its endpoint before checking its write-frame length.
This means an oversized frame and an absent owner can produce different local error precedence.
Valid admitted requests reach the same owner contract.
Foreign buffer lowering can allocate before the Rust length check.
The frame limit does not bound total foreign process memory or allocator overhead.

Run synchronous requests outside the foreign UI thread.
Swift reference release and Kotlin `close()` release client handles without stopping the owner.
Each request owns only its existing bounded connection lifetime.
Discarding a reply or cancelling a foreign task does not undo an already-sent command.
Explicit shared Stop and mute commands retain their existing meaning.
Prompt request cancellation, event subscriptions, and foreign callbacks remain unavailable.

## Build and check

Build the package-local `earph-bindgen` binary with the explicit `bindgen` feature.
Build the ordinary library without that feature afterward.
Generate from that exact library using the already-built generator executable.
This order keeps the generator's optional CLI features out of the ordinary library build.
Do not run Cargo while relying on a frozen executable or library hash.

Run `scripts/check-ffi --help` for the focused gate.
Select `kotlin` or `swift` and a new absolute artifact directory.
For Kotlin, supply the compiler executable and JNA JAR explicitly.
For Swift, supply its compiler path when it is absent from PATH.
For direct macOS compiler runs, select the SDK through `xcrun --sdk macosx --show-sdk-path`.
Apply `SDKROOT` only to the gate's child process.
Replace the two uppercase path placeholders before running this command:

```bash
SDKROOT="$(xcrun --sdk macosx --show-sdk-path)" scripts/check-ffi --language swift --output NEW_ABSOLUTE_DIRECTORY --swiftc VERIFIED_SWIFTC_PATH
```

This invocation does not change global Xcode selection or shell configuration.
The gate builds, generates, compiles, and exercises the actual owner and request CLI.
It also runs the startup parser's real-file tests before building.
Endpoint publication identifies the current socket before the owner finishes startup.
The foreign driver waits for the bounded owner announcement, then checks the same identity through State.
It retains the existing eight-second startup budget and one-second IPC deadline.
It runs strict Clippy with the generator feature because default workspace checks omit that binary.
It preserves all generated files and logs, including failed runs.
It never installs tools or removes artifacts.

The Kotlin gate extracts its verified JNA dispatcher into the new artifact directory.
It verifies exact dispatcher bytes before and after execution.
The actual consumer checks JNA's loaded dispatcher path against that retained file.
Child JVMs set `jna.nounpack=true`, `jna.noclasspath=true`, and `jna.nosys=true`.
They set `jna.boot.library.path` to the private dispatcher directory.
They also use private `jna.tmpdir` and `java.io.tmpdir` directories.
These are child-process settings, not global Java or JNA configuration.
The compiler receives the same settings through its supported `-J` arguments.

JNA's default loader can clean earlier JNA temporary files before loading its dispatcher.
The first development gate did not isolate that behavior.
No before-state receipt establishes whether earlier files existed or were removed.
That first gate remains preserved with this limit and must not be rerun without the safe settings.
The retained dispatcher path disables automatic unpacking, cleanup, and system or classpath fallback.
See the [pinned JNA loader](https://github.com/java-native-access/jna/blob/5.19.1/src/com/sun/jna/Native.java).

The Kotlin reference tools are compiler 2.4.21, Java 26.0.2.1, and JNA 5.19.1.
The compiler archive SHA-256 is `7cc140e76daf416a0424a5557f99afd7ca89ebd463ea1101261e3e01c33e75cd`.
It matches the publisher's checksum file and GitHub release asset digest.
The JNA JAR SHA-256 is `4fb141dd8ef6b0585ffceea4bc49602fbc6312fa977e2c488794ea3e6aafecae`.
Maven and the pinned GitHub tag provide identical JAR bytes; Maven's SHA-1 sidecar also matches.
The missing Maven SHA-256 sidecar does not supply a checksum claim.
See the [Kotlin release](https://github.com/JetBrains/kotlin/releases/tag/v2.4.21) and [JNA source](https://github.com/java-native-access/jna/tree/5.19.1).

The existing dependency inventory producer includes `earph-ffi/bindgen` explicitly.
It records the ordinary workspace plus binding-generator feature union.
The ordinary library still uses UniFFI without default or CLI features.
Reviewed source licenses retain exact published VCS identities and content hashes.
The generated notices remain the Cargo dependency license authority.

## Hosted Swift software check

The Mac software workflow runs this same generated Swift gate on its arm64 runner.
It pins Node 26.13.1 and Rust 1.98.1, then resolves Swift and the SDK through Xcode.
The actual foreign client compares replies with the real owner and request CLI.
Its generated profile, network, renderer-refusal, and volume cases retain their existing scope.
The fixture output stays unopened and unconsented, with no source start or audio submission.
Failure output includes bounded build and scenario logs, without publishing private JSON transcripts.

This workflow requires no SSH identity or desktop permission grant.
An earlier Mac software pass does not establish that this new gate passed.
Read the workflow result for the exact changed commit before claiming current Swift compatibility.
Native playback, GUI access, SDK distribution, and release qualification remain separate.

## Qualification limits

The first unit covers control bytes, immutable replies, object disposal, bounds, and local IPC failures.
Its real owner scenarios include asynchronous profile-save receipts without opening output.
The foreign gate compares actual replies with the CLI and checks literal safety and receipt outcomes.
Safety comparisons cover fields actually exposed by the shared snapshot.
Private fixed-output arming is not directly observable through that JSON snapshot.
It does not qualify desktop hosting, UI workflows, audio callbacks, or real-time performance.
The full play/pause/seek/profile/graph and subscriber/cancellation matrices remain later T13 work.
Run both platform gates and record their exact source and artifact bindings before claiming cross-platform qualification.

The integrated scenario reads fresh State before each profile or package mutation.
It retains the first foreign reply, including Accepted, before polling the identical envelope through the CLI.
It then compares stable terminal replies byte-for-byte through both clients.
Every foreign batch releases its handle and queries State through a fresh handle.
The Kotlin checkpoint exercises profile creation, lookup, listing, manual resolution, and generated directory package export and inspection.
It also refuses missing export review, rejects an occupied import ID, and retains a failed missing-package inspection.
Exported object hashes and bytes match the owner's original generated objects.
These operations leave output selection, consent, mute, gain, caps, and configured profile unchanged.
Successful foreign package import and current Swift execution remain separate qualification work.
See [the Kotlin checkpoint](evidence/desktop/kotlin-profile-replay-linux-20261119.json) for exact counts and limits.

The [earlier Linux Kotlin network gate](evidence/ffi/linux-kotlin-network-20261120.json) exercises network control through generated bindings.
Nine network commands cover grants, URI staging, revocation, and exact grant replay after revocation.
The replay preserves the revoked generation and absent lease; the source snapshot hides the staged URI query.
A real HTTP listener on an owned private IPv4 interface observes zero connections and requests.
The focused gate requires that private interface for its control-only fixture.
The extended run sends 66 foreign frames through 29 consumers and makes 38 exact CLI comparisons.
All 116 local guard replies cover four scenarios and two distinct error codes.
Current Swift execution and older generated-consumer compatibility remain unqualified.

The [renderer refusal snapshot](evidence/ffi/linux-kotlin-renderer-20261120.json) binds the later frozen Linux runtime and driver.
Thirteen Rust client tests, four startup tests, strict Clippy, generation, and Kotlin compilation pass.
Its corrected actual scenario sends 78 frames through 35 consumers and makes 44 exact CLI comparisons.
All 140 local guard replies retain the private JNA settings.
Six renderer calls verify inspection, refused grants and revocation, denied untrusted ingress, and exact rejected-grant replay.
The disabled registry and actual owner state remain unchanged.
The nine network cases still observe zero TCP connections and HTTP requests.
The owner exits normally, and all six frozen products remain unchanged.

The first scenario reused a receipt filename and failed its exclusive write.
The corrected State label keeps every assertion and exclusive-write guard.
Only that driver label changed after the successful build phases; the corrected scenario reuses their exact verified products.
Later test-fixture changes fall outside that frozen source snapshot.
This proof covers disabled renderer control, rather than enabled renderer ingress or playback.

The [generated Kotlin gain receipt](evidence/ffi/linux-kotlin-volume-20261120.json) covers the later integrated zero setting.
Its 94 frames use 43 consumers, 52 exact CLI comparisons, and 172 local guard replies.
Eight gain calls cover zero, finite exit, diagnostic readback, exact replay, cap refusal, and FactoryDefaults.
The fixture selects an unopened, unconsented output after the earlier intent-preservation checks.
All 48 JVM invocations retain private JNA guards.
The owner exits normally without opening audio.
Two runtime hashes remain stable through the scenario; six products are frozen after the complete gate.
