<!-- ABOUTME: Documents explicit native candidate construction and verification. -->
<!-- ABOUTME: Separates package integrity from product provenance and release qualification. -->

# Native package candidates

`earph-harness` prepares an unsigned, unreleased application directory from explicit, already-built products.
The tool never builds products, starts an owner, installs files, signs artifacts, or extracts source archives.
It retains partial outputs after errors and names their path.
Use a new output directory for every attempt.

Build the harness through the repository's pinned toolchain.
Then run:

```sh
earph-harness package-build --request package-request.json --output new-candidate
earph-harness package-verify --package new-candidate
```

The request uses this JSON shape:

```json
{
  "target": "x86_64-unknown-linux-gnu",
  "desktop": {"path": "products/earph-desktop", "byte_length": 123, "sha256": "<64 lowercase hex digits>"},
  "engine": {"path": "products/earph-engine", "byte_length": 456, "sha256": "<64 lowercase hex digits>"},
  "player": {"path": "products/earph", "byte_length": 789, "sha256": "<64 lowercase hex digits>"},
  "workspace_manifest": "Cargo.toml",
  "cargo_lock": "Cargo.lock",
  "dependency_inventory": "docs/dependency-lock.json",
  "license": "LICENSE",
  "specification_license": "SENDSPIN-SPEC-LICENSE.md",
  "notices": "THIRD_PARTY_NOTICES.txt",
  "build_record": "declared-build.json",
  "mpl_sources": "retained-mpl-sources",
  "linux_final_prefix": "/opt/earph"
}
```

Lengths and hashes must describe the actual input files.
All paths are explicit and resolve against the caller's working directory.
Request paths stay outside the distributed manifest.
The tool compares supplied canonical metadata against its compiled workspace metadata.
Use the matching tool revision when dependencies or notices change.
Metadata inputs must have owner read permission and no executable or special mode bits.
Products must have executable permission and no special mode bits.
The tool preserves accepted input bytes and modes.
Generated metadata uses mode `0644`.
Private input modes can limit access for other accounts; broader-user installation remains unqualified.
Symlinks in input, output, and ancestor paths are forbidden.
Use private directories without concurrent writers; this tool does not provide a hostile-filesystem sandbox.

The declared build record uses this shape:

```json
{
  "workspace_version": "0.1.0",
  "target": "x86_64-unknown-linux-gnu",
  "source_commit": null,
  "source_archive_sha256": null,
  "profile": "debug",
  "rustc_version": null
}
```

Use a known 40-digit commit and 64-digit archive digest when available.
Keep unknown values `null`.
The profile accepts `debug` or `release`.
The optional compiler string accepts bounded public version text.
The record must match the canonical workspace version and requested target.
No supplied record proves that a binary came from those sources.
Every package therefore labels product provenance `declared-unverified`.
An exact retained qualification must bind product digests before broader claims become valid.
The manifest records the package tool's own embedded build provenance separately.
Packaging older qualified products does not make them products of the tool's current source revision.
Do not infer source identity from binary strings or invent a desktop build-info command.

Linux candidates contain adjacent `bin/earph-desktop`, `bin/earph-engine`, and `bin/earph` products.
`earph` is the command-line player; see [its guide](../crates/earph-engine/PLAYER-CLI.md).
Notices, canonical manifests, source archives, and declared provenance live under `share/earph`.
A non-null `linux_final_prefix` creates `share/applications/earph.desktop`.
Its quoted `Exec` names that absolute final prefix and escapes desktop-entry special characters.
The prefix must match the eventual installation location.
Relocating the staged directory does not update menu registration.
Set the prefix to `null` to omit the desktop entry and register it separately later.
The package tool does not install or register it.
Linux runtime dependencies remain external and incompletely enumerated.

macOS candidates contain `Earph.app/Contents/MacOS/earph-desktop`, the adjacent engine, and the adjacent `earph` player.
Resources live under `Earph.app/Contents/Resources`.
The native Mac controller uses SwiftUI and standard AppKit text editing.
Build it against the generated desktop bindings and Rust static library:

```sh
scripts/check-macos-desktop --output /absolute/new-native-build
```

