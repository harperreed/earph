# Sendspin timing arithmetic

Doctor Biz authorized this pure timing unit and its fixed policies.
The authority is [Sendspin spec 9d2f15e](https://github.com/Sendspin/spec/blob/9d2f15ee8f172c67ce00dc0d9f6603c3b1030d9f/messaging.md#clock-synchronization).
The player authority is the same revision's [player v1](https://github.com/Sendspin/spec/blob/9d2f15ee8f172c67ce00dc0d9f6603c3b1030d9f/roles/player/v1.md).

`earph_sendspin::timing::TimeSync` owns one connection's scalar filter and bounded burst state.
It owns no clock, thread, transport, credentials, audio buffer, DSP control, or output lease.
It returns mapping facts without player availability or playback admission.

## Binding and timestamps

Create a fresh reducer for each server, connection generation, or caller clock-domain generation.
`Binding` contains only public identity bytes and generations.
Debug output redacts all binding fields.
Keep the reducer through re-handshake, while abandoning its burst work through `set_suppressed`.

Pass nonnegative local monotonic microseconds from one caller clock domain.
Pass signed server microseconds in the server's clock domain.
All integer inputs and results must fit `±(2^53 - 1)`.
This is Earph's numeric envelope, rather than a protocol wire limit.
Use checked i128 arithmetic before converting integers to f64.
Conversion rounds halves away from zero.
Numeric faults retire mapping use without covariance repair.

Call methods in local timestamp order.
Capture T1 at the declared actual send boundary.
Capture T4 at complete transport delivery, using final-fragment arrival for fragmented messages.
Deliver captured replies before a tick that would expire their pending request.
`ReceiveStamp` stores the original checked microseconds; only deadline calculations derive milliseconds.
Do not timestamp a reply when the consumer dequeues it.
An unmatched old reply has no authority to advance or regress the current clock.
A current-domain clock regression retires the reducer.

`ClientTime` and `ServerTime` use the actual `client_transmitted`, `server_received`, and `server_transmitted` wire names.
The caller retains bounded envelope parsing and authenticated-message admission.

## Burst contract

`TimePolicy::new` requires explicit reply timeout, burst period, and maximum measurement age.
All three durations must be positive and inside the numeric envelope.
The burst period must satisfy `8 * reply_timeout_us < burst_period_us <= 10_000_000`.
Every absolute deadline uses checked addition within the numeric envelope.

`poll` returns one action, the next absolute deadline, and copied counters.
`PrepareProbe` offers a stable token until emission, cancellation, or offer expiry.
`probe_started` records the actual echoed T1 and response deadline.
Repeated T1 values cannot start another request in the same binding.
Unstarted offers do not count as emissions.
The caller waits for a real clock increment before retrying an equal T1.

Each complete burst contains exactly eight sequential emissions.
Timeout wins at the exact response deadline.
Unmatched replies never extend the pending deadline.
Keep only the earliest candidate with the smallest strictly positive error.
Feed that candidate once after the eighth reply or timeout.
All-invalid and all-timeout bursts leave the filter unchanged.
The filter count stops at 100; diagnostic counters have separate checked lifetimes.

The next burst is due relative to the prior burst's first actual emission.
Record one cadence miss when that start cannot occur within ten seconds.
Discard an incomplete burst at its ten-second boundary.
Suppression abandons only burst work and never authorizes a send.
Activation offers fresh tokens while preserving the connection's filter.
Cancelling an uncertain pending send abandons its entire burst.
The existing session's uncertain-write policy still controls connection shutdown.

## Mapping and player messages

Snapshots distinguish `Uninitialized`, `Mapped`, `Stale`, `NumericFault`, and `Retired`.
Initial covariance remains private until the first accepted sample.
`Mapped` means an estimate exists within the explicit age policy.
It supplies no convergence or accuracy certificate.
Only a completed burst that changes the filter advances the mapping revision.

`map_server_time` and `map_client_time` implement both pinned conversion formulas.
Every mapped value carries its binding and mapping revision.
`port_target` accepts the existing decoded ID-4 fields.
It converts server time first, then subtracts `output_delay_ms * 1000` once in local time.
The delay range is 0–5000 ms.
Send-ahead never changes a playback target.
`arrival_delay` maps `timestamp - send_ahead` for diagnostics alone.
It skips saturated send-ahead values 0 and `u32::MAX`.

## Evidence boundary

Numeric tests use hand-calculated anchors, strict threshold cases, and boundary failures.
The integration tests use actual message producers, reassembly, ID-4 decoding, and reducer consumers.
The portable end-to-end test also exercises the real session's send suppression and re-handshake.
Its microsecond inputs remain explicit, independent caller facts.

The Python peer's per-reply updates and ties-to-even rounding differ from this pinned specification.
Music Assistant's vendored browser filter also has different defaults.
Neither peer supplies this unit's numeric oracle.
No upstream implementation source was copied.

The roleless Network worker now supplies live authenticated clock exchanges.
Its explicit `ClockConfiguration` separates the clock domain from transport and credential admission.
The worker consumes `server/time` before advancing timeout policy.
It abandons incomplete bursts whenever actual send suppression or ordered session blocking starts.
It keeps established estimates through re-handshake and publishes redacted mapping snapshots.
One pending probe uses the existing Network thread and the original socket.

The pinned Python WebSocket tests use eight unique live T1 values and real server monotonic timestamps.
They cover fragmented replies, invalid fields, duplicates, timeout, and re-handshake.
Deterministic session tests preserve submillisecond T4 through acknowledged commit, rotation flush, and admission.
The peer is a transport check; the existing hand-calculated reducer vectors remain the numeric oracle.

Output-clock mapping, live ASRC, and a scheduling latency plan remain unresolved.
These tests establish no AC-17, acoustic alignment, production timing, or synchronized-player completion.
Root owns combined canonical checks and independent contract review before integration.
