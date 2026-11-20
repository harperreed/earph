# Renderer control ownership

Core defines renderer authority separately from media-fetch authority.
The production owner keeps renderer ingress disabled.
These contracts do not enable a UPnP listener, discovery, subscriptions, or remote playback.

## Local inspection and grants

Use `renderer.get` through the existing private owner command socket.
The reply exposes enabled state, grant generations, expiry, and staged, active, and closing ownership.
It omits media URIs, metadata, peer addresses, and grant rights.
The `retained_bytes` field reports the shared network ledger, rather than a separate renderer allowance.

Use `renderer.grants.replace` to replace the controller registry atomically.
Supply `expected_generation`, `lifetime_ms`, and a `grants.controllers` document.
Each controller has an `id`, explicit peer IP addresses, and `rights`.
Supported rights are `stage`, `transport`, `rendering_control`, and `takeover`.
Takeover requires transport authority.
Existing transport grants do not authorize gain or preset changes.
Limits are eight controllers, eight peers per controller, and a lifetime from one millisecond through one hour.
Use `renderer.grants.revoke` with the current registry generation and the exact controller ID.

Registry edits advance the registry generation.
Surviving controller grants retain their own generation and expiry.
Exact request replay returns the original receipt without renewing authority.
Restart restores neither renderer grants nor remote ownership.

## Trusted ingress

The transport supplies the observed peer to `Session::submit_renderer_with_network_effects`.
The serialized `renderer.request` body cannot assert that peer identity.
Ordinary local IPC rejects renderer requests before replay lookup.
Only an enabled trusted ingress may submit a controller-scoped action.

Stage validates the descriptor and staging generation without fetching media.
It preserves the local queue, output selection, consent, gain, and mute state.
Play requires separate media grants and the current descriptor and controller fences.
It lowers to the existing HTTP-FLAC start transaction.
Repeated Play for the same current source neither refetches nor renews its leases.
Play never unmutes output.

## Gain and presets

Renderer gain and preset requests require the exact current active source binding and rendering-control authority.
Staging alone cannot authorize changes to unrelated playback.
Remote gain uses the same Core setting, caps, and headroom checks as local control.
Volume spans 0..30700; VolumeDB spans -32767..0 in 1/256 dB units.
Volume zero and minimum VolumeDB select exact software zero.
Finite readbacks derive from the shared Core grid, with quieter ties.
Wire quantization never rewrites an arbitrary local finite setting.
Descriptions, SOAP admission, responses, and the later event bridge use these same limits.

Local `volume.silence` selects zero without changing mute or the retained finite dB value.
The required `software_gain_zero` snapshot field distinguishes this setting from finite attenuation.
An explicit `volume.set` clears zero without unmuting.
Zero silences only the unaccepted suffix of a cached native period.
Accepted samples and reported progress remain unchanged.
Finite exit finishes any cached zero suffix before applying its requested ramp to newly generated frames.

`volume.factory_defaults` mutes and clears zero as one mutation.
Software mode selects the quieter of -40 dB and the current cap, subject to graph headroom.
Fixed mode preserves unity and disarms output.
Unsupported modes reject the complete preset.
These controls do not enable production renderer ingress.

## Retirement

One staged association, one active association, and one closing association have separate ownership.
There is one audio worker and one closing slot.
Staging another controller's descriptor does not transfer active ownership.

If A closes while B waits to start, stopping or revoking B invalidates B's execution authority immediately.
Core retains B's cleanup context until A's exact retirement acknowledgement frees the closing slot.
B then occupies that slot until B's exact acknowledgement settles its pending receipt.
Stale or duplicate acknowledgements cannot settle another source.
Local Stop, shutdown, and media-grant changes retain this cleanup obligation.

The Engine must cancel queued starts and join actual decoder, transport, and output resources.
Core acknowledgements alone do not prove native cleanup.
Fourteen real worker cases verify queued cancellation and cancellation during HTTP preparation.
They require actual A/B retirement acknowledgements, connection closure, worker joining, and matching native PCM captures.
The combined Linux canonical check also passed.
See the [qualification receipt](evidence/network/renderer-ownership-linux-20261120.json) for exact source bindings and limits.

These tests do not establish the internal deferred-start branch or cancellation after B unmutes.
The production bridge, callback ownership, and controller interoperability remain required before enabling renderer ingress.

See [HTTP owner playback](owner-http-playback.md) for the underlying source and output contract.
See the [Sendspin crate](../crates/earph-sendspin/README.md) for its separate protocol boundary.

The [shared gain receipt](evidence/network/software-zero-linux-20261120.json) binds the integrated Linux source and PCM proofs.

The [native zero acceptance](evidence/desktop/native-zero-linux-20261120.json) also verifies the shared setting through actual GUI views.
It covers software output and active diagnostics without enabling renderer ingress.