This command requires macOS, Xcode, and the pinned Rust toolchain.
It retains compiler logs, generated bindings, native link arguments, tests, and product hashes.
Its owner integration uses a separate private runtime without selecting output or starting playback.
It retains that runtime after the check.
It never changes the general clipboard or grants system permissions.
Use its `products/earph-desktop`, `products/earph-engine`, and `products/earph` as the package inputs.
The Swift product statically links the Rust controller; no extra runtime bridge library is bundled.
Build and package checks do not establish native user acceptance or physical audio delivery.

The XML property list uses the canonical workspace version and `com.harperreed.earph`.
It does not advertise a minimum OS, permissions, file associations, or hardware support.
Set `linux_final_prefix` to `null` for macOS.
Supported structural targets are x86_64 and aarch64 Linux GNU, and x86_64 and aarch64 Darwin.
The tool checks 64-bit little-endian ELF or Mach-O executable headers against the target.
Those checks do not establish a libc ABI or inspect embedded product signatures.
The manifest records product signatures as uninspected and the distribution candidate as unsigned.
Structural checks do not establish that an operating system can launch the application.
Generated header fixtures test structural handling only.
macOS validation and launch require a real Mac gate.
Use `desktop-file-validate` and macOS `plutil -lint` when available.

Supply `mpl_sources/index.json` and every archive named by that public-safe index.
The index must bind the canonical Cargo lock and dependency inventory.
It must include every exact MPL-2.0 package in the workspace feature union.
The current union has fifteen archives, including generator tooling.
This union does not establish per-product linkage.
Archive digests must match the Cargo lock.
The tool copies each original archive without extraction or relicensing.
It distributes recipient instructions beside the notices.
Private retention receipts and other files from the input source directory are excluded.
Dependency changes require a fresh source set and review.
SDK distribution needs separate preparation.

`package.json` inventories every regular distributed file except itself.
Each entry names a canonical relative path, length, SHA256, mode, and product kind.
Verification rejects missing files, extra files or directories, symlinks, wrong modes, and changed bytes.
It also checks the target layout, canonical notices, source set, declared record, and generated launch metadata.
Limits include 40 manifest files, 64 tree entries, six path components, and a 128 KiB manifest.
Each product is capped at 512 MiB.
Other copied files are capped at 8 MiB.
The verifier never repairs a candidate or launches its contents.
The manifest is unsigned; an attacker who changes both products and manifest can replace their declared identities.
Checksums provide consistency checks, not authenticated distribution.

Package tests retain their fixtures under the process temporary directory.
They read original locked archives from `CARGO_HOME/registry/cache` without changing the cache.
Run `cargo fetch --locked` first if those archives are absent.
Real-product integration must use exact retained Earph binaries and independently bound input hashes.
Its software E2E starts the packaged owner explicitly, sends a packaged CLI State request, and checks owner identity.
It then sends a guarded shutdown and observes a normal exit.
It uses no selected output, source, user media, or physical audio.
GUI acceptance remains a separate native harness gate.

REL-03 and AC-50 remain partial.
Installation, authenticated artifacts, signing, macOS launch, rollback, and the full release evidence matrix remain later gates.

## Qualified software scope

The [Mac native-control receipt](evidence/desktop/macos-native-controls-20261121.json) binds the standard-control application and six native checks.
The operator confirmed external paste in the launched app.
The separate Core Audio rate and callback mismatch was fixed after that control-only receipt.
Doctor Biz later confirmed audible local PCM from the installed Orbit app.
The [system-output receipt](evidence/local-playback/coreaudio-system-20261121.json) binds the separate silent native lifecycle qualification.
The [simpler-player receipt](evidence/local-playback/simple-player-20261121.json) binds the later controller and six native checks.
That later app still needs installation and user acceptance on Orbit.
Control and package checks alone establish no physical playback or timing qualification.

The [Linux package receipt](evidence/packaging/software-linux-20261120.json) binds construction and verification to the integrated source.
The [packaged GUI receipt](evidence/packaging/packaged-gui-linux-20261120.json) covers a separate, exact retained product pair.
That pair passed private X11 interaction, graph diagnostics, exact-zero software playback, finite exit, and command-based FactoryDefaults.
Its preview matched all 97375 exported diagnostic bytes.
Post-run verification found every package file unchanged and every tracked process retired.
This qualifies those Linux software products without extending the result to later binaries or physical output.

The [current candidate receipt](evidence/packaging/candidate-linux-20261121.json) binds the later renderer and Sendspin products.
It verifies package consistency, a real packaged owner/CLI exchange, and normal shutdown.
Its GUI, installation, signing, and physical-output gates remain separate.
