# Newest Skyrim SE dynamic schema — issue proposals

Status: local drafts, 2026-10-04. No GitHub issues or comments have been published. The [SE-first plan](dynamic-schema-initiative.md) controls scope; these proposals divide its first wave into reviewable units.

We can begin xEdit/Mutagen source inventory and validator qualification while decompilation runs. Native research resolves disputed or unexposed semantics; it is not a prerequisite for reading either source's definitions. Full interpretation acceptance still requires those research gaps to close.

The RE workbench already pins **Steam Skyrim SE/AE 1.7.104.0, build 24914197**, executable SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`. Its `1.6.1170.0` executable is a static importer-native aid; cross-build applicability needs evidence. The [plan's stack table](dynamic-schema-initiative.md#reverse-engineering-stack-and-existing-evidence) records tools and sources. Full official Data/Creation content remains unpinned.

The existing `mudcrab-reverse-engineering/oracles/records` wrapper uses Mutagen `0.54.4` and .NET SDK `9.0.318`. It already provides a placed-record pilot; extend that owner for full-catalog observations. F0005 records matching counts on `Skyrim.esm`; this draft does not claim a new oracle run or complete tool qualification. The [source inventory kickoff](../research/dynamic-schema/README.md) begins DS1 with pinned declarations and explicit applicability limits.

## First wave and dependencies

Draft IDs below are local names, not GitHub issue numbers. Create one tracking proposal and four initial work issues; defer per-record implementation tickets until the catalog ledger identifies the actual gaps.

| Draft | Proposed title | Plan/task mapping | Start condition | Decompilation dependency |
| --- | --- | --- | --- | --- |
| DS0 | Proposal: complete newest Skyrim SE record interpretation with declarative schemas | P0–P6 | Ready to propose | Final unresolved meanings need native evidence |
| DS1 | Schema S0: pin newest SE inputs and build the xEdit/Mutagen catalog ledger | P0, DST1 | Source inventory started; executable pin known, full official-data manifest remains | None for inventory; native findings carry 1.7.104 applicability |
| DS2 | Schema S1: qualify independent xEdit and Mutagen observation adapters | P0, DST1; prepares P4/DST8 | Start alongside DS1; extend the existing Mutagen placed pilot and qualify xEdit | None for tool qualification; omissions become research items |
| DS3 | Schema S2: preserve every Skyrim SE structural occurrence without mutation | P1, DST2 | DS1 preservation/profile/identity contracts and scoped spec handoff accepted | None |
| DS4 | Schema S3: persist and reopen lossless SE archives with byte-exact output | P1, DST3 | DS3 scanner and snapshot authority accepted | None |

DS1 and DS2 can run in parallel. An unavailable external runner is a recorded prerequisite for the later interpretation campaign, not a reason to stop source inventory or independently provable structural preservation. DS3/DS4 do not require a completed decompilation or catalog semantics; they do require their stated P0/P1 contracts. P2 registry and later semantic integration open only after the complete P1 gate.

## Current coordination

Live read-only check: 2026-10-04. Team `main` is `5579f007ad296c2e7134f1dbf2b8eb8cec5369fd`, matching the plan's audited code baseline. No matching dynamic-schema umbrella issue was found in the open issue list inspected for this draft.

- [#108](https://github.com/Mudcrab-Team/mudcrab/issues/108) owns explicit-list master/light precedence. Include its winning-order contract in later canonical/consumer acceptance; source indexing must preserve supplied order independently of that correction.
- [#110](https://github.com/Mudcrab-Team/mudcrab/issues/110) and open [PR #136](https://github.com/Mudcrab-Team/mudcrab/pull/136), head `136c2d7c9d9fd90a87b954e6828de561e7e3ad53`, own the existing reference/cell remapping fix. Reuse the final merged contract/tests instead of filing that bug again. Its optional-link clearing/dropping policy belongs to the derived runtime view; the immutable archive retains original values and malformed occurrences.
- [PR #105](https://github.com/Mudcrab-Team/mudcrab/pull/105) shares XESP decoding and converter-table changes. Keep one decoder and adopt its final schema contract. [PR #128](https://github.com/Mudcrab-Team/mudcrab/pull/128) consumes optional `door_links`; its producer remains separate work, fed by the canonical reference projection.
- [PR #154](https://github.com/Mudcrab-Team/mudcrab/pull/154) owns MO2 source/profile selection. Snapshot/archive authority must follow its selected effective Data inputs rather than assume a single physical Data folder.
- [#113](https://github.com/Mudcrab-Team/mudcrab/issues/113) is a proposed semver/persisted-schema policy, not an already adopted rule. Coordinate archive/layout/canonical/product version distinctions there; do not create a duplicate release-policy proposal.
- [#129](https://github.com/Mudcrab-Team/mudcrab/issues/129) and [#147](https://github.com/Mudcrab-Team/mudcrab/issues/147) own lighting and grass behavior. Registry fields can feed those consumers without duplicating their rendering/visual acceptance scope.

Refresh live heads/contracts before implementation. Do not import active branches or change their owners as part of these proposals.

## DS0 — tracking proposal body

Title: **Proposal: complete newest Skyrim SE record interpretation with declarative schemas**

Suggested labels: `enhancement`, `proposal`, `area: converter`.

### Describe the problem

Mudcrab's generic parser already accepts unknown record signatures, but the pipeline loses source structure and interprets selected fields through handwritten layouts. Keeping a raw payload does not establish that the record is correctly understood. The current game needs one complete, independently validated data contract before work expands to another Skyrim release or game.

### Describe the proposed solution

Target the recorded Steam Skyrim SE/AE `1.7.104.0`, build `24914197`, and complete its official-data pin at kickoff. Preserve every source occurrence and binary encoding, select declarative physical layouts, expose complete Mudcrab-owned canonical definitions, and validate all signatures, variants, fields/flags/ranges, references and valid corpus occurrences. xEdit is the primary serialization reference; Mutagen is the independent implementation; controlled native research closes omissions and disagreements.

Initial work: DS1/DS2 establish pins, ledger and validators; DS3/DS4 establish immutable extraction and exact archive round-trip. Later waves implement the registry, canonical/runtime bridge, full catalog, integrated compatibility and final acceptance. Raw-only records, unexplained tails and disputed semantics block the 100% newest-SE milestone. Correctly interpreted definitions must remain queryable even when today's engine does not consume them.

**Completion gate:** newest-SE catalog interpretation is 100%, required references/overrides/localization match evidence, archive-reopened output is byte-exact, complete canonical data survives persistence, and Mudcrab's supported converter/runtime compatibility and resource/performance checks pass. Built-in engine reference targets need explicit native evidence; absence from plugin records alone is not a complete reference-validity rule.

Earlier SE and VR follow accepted newest SE; LE follows those; other Bethesda games remain eventual goals after Skyrim compatibility. No other-game reader, identity framework, corpus or OpenMW adapter is part of this minimum target. xEdit/Mutagen work does not wait for decompilation; unresolved native semantics cannot be marked complete while waiting.

### Importance

Core compatibility work for the Skyrim SE game Mudcrab is building.

## DS1 — catalog and target-pin issue body

Title: **Schema S0: pin newest SE inputs and build the xEdit/Mutagen catalog ledger**

Suggested labels: `enhancement`, `area: converter`. Parent: DS0. Plan: P0/DST1; DSV18, DSV24–DSV26.

### Describe the problem

A 100% claim needs a fixed newest-SE target and an explicit catalog/variant/field denominator. Counts from one plugin or one tool cannot define completeness, and native research must refer to the same runtime/content revision as serialized inputs.

### Describe the proposed solution

- Adopt the RE lock's Steam `1.7.104.0` / build `24914197` / executable hash. Complete the official corpus manifest for `Skyrim.esm`, `Update.esm`, `Dawnguard.esm`, `HearthFires.esm`, `Dragonborn.esm`, selected installed official Creation plugins and companion bundles. Record hashes, language, dependencies/load order and missing inputs; do not infer the Creation list from the runtime version.
- Keep runtime, importer-native `1.6.1170`, xEdit/Mutagen source commits and Mutagen `0.54.4` package/runner pins distinct. Transfer older-build evidence only with recorded applicability proof.
- Inventory sourced record declarations, per-release selectors and specialized-codec/reference seams from both upstreams. Keep source path/line/pin and extraction limitations for every candidate. Compare signature sets and review differences without treating agreement as semantic completion.
- Build the catalog/variant/field/range ledger, including absent-from-retail types through source-backed synthetic cases. Separate candidates, independently validated facts, native-research gaps and consumer requirements.
- Capture current structural census, SQL/cache/identity outputs and same-input cold/warm plugin time/RSS baselines for later regression checks.
- Land the scoped dynamic-schema spec handoff while preserving root movement goals/tasks, then accept the P0 preservation/profile/archive contract.

**Gate:** reproducible target/corpus/source manifests, explicit complete-catalog denominator, unresolved-field ledger, current-game baselines and accepted scoped contracts. A provisional source-declaration inventory is useful progress, not this final gate. This issue requires no other-game tooling or completed decompilation.

### Importance

Defines measurable newest-SE completeness and keeps implementation grounded in a fixed target.

## DS2 — validator issue body

Title: **Schema S1: qualify independent xEdit and Mutagen observation adapters**

Suggested labels: `enhancement`, `area: converter`, `area: ci`. Parent: DS0. Plan: P0 adapter spike and P4 prerequisites; DSV12, DSV18, DSV24.

### Describe the problem

Definitions alone do not prove executable validator behavior. Lazy traversal, normalized writer output, unknown-field omissions and diagnostic exit handling can make an incomplete comparison look like a pass.

### Describe the proposed solution

Extend the existing `mudcrab-reverse-engineering/oracles/records` Mutagen `0.54.4` wrapper rather than duplicate it. Preserve its placed-record pilot and qualify full traversal using deterministic synthetic plugins: exact build/invocation/platform, complete enumeration, normalized observations, timeouts/errors and exposed/omitted fields. Force lazy Mutagen records to materialize. Probe xEdit unattended traversal and diagnostic status on supported infrastructure. Retain counts and source identities before winner merging, plus ordered logical subrecords, known values and typed references. Fields a tool cannot expose remain explicit unavailable observations.

Existing F0005 count agreement is useful pilot evidence only. Recheck its artifact/input identity, refresh field-gap candidates against current Mudcrab, and expand beyond `Skyrim.esm` and placed records. Native behavior estimates still require observation on the pinned runtime.

The adapter contract reports unavailable observations and disagreements. Tool exports are semantic re-import checks; Mudcrab's own retained source/writer proves exact physical framing. Source-derived regression cases are not independent corroboration for copied logic. Native research owns disputed/unexposed meanings rather than silently defaulting them.

**Gate:** both adapters reproducibly enumerate a shared synthetic pilot corpus and report observed values/limitations and error behavior. Runner unavailability remains explicit. Later P4 expands to the full target corpus; this pilot does not establish 100% interpretation.

### Importance

Provides repeatable independent evidence before registry and catalog interpretation are accepted.

## DS3 — raw structure issue body

Title: **Schema S2: preserve every Skyrim SE structural occurrence without mutation**

Suggested labels: `enhancement`, `area: converter`. Parent: DS0. Depends on DS1's accepted P0/spec contracts; DS2 runner execution is not a parser prerequisite. Plan: P1/DST2; DSV1, DSV3–DSV5, DSV20, DSV26.

### Describe the problem

Current ingestion drops TES4/group metadata, compression streams and XXXX framing, then remaps and merges source occurrences. Those processing objects cannot reconstruct the imported plugin or provide reliable original-field provenance.

### Describe the proposed solution

Extend the current iterative 24-byte Skyrim scanner with an immutable retained source snapshot and ordered span/node arena. Preserve TES4, all complete headers/reserved bytes, group nesting/order, all records/subrecords/repeats, compression stream and length encodings, XXXX, offsets, overrides and deletions. Occurrence identity differs from owning and winning record identity. Decoded compressed spans explicitly identify their address space.

Preserve unknown semantic data without guessing references or mutating bytes. Enforce checked lengths, exact zlib validation, depth/work/per-record and aggregate decoded-memory budgets. Distinguish malformed, unsupported and resource-limited outcomes; never publish partial completeness. Extend existing structural regression fixtures and fuzz each relevant framing boundary.

**Gate:** zero unaccounted structures/ranges, stable source hashes under interpretation/merging, correct occurrence identities and bounded diagnostics. Complete extraction is a foundation and does not satisfy newest-SE interpretation acceptance. No TES3/20-byte reader is required.

### Importance

Makes source preservation reliable before adding complete declarative interpretation.

## DS4 — archive and writer issue body

Title: **Schema S3: persist and reopen lossless SE archives with byte-exact output**

Suggested labels: `enhancement`, `area: converter`. Parent: DS0. Depends on DS3. Plan: P1/DST3; DSV2, DSV5, DSV15–DSV17.

### Describe the problem

In-memory retained bytes are insufficient if source files disappear, cache eviction removes source authority, or interrupted persistence binds an index to different bytes. Existing plugin-to-SQL tests do not emit reconstructed plugins.

### Describe the proposed solution

Persist verified immutable source blobs and a manifest/span index outside runtime output and the evictable asset cache. Bind scanner/index to the same private retained snapshot; seal blobs before atomic index completion. Validate directory separation, missing/corrupt blobs and recovery/cancellation. Do not hard-link mutable Data-folder inputs into the archive.

Implement a no-op writer over preserved structure and verified spans. Prove import → persist → close → reopen → emit equals input after original source removal, including compression, XXXX, order and metadata. Preserve supplied string-table bundles and explicit plugin-only scope. Interpretation/registry annotations and derived runtime sanitization must never change emitted source bytes.

**Gate:** exact plugin/bundle output and complete structural accounting survive reopen/source deletion; corruption/interruption/resource limits cannot produce a complete verdict; cache eviction cannot delete source authority. Canonical editing is separately scoped. Native decompilation is not needed for this gate.

### Importance

Establishes a durable lossless foundation independent of changing interpretation knowledge.

## Next handoff

Ready now: DS1 source inventory and DS2 qualification work, aligned to the recorded Steam executable pin; the complete official-data manifest and broader validator qualification remain P0 tasks. DS1's provisional declaration comparison has started. DS3/DS4 become implementation work after their scoped contracts/gates. Complete decompilation is not a global dependency; each native-research item names the specific field/behavior it blocks.

Registry, canonical integration and catalog-completion tickets are the next wave after the P1 gate. Earlier SE/VR, LE and other-game tickets stay deferred. These drafts do not claim P0 acceptance, a new external validator run, a parser implementation or 100% record interpretation.
