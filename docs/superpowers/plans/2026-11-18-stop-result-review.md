# Stop result ownership

Resolve review 5958051547 against source commit `d7b1098`.
Preserve prior worktrees, source files, and diagnostic artifacts.

## Success criteria

- Preserve an unacknowledged same-source fault across Stop.
- Keep replacement Start's old-source discard behavior.
- Bind Stop faults to the exact accepted Stop and captured owner source.
- Preserve the original installed graph identity in terminal proof.
- Reject unrelated, stale, and future source bindings.
- Preserve ordinary fault generation checks and exact acknowledgements.
- Keep successful Stop and normal EOF behavior unchanged.
- Verify receipt settlement and consent revocation at the revision limit.
- Pass checks and independent review before integration.

## Decisions

Capture each Stop request ID and settings in the worker action.
Report its failure through an explicit `StopFailed` event.
Retain the installed-generation terminal token unchanged until exact acknowledgement.
Allow the captured owner generation or its checked successor for this requested result.
Only one pending graph activation can explain that successor.
Ordinary failures still require the owner's exact installed graph generation.

Check the actual retained accepted Stop through a read-only Core accessor.
Check the captured owner, session, stream, graph, and device identity.
Check the original terminal token's source origin and final entry observation.
Apply `LocalFailed` before acknowledging its proof.
Repeated delivery cannot apply another terminal transition after the Stop settles.

Reconcile retained faults before clearing terminal state on Stop.
Clear old results on replacement Start.
Keep normal completion and successful Stop on their existing paths.
Box the optional Stop binding to keep bounded event variants small.

## Evidence

The native regression reproduces an ALSA write fault through `/dev/full`.
Before the fix, Stop reports success after discarding that retained fault.
The host regression reproduces rejection of graph generation G+1 after Stop cancels activation G.
Both regressions pass with explicit Stop binding.

Tests cover repeated mute generations, wrong and exact acknowledgement, and another Stop.
They cover stale Stop IDs, a pending Pause, replacement source, and invalid token fields.
They also cover revision exhaustion, ignored activation acknowledgements, normal EOF, and unchanged consent after successful Stop.
The native graph regression preserves generation G+1 after installing a real NULL-output pipeline.
It passes a genuine closed-output native fault through the captured cancellation-result seam.

The tests do not force the native ALSA stop syscall itself to fail.
That would require device loss or a synthetic backend in this environment.
No hardware or application mock mode was used.
Expected ALSA diagnostics are captured and asserted in a private test subprocess.

The canonical check passes 393 tests, with two ignored and no failures.
Formatting, strict workspace Clippy, requirement checks, and dependency checks pass.
The scoped final diff review found no further defect.
Root independently reviewed the actual patch, callers, ownership paths, and evidence.
No Important or Critical finding remains in this patch.
The root combined canonical check also passes 393 tests, with two ignored.

## Authorization and handoff

Doctor Biz authorized these fixes and their validation.
Root approved the private Stop request contract and read-only Core accessor.
Root owns integration, independent review, commits, and publishing.
Do not delete files, use hardware, push, reset, or bypass hooks.
Earlier hardware receipts remain historical.
