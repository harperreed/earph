# Renderer owner execution

The Engine library can bind one explicit renderer listener.
Production CLI startup and automatic discovery remain disabled until joint U5 qualification.
This implementation does not qualify named controllers, macOS, physical output, or a release package.

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

Root owns the merged canonical check and independent high-risk review.
Default startup, discovery, named-controller interoperability, current Mac qualification, and release acceptance remain separate gates.
