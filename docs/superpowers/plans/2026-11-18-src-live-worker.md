# Live SRC worker

Keep Session authoritative for consent, activation, transport, and completion.
Use the existing prepared stream wrapper for gain, quantization, replay, and actual counts.
Return every live graph to the already-open native format.

## Ownership and approval

Doctor Biz authorizes autonomous implementation and parallel work.
This unit uses generated files and software native outputs only.
Root owns GraphService preparation and the stream wrapper dependency.
Preserve those dependency copies outside this unit's commit.
Root owns integration, publishing, and later platform qualification.

## Contracts

Advance packet offsets only by accepted source frames.
Cross-check successful steps against the stream's source counter.
If a pump fails, reconcile its checked source-counter delta within the offered span.
Reject impossible deltas rather than clamping them.
Advance native submission only by accepted output frames.
Keep decoder packets and kernel history across holds.
Keep source epochs distinct from output epochs.
Publish prefetched readiness under current control epochs.
Reset decoder, stream, and replay together on true seek.
Replace a streaming graph at its exact accepted source frontier.
Publish that source boundary and discarded held output.
Keep the boundary visible when the replacement uses a same-rate graph.
Keep streaming audible positions unknown.
Finish only after final source EOF and drain only after all stream output is submitted.
Retire candidates off-thread without cancelling the shared source.

Streaming observations report accepted input frames and native output ledger positions.
They share prepared stage facts through the existing timing Arc.
Conservative resident bytes remain separate from exact audio payload bytes.
Same-rate audio payload bytes retain their numeric wire values.
Streaming audio payload bytes serialize as null because exact payload accounting is unavailable.
No aggregate SRC latency or acoustic position is inferred.

## Verification

- [x] Reproduce the decoded-frontier and streaming-rewind failures before changing production behavior.
- [x] Compare real owner captures with standalone SRC kernels across empty and chunk boundaries.
- [x] Verify one continuous guard across queue joins and running pause/resume.
- [x] Append a source after a hold and consume it under retained source epochs.
- [x] Verify true seek against an independently rendered exact source suffix.
- [x] Verify streaming replacement against the accepted source frontier and real captured suffix.
- [x] Verify real native write failure revokes consent and retains actual progress.
- [x] Verify holds at source EOF and before native drain do not restart finish.
- [x] Verify native stop preserves the actual written prefix.
- [x] Continue a real native period after a separately acknowledged seven-frame prefix.
- [x] Preserve a native write acknowledged after source cancellation.
- [x] Keep queued pause, seek, settings, and activation from erasing a cancelled source's native progress.
- [x] Retain coherent pre-control observations when cancellation interrupts an actual hold or decoder seek.
- [x] Reconcile source accepted before a real pump cancellation error.
- [x] Run canonical checks and one independent main-model review.
- [x] Prepare the reviewed seven-file commit and keep root dependency copies separate.

The initial runtime observation test also fails before implementation because streaming counters are absent.
Later lifecycle, boundary serialization, partial-period, and staged-hold coverage extend the passing implementation.
Those later scenarios have no separate test-first failure receipt.
The initial holds reproduce the missing hold path before implementation.

The real stop scenario also fails before its cancellation fix.
Cancellation can arrive inside a stream call after the worker's entry check.
Treat that source cancellation as ordinary stop only when the actual shared token is cancelled.
Close native output before reporting cancelled completion.
Keep a real native close error on the existing failure path.
The corrected scenario completes and its capture matches the standalone kernel prefix.

Independent review finds another cancellation window after an irrevocable native write.
The deterministic native-prefix test reproduces rejected acknowledgment after actual source cancellation.
The wrapper now acknowledges committed native spans after cancellation while retaining its failure and span checks.
The worker regression compares its stopped observation with the actual native submission count.
Pump, stage, finish, and subsequent work still reject cancellation.

Queued pause or seek can also encounter a source already cancelled by Stop.
Those transport calls previously suspended native output before failing the stream's cancellation check.
Their action-error path lost the observation needed by the following Stop.
Gate retired streaming sources before transport effects and candidate installation.
Let the existing stop path retain actual progress and propagate real native close errors.
The real native regression preserves submitted frames across queued pause, seek, settings, and activation.
Entry guards cannot close cancellation windows inside native suspend or stream reset.
Retain one coherent source snapshot before streaming controls mutate decoder or output state.
If actual source cancellation interrupts a control, close native output and preserve that snapshot.
Keep genuine close errors on the existing stop failure path.
Real post-suspend hold and decoder-seek regressions exercise the same production action cleanup.
Clear reservations under both captured and current exact epochs when retiring a partly changed control.
The regression also reproduces a reservation left behind under the pre-control epoch.

Independent review also identifies source acceptance before a failed pump returns cancellation.
Doctor Biz approves a checked source-counter delta when no successful step exists.
The real concurrent cancellation regression reproduces 2,048 accepted frames against a stale 1,984-frame worker frontier.
The corrected worker reconciles that actual prefix before handling the error.
The test uses a real Quality SRC call and bounded cancellation attempts without a synthetic fault mode.
No output acceptance is inferred from a source-counter delta.

The software NULL route accepts full offered periods in these scenarios.
Native partial-write behavior is therefore not claimed by these owner receipts.
The worker remainder test submits an actual native prefix and then completes its staged period.
It verifies the retained offset without claiming a short native return or WouldBlock.
The reviewed wrapper's real partial-submission tests remain its component evidence.
No admitted integer source currently triggers an actual island numeric failure in these scenarios.
Failure-node mapping uses each island's actual schedule and checks authored identity.
No policy mutation or synthetic runtime fault is used to claim that failure path was exercised.

## Settled verification

The final canonical command exits zero with 603 passed tests and three ignored tests.
Strict workspace Clippy, formatting, contract catalog, and dependency checks pass without warnings or errors.
The final source matches the settled source and dependency manifests.
The canonical log SHA256 is `fc74a357635dacf2fcd6cee33d5e64666232a0851eda9afc9a4cadcbf7daca6f`.

One independent main-model reviewer approves the combined worker and preparation implementation after the recorded fixes.
The reviewer reruns 77 library cases, six wrapper cases, 34 service cases, and six real executable scenarios.
The final reservation hunk also passes the reviewer's focused control regression.
The review capsule SHA256 is `f8d52554b3c67a5c15d66751eee68d4b588e01c100c37c9ff7d594ac75ca8a0d`.
The review excludes physical playback, CPU qualification, and forced native short-return claims.
Root retains the four preparation dependency files for the combined integration snapshot.
