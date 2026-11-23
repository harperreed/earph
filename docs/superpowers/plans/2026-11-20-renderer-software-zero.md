# U2 accepted implementation contract

Doctor Biz, add one exact-zero software setting and zero the cached period's unaccepted suffix on the serial worker. Keep source and output epochs, transport, cursors and replay ownership unchanged. Use one shared remote grid with Volume0..30700. Preserve local finite VolumeSet semantics.

This document records the accepted U2 design before implementation. The integrated behavior and qualification appear in [renderer control](../../renderer-control.md).

## Evidence before implementation

Before U2, Core GainRamp already represented exact linear zero through set_target(None,0). Worker set_gain chose None only for mute. Raw integer playback, PreparedRender and PreparedStreamRender cache native integer periods. Updating their gain target does not change an already cached period. Output acceptance advances only the accepted prefix. PreparedStreamRender keeps the pre-gain replay separately.

Playback::reposition uses native suspend and may rewind a local source. It also emits pause observations. Those effects are unnecessary for a software-zero transition. The selected design does not use reposition, OutputReplay::hold, native suspend, a new output epoch or a source reset.

## Core setting and public contracts

Add software_gain_zero:bool beside the existing finite software_gain_db. This bit is the canonical zero setting, independent of muted. Keep software_gain_db finite and keep its current meaning as the finite setting. Do not encode negative infinity, change mute to express zero, retain a remote percentage, or add another gain ledger.

Add an explicit zero command alongside VolumeSet. A proposed name is VolumeSilence, serialized as volume.silence. Its admitted effect sets software_gain_zero=true and gain_ramp_frames=0. It leaves mute, playback intent, logical transport, queue, source, graph, consent and epochs unchanged. An explicit finite VolumeSet clears the zero bit, including an identical finite value. It retains all existing millidecibel, cap, headroom and ramp checks. A finite set never unmutes.

Reject zero when the selected mode/domain/fidelity cannot perform software attenuation. In particular, zero cannot bypass Fixed, unsupported hardware volume, DSD or strict preservation. Recheck the effective zero setting on unmute, start and graph/profile adoption. Keep existing public finite-only safety methods; add a zero-aware internal path or additive method rather than changing their meaning. No unconfigured intent may authorize an unsupported route later.

Default and restore the zero bit to false under the existing muted, disarmed safe restore policy. Do not add gain persistence in U2. Preserve zero through controls which do not explicitly set a finite level. Output/profile mode changes must validate the combined setting before execution. They must not silently authorize zero in an unsupported mode. FactoryDefaults explicitly clears zero.

Propagate the bit through the existing strict snapshot, diagnostics, Desktop and FFI/CLI views. Show the effective zero setting beside the finite value. Do not display the dormant finite value as the effective gain while zero is selected. Update the canonical gain ledger once and derive its projections. Existing finite command shapes and numerical meaning remain unchanged.

## Worker execution fence

Define the execution fence as successful application of the matching Settings on the serial worker. A Core or SOAP receipt reports admission according to the existing command contract. It does not prove the worker already crossed that fence. Do not promise wall-clock silence or retract a prefix the native output already accepted.

For exact zero, select the common target None with ramp0 in Playback::set_gain. After validating the incoming Settings binding, zero only frames from the current accepted offset to the cached period's frame_count. Raw and fixed-render paths use period_offset. The streaming path uses PreparedStreamRender::period_offset. Add one narrow native-period access/helper for the raw, fixed-render and stream variants. Use the existing integer sample setter or a bounded equivalent. Allocate no buffer and mutate no header or cursor.

Keep pending, period_offset, staged_offset, staged_report ownership, output/replay submission counters and sequence numbers intact. Do not discard or regenerate the period. Continue the normal write/accepted path with its zero suffix. Future periods receive the same zero target through GainRamp. This covers source periods, graph tails and pre-gain replay.

Already accepted prefix samples and counters remain truthful. Do not relabel an earlier positive prefix or its observed peak as silent. Zero is a target setting, not a claim that an entire measured period or native buffer contains only zero.

Logical playing continues to decode, process and submit zeros. Paused remains paused. Preparing stores the latest setting for later preparation. Do not emit Paused or Started solely because zero changed. Existing terminal and drain facts remain intact.

