# Shared SRC facts

Share immutable compiled SRC facts with Core observers.
The approved base is `f6a3eae`.
Root owns Engine integration and upcoming GraphPreview fields.
This unit owns Core fact definitions and DSP metadata ownership.

## Success criteria

- Keep one definition for each shared fact type in Core.
- Preserve existing DSP paths through ordinary reexports.
- Preserve serialization bytes, field order, struct sizes, and numerical output.
- Preserve parameters, fingerprints, algorithm versions, and resource figures.
- Share one stage array through `Arc` without observer copies.
- Keep facts alive after compiled plan and runtime retirement.
- Preserve the existing stage slice and stream-plan conversion APIs.
- Verify actual allocation bounds through existing compiler accounting.
- Pass canonical checks and independent review before integration.

## Decisions

Move SrcResources and SrcParameters into Core dsp_nodes.
Move CompiledStreamStage and CompiledStreamStageKind into Core graph_control.
Add PartialEq to stage facts for observable Core structures.
Do not add Eq to floating-point facts.

Store compiled stage metadata in `Arc<[CompiledStreamStage]>`.
Expose `stage_metadata()` as an allocation-free Arc clone.
Keep `stages()` as a borrowed slice.
Keep `into_stream_plan()` unchanged.
Retain the existing two MiB compiler metadata reserve.
Its charge covers transient Vec and Arc backing arrays during conversion.
The admission limit bounds these arrays to 128 stages.
The measured compiler-to-PreparedStream test retains the shared facts through construction.

## Evidence

The new ownership scenario started red because the Core stage type and getter were absent.
The focused rate-compiler suite now passes fourteen scenarios.
The ownership scenario verifies zero clone allocations and exact array sharing.
It serializes real compiled facts through their Core type after runtime retirement.
The existing full preparation allocator test still fits resident and peak admission bounds.

A throwaway capture harness lives under ignored `target/src-fact-baseline`.
It reuses the real serial compiler and PreparedStream scenario.
Its baseline and changed JSON receipts match byte for byte.
The receipt includes the serialized stage array, plan hash, output digest, resource figures, and sizes.

Stage JSON SHA-256: `6226397c5001a568111b0e83b150060cfd7a38d581bfc5a2ad6bb0979fa0df14`.
Plan hash: `8db855dd3a67f8b29b8306b18e8c03a40d9e110973b84fc8045288c276eb24e0`.
Output SHA-256: `20870337f401bf6ac35c0b461c0f27245bd4599de1865ed4f8999de53262f599`.
The real scenario emits 2782 interleaved samples.
Resources remain 7403644 resident bytes and 8451746 preparation peak bytes.
Sizes remain 16 bytes for resources, 96 for parameters, 280 for stages, and 192 for stage kinds.

The canonical check passes 515 tests, with three ignored and no failures.
Formatting, strict workspace Clippy, requirement checks, and dependency checks pass.
The scoped final diff review found no further defect.
Independent main-model review approves the exact source delta without material findings.
The reviewer passes fourteen real release compiler scenarios and verifies the unchanged producer receipts.
The review capsule remains under `target/shared-src-facts/independent-review.md`.

## Scope and handoff

No numerical, scheduler, lifecycle, or GraphPreview code changes belong to this unit.
Do not use hardware, private media, push, delete files, reset, or bypass hooks.
Root accepted the independent review before the named unit commit.
Root will integrate the shared Arc into live Core observability afterward.
