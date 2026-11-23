# Renderer owner execution

The Engine owner can bind one explicit renderer listener.
The owner starts it only when its command line names an interface address.
Without that option, the owner opens no network socket and sends no multicast.
This implementation does not qualify named controllers, macOS, physical output, or a release package.

## Owner command line option

```
earph-engine owner --runtime DIRECTORY --renderer-listen IPV4:PORT [--renderer-discovery]
```

`--renderer-listen` starts HTTP description, SOAP control, and eventing on one address.
Give the IPv4 address of one interface, with a port.
Port 0 lets the system choose a free port.
The owner refuses `0.0.0.0`, multicast and broadcast addresses, IPv6, host names, and a missing port.
It refuses a repeated option. It never scans interfaces or binds another address.
A loopback address is valid and serves controllers on the same host only.

`--renderer-discovery` adds SSDP on the same interface.
The owner joins the SSDP group on that interface, sends alive messages, and answers searches.
It sends byebye messages when it stops. The multicast TTL is 4.
The option needs `--renderer-listen`. It works on Linux only.
On macOS, the owner start fails with `cannot start renderer discovery on the selected interface`.
See [the protocol surface](upnp-protocol-surface.md) for the required Mac interface work.

The option opens the listener only. It grants no controller any right.
Until a local `renderer.grants.replace` command names a peer address, every SOAP action returns fault 606.
The device description and service descriptions answer any peer that reaches the listener.
The existing URL admission, media grants, callback grants, and byte budgets apply unchanged.

A renderer that cannot start fails the owner start.
Discovery acquires its sockets before the listener serves any request.
A failed start serves nothing and creates no `renderer-udn` file.
The owner prints the error, exits with status 3, and serves nothing.
A user who asked for network control must not get a silent owner without it.
The Sendspin option fails the same way.
When another owner already holds the runtime, the command fails; a running owner cannot adopt the option.
The owner checks the option's syntax before it touches the runtime directory.

The owner reports its listener in two places.
The startup line gains `"renderer": {"address": "IPV4:PORT", "discovery": BOOLEAN}`.
The `renderer.get` reply carries the same object in `listener`, or `null` when no listener runs.
Both show the bound port, also when the option gave port 0.

The owner keeps its device name (UDN) in the private runtime file `renderer-udn`.
It creates a random UUID on first use, stores it once the renderer has started, and reads it on every later start.
Controllers therefore see the same device after a restart.
The owner refuses to start the renderer when that file holds no valid UUID name; it never replaces the file.
The friendly name is `Earph`.

`owner.shutdown` retires renderer authority, stops the listener, and joins its thread before the owner exits.
With discovery, the thread sends byebye before it closes its sockets.

The `earph` player command has no renderer option yet.
It has no command for controller grants, so a renderer started there could serve no controller.

## Explicit library configuration

Create `RendererConfiguration` with one concrete IPv4 address, a stable UDN, a friendly name, and a valid server identity.
Call `EngineOwner::with_renderer` before `serve`.
The builder binds the actual HTTP listener before enabling Core renderer admission.
`renderer_address()` returns its bound address, including an allocated port when configuration supplies zero.
The default owner has no renderer listener.

The configuration creates no controller, playback, media-origin, or callback authority.
Install those grants through existing local owner commands.
A controller's nullable playback authorization pins immutable authored intent, output configuration, and device epoch.
Callback grants remain separate from media grants.
There is no application mock mode or automatic Unmute.

## Serial commands and playback

HTTP actions enter the same owner transaction and playback worker as local IPC commands.
The adapter supplies the observed peer separately from the command envelope.
The Engine rejects absent or ambiguous controller ownership.
It captures each ticket's envelope once, with current owner, session, revision, and controller generation.
Request IDs contain a bounded owner digest, listener epoch, and unique transport ticket.
Equal SOAP bodies from separate tickets receive separate identities.

Stage reads the current global staging generation through an actual typed `SourcesGet` transaction.
That read and Stage admission occupy the same serial owner turn.
The bridge never reconstructs staging generation from a renderer association or cached local state.
The shared queue continues to represent local intent.
Renderer Stage, Play, and Stop create no controller queue.