When an explicit finite set exits zero, the common gain starts from exact zero and uses its requested ramp. The already-zeroed cached suffix may finish first, preserving the existing bounded period latency of finite volume changes. Begin the ramp on later generated frames. Never recover a positive suffix by rerunning stateful DSP or rereading a source. Ramp0 selects the finite target for the next newly generated period. This latency must remain explicit in tests and documentation.

Classify the zero command as safety in Core retention, Engine admission and Worker Settings capacity, and Desktop lanes. Match the existing bounded safety delivery policy. Saturation or disconnected delivery must report failure and use the existing fail-closed cancellation path. It must not report completed worker execution. Positive finite volume retains its current ordinary classification.

## One remote grid, without shadow state

The [RenderingControl1.01 specification](https://upnp.org/specs/av/UPnP-av-RenderingControl-v1-Service.pdf), sections2.2.16–17 and2.2.22, links the two scales. Volume zero and the declared minimum VolumeDB mean silence. VolumeDB uses1/256dB units and selects the nearest supported value for an in-range unsupported request. These constraints require explicit projection and nearest-setting rules.

Revise the proposed, unshipped Volume0..100 range to0..30700. This is a bounded accepted plan amendment. It preserves the normalized controller API while greatly reducing projection error. A100-point finite grid spans roughly1.21dB per step. It can misreport a local fine setting by roughly0.61dB. It does not solve the existing finite-state mismatch.

Define Q as a signed integer measured in1/256dB. Use signed64-bit integer arithmetic for wire conversions. Define nearest ties toward the quieter value.

- Volume0 corresponds only to software_gain_zero=true and VolumeDB=-32767.
- Finite Volume v is1..30700.
- q(v)=-30720+nearest((v-1)*30720/30699).
- q(1)=-30720 and q(30700)=0. The30700 finite settings are strictly increasing.
- This grid omits21 interior integer DB codes. Each gap is at most two DB units.
- A finite wire setting lowers to nearest(q(v)*1000/256) millidecibels and clears zero.
- SetVolumeDB selects the nearest member of {-32767} union {q(v)}. Break equal distance toward the lower value.
- Reject -32768 and values outside the advertised range. Include the silence sentinel in nearest-setting selection.
- Apply cap, headroom and mode policy after conversion. A safety refusal is a fault, never silent success or a hidden clamp.

Compute both readbacks from the current Core setting. For finite Core gain, choose the nearest finite q(v), then return v and q(v). Keep mute independent. A muted finite setting still reports its finite volume. Only the zero bit selects the silence sentinel. Use one Core conversion helper for both reads, both setters and later event projections. Use a bounded binary search or equivalent integer calculation, not a retained lookup cache.

Local VolumeSet still accepts all of its original millidecibel values. It changes no value to fit this remote grid. These wire readbacks are quantized observations, not a lossless index of all120001 finite local millidecibel settings. The maximum finite projection error is1/256dB. Lowering a supported wire setting adds at most0.0005dB of millidecibel rounding. Test these separately. Preserve the exact finite value in the local Core projection.

A set of the already reported finite wire setting may be an idempotent no-op after all live authority checks. This avoids small readback/set drift or a cap refusal caused solely by wire quantization. It must not renew grants or reinterpret zero as finite. A different requested setting still uses ordinary admission.

## Pinned controller behavior

Pinned [async-upnp-client0.48.0](https://github.com/StevenLooman/async_upnp_client/blob/0.48.0/async_upnp_client/profiles/dlna.py), lines436–469, normalizes reads by the advertised maximum. Its setter scales a normalized value with the advertised minimum and maximum. Minimum zero is compatible with this path.

Pinned [Music Assistant2.10.12](https://github.com/music-assistant/server/blob/6ab5bfa63f02d0daa237d342097a42feae5c5aae/music_assistant/providers/dlna/player.py), lines151–152 and346–349, uses that normalized API. Its percentage display truncates. Maximum30721 would introduce a downward percentage step for most ordinary requests. For example, requested50 becomes wire15360, then displays49. Maximum30700 sends15350 and that example displays50.

The multiple-of100 maximum avoids that newly introduced near-universal drop. It does not fix all upstream floating-point/truncation edge cases. A pure double-arithmetic trace finds three percentage roundtrip edge cases for both the existing100 maximum and30700. This is source/arithmetic evidence only. U5 must confirm actual pinned runtime behavior and the advertised SCPD. Do not patch or monkey-patch the controller in U2.

## FactoryDefaults and renderer authority

Implement FactoryDefaults as one Core mutation. In supported software mode, mute, clear zero, and set min(-40dB,current cap), using existing headroom checks. In supported Fixed mode, mute and disarm while preserving unity. Reject a mode whose complete preset behavior cannot be performed. Preserve output, consent, caps, source, queue and authored graph. List only supported presets.

Future renderer gain and preset mutations require current observed-peer/controller authority and the exact active owned source binding. A staged association alone grants no right to change unrelated local playback. Lower admitted requests into the same Core controls and serial Settings path. U2 adds no production listener or U4 execution bridge.

## Required real tests and gates

1. Prove a behavioral RED with a positive cached period and an actual accepted native prefix. The remaining positive suffix must appear without the fix.
2. Reuse worker::tests::staged_streaming_playback and the existing actual partial native write seam. Its test writes a bounded prefix directly to the real output, then calls accepted with the actual returned count. Extend this seam for zero.
3. Exercise raw, fixed-render and streaming variants. Assert accepted prefix unchanged, every later submitted suffix sample zero, unchanged headers/cursors/source epochs and no extra source or DSP consumption. Keep the worker's pending state consistent.
4. Capture real nonzero generated audio through the existing owned ALSA file route or equivalent software output. Use signed24 or inspect prequantization f64 to distinguish exact zero from finite -120dB. Native-null-only counters do not prove sample values.
5. Exit zero with ramp0 and a finite ramp. Prove the permitted zero remainder and the later exact ramp. Preserve mute, source identity and logical transport. Cover pause/resume, preparation, graph tail, replay/hold and terminal drain.
6. Cover refused modes, cap/headroom failures, state rollback and safety reserve saturation/disconnection. Test actual FactoryDefaults state and worker settings.
7. Test every supported wire volume roundtrip. Test bounds, sentinel-nearest transitions, every omitted DB code, quieter ties, arbitrary local millidecibels and readback/set idempotence. Assert numerical error limits rather than exact wire preservation of local values.
8. Pass actual Core command serialization, strict diagnostics/Desktop consumption, and actual CLI/FFI owner paths. Run scoped checks, canonical scripts/check and one independent high-risk review of the final implementation.
9. Keep actual Music Assistant normalized read/write and remote event/SOAP qualification in U5. Source inspection does not qualify its runtime.

No application tests, Cargo, native output or Python ran for this design review. The arithmetic exploration is retained as design arithmetic only. Primary source bytes and hashes are retained beside this report. No repository edits, installations, permissions, deletion or external writes occurred.

## Implementation bindings

This document preserves the accepted design report, SHA256 `8735cdd4ee107f4af595073f5e4c173658e373be36e706f8108909f7427df4f7`.
The pinned source line references above were corrected against the retained source bytes.

Doctor Biz accepted an explicit `rendering_control` controller right for the new gain and preset actions.
Existing `transport` grants keep their current permissions.
The scoped actions are `set_volume`, `set_volume_db`, and `factory_defaults`.
Each action requires its exact currently executable owned source binding.
Remote gain setters require supported software attenuation before mutation or a reported-value no-op.
FactoryDefaults retains its separately qualified Fixed-mode behavior.

Local commands are `volume.silence` and `volume.factory_defaults`.
`remote_volume::RemoteVolume` derives both wire scales from the canonical finite value and zero bit.
`Session::renderer_presets()` lists presets whose complete effect satisfies current mode, cap, and graph headroom.
U2 adds no production renderer listener or execution bridge.

Finite exit preserves a previously zeroed cached remainder before starting the requested ramp on new frames.
It never regenerates cached DSP output or rereads the source.

The worker may install a graph before Core adopts its activation acknowledgment.
Matching Settings can therefore retain the preceding graph counter while already naming the current output epoch.
Keep the existing output-epoch Settings fence; a separate graph-counter fence would drop valid exact-zero settings.
FactoryDefaults waits for that acknowledgment before selecting a finite level against active graph headroom.
