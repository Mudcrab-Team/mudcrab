# Lighting and color research notes

This corpus carries supplied research into the existing [native rendering map](../README.md).
Each input stays intact. Claim audits link it to authored records, native static
segments, shipped shaders, mod source and current Mudcrab consumers. The
[world model](world-model.md) connects those layers and retains their missing links.
It is an investigation model; selected retail execution and visual parity remain open.

| Input | Preserved source | Audit |
| --- | --- | --- |
| N001: CK/CELL lighting notes | [Original](note-0001/supplied-notes.md) | [Carry-forward claims](note-0001/claims-ck-data.json), based on the [previous comparison](../../skyrim-lighting-notes-comparison-20261010.md) |
| N002: rendering architecture, grading and interception | [Original article](note-0002/supplied-notes.txt), [complete user message](note-0002/supplied-message.txt) | [Native lighting](note-0002/native-lighting-audit.md), [image space](note-0002/imagespace-audit.md), [mod interventions](note-0002/mod-interventions-audit.md) |
| N003: forward lighting, authored data and grading | [Preserved article](note-0003/supplied-notes.md), [13-section map](note-0003/source-sections.json) | [Intake](note-0003/intake-summary.md), [surface lighting](note-0003/native-lighting-audit.md), [image space](note-0003/imagespace-audit.md), [mod interventions](note-0003/mod-interventions-audit.md) |

The [N002 intake](note-0002/intake-summary.md) and [N003 intake](note-0003/intake-summary.md) summarize their findings and cross-note corrections.
The [interactive viewer](model.html) searches claims and follows the conceptual
model. `python3 scripts/build-skyrim-research-notes-viewer.py` rebuilds its HTML
and model from the current audit inputs and curated stage associations.
The shared [model association map](model-associations.json) records reviewed
claim-to-stage links; claims without an association remain explicitly unmapped.

[Input receipts](input-receipt.json) record byte hashes, sizes and origins.
The [first corpus receipt](corpus-receipt-20261010-01.json) and [N003 checkpoint](corpus-receipt-20261010-02.json) pin reviewed artifacts and record their checks.
The generated [claim index](claims-index.json) connects each claim to its audit.
The audit's `supplied_statement` may be an excerpt or an auditor's summary; the
index marks whether it occurs verbatim in the input. A paraphrase is never a
replacement for the preserved article. Unsupported numeric formulas remain
research claims until their operands and consumers are traced.

## Reading a claim

`supported_static` means the cited static evidence supports the stated scope.
`partially_supported` means only a segment or condition is supported.
`contradicted` means cited evidence conflicts with the statement within that
scope. `unverified` means the audited sources do not establish it. These labels
do not grade prose quality or imply active settings, selected shaders or retail
execution. Read the claim's scope and unresolved fields with its assessment.

Every claim retains a stable ID, source statement, evidence anchors, current
implementation comparison and next proof question. Stored fields, source
reconstructions, target instructions, shader arithmetic and runtime observations
are separate evidence kinds. A mod's replacement path cannot fill a vanilla
gap. A version, feature name or constant-buffer parameter ID cannot establish
effective values, GPU binding locations or execution order.

## Adding the next installment

1. Allocate the next note directory and stable note prefix. Preserve the
   source bytes and origin in a new input receipt; retain this initial receipt.
2. Split the note into independently checkable statements. Delegate domains to
   Luna subagents, then review the exact source attribution and proof scope.
3. Trace each statement into the existing map and the owning project consumer.
   Record executable/package or repository revisions and file hashes. Preserve
   conflicting statements and missing data rather than choosing a plausible value.
4. Add cross-note relationships and append corrections with their own evidence.
   Keep accepted inputs and audits intact. A later note can challenge a claim;
   it cannot silently replace its original statement or evidence.
5. Refresh the derived index, validate it, and save a new corpus receipt.
   Implementation and screenshots require their own activation and runtime gates.

An index refresh does not rebuild the model or viewer. Run their builder after
reviewing the new stage associations. Newly unassociated claims stay explicitly
unmapped until that review; discovery does not infer their ownership.

Run `python3 scripts/check-skyrim-research-notes.py --refresh` from the repository
root to rebuild the derived index. Run it without `--refresh` to check inputs,
claim references, retained artifact hashes and current workspace anchors. This
checks papertrail coherence; it does not validate native semantics or visual
matching. Source drift must be reported or checked against the named historical
checkpoint; refreshing an index does not refresh evidence interpretations.