Accepted Play starts the canonical HTTP-FLAC worker under the exact Core source binding.
Fresh graph preparation begins only after the owner adopts measured `Ready` facts.
The request uses the continuation's immutable authorization.
It never inherits later configured intent or a previous prepared token.
Commit uses the actual new token and compiled activation hash.
Core rechecks authority on preparation completion, commit, and native acknowledgement.
An exact pending GraphCommit replay neither persists again nor queues another activation.

`GraphJobs` retains at most one captured pending request while its displaced candidate retires.
The pending request keeps Core's original five-second deadline.
Before submission, the bridge rechecks the exact job, owner, session, epochs, device, revision, and source cancellation.
It also rechecks renderer authority when the job belongs to a renderer continuation.
The existing preparation service retires the previous pipeline before allocating its replacement.
Stop and shutdown discard a pending request which never submits.

Stop, revocation, expiry, listener retirement, and accepted competing graph work invalidate continuations.
Late results cannot recreate authority.
An incompatible authored rate or layout fails muted without substitute processing.
An early Unmute refusal leaves no deferred action.
Pause, Resume, mute, volume, and presets reuse the existing worker and Core safety policy.
Paused Play resumes only when this controller's staged media matches its active source.
Changed staging follows normal Play admission, including the existing takeover requirement.
During takeover, Core retains the replacement's live graph authority while the outgoing source retires.
Graph preparation, commit, and activation still require that retirement to finish.

## Subscriptions and callbacks

Core owns SIDs, initial and latest snapshots, sequence numbers, expiry, and closing delivery records.
The Engine retains only exact dispatch bindings and their captured endpoint service.
Renewal and unsubscribe through another service URL fail before mutation.
Core still checks the SID's complete controller and callback scope.

New subscriptions reserve concrete leases from the adapter's existing eight-MiB ledger.
The Engine adds no event buffer pool.
The adapter's reservation partition permits six full AVTransport subscriptions while preserving ordinary and Stop progress.
Sixteen remains Core's record ceiling, including closing records.

The Engine converts each accepted connection's actual deadline into Core's monotonic clock.
It acknowledges initial response completion only from the adapter's successful TCP write-FIN completion.
Handing a response to Hyper creates no sending authority.
Each callback attempt obtains a fresh Core authority view.
Committed revocation invalidates Core first, then clears the adapter's live view before cancellation.

The Engine holds each delivery completion while Core reconciles its exact identity and actual join result.
Only then does dropping that completion release the adapter's delivery position.
An unjoined result retains Core closing state and the adapter lease.
It does not terminate the serial owner or report successful retirement.
Callbacks work before Play and after Stop while independent authority remains live.

## Shutdown and exceptional retention

Listener retirement advances Core's epoch and revokes old authority before transport shutdown.
The actor requests shutdown and polls completion without blocking joins.
Owner exit waits for listener join and Core subscription retirement alongside existing graph and source retirement.

If fatal actor exit occurs before listener join, the bridge retains that exact surface and join handle.
This exceptional retention has no recovery API in this unit.
Normal shutdown joins and releases the surface.
An uncertain delivery join keeps shutdown pending rather than discarding its ownership.

## Verification boundary

Engine tests use actual private HTTP sockets, generated public FLAC, and isolated ALSA file/null fixtures.
They exercise fresh compilation, native acknowledgements, stale fences, pending cancellation, exact replay, and queue preservation.
The executable owner fixture checks pinned half-gain samples and callbacks after media Stop and media-grant revocation.
Event tests exercise actual FIN-gated initial NOTIFY, service fences, shared-budget pressure, cancellation, expiry, and joined shutdown.
The unjoined-result test holds an actual sender task and retains its resources until a later actual joined completion.
It injects the unjoined boundary result; it does not claim a spontaneous transport join failure.

Owner process tests start the shipped `earph-engine` executable on a loopback address.
They read `/proc` to prove that a default owner holds no IPv4 or IPv6 socket.
With the option, they drive a refused SOAP action, a granted SOAP read, and a clean shutdown.
The discovery test joins the SSDP group on the loopback interface only.
It observes alive, a search answer, and byebye. No test sends multicast to a real network.

Root owns the merged canonical check and independent high-risk review.
Default startup, real-network discovery, named-controller interoperability, current Mac qualification, and release acceptance remain separate gates.
