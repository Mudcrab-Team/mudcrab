# Skyrim movement: first native investigation

This follows the [REA assessment](rea-skyrim-assessment-20261008.md) against
Mudcrab `f40ca1df770e`. The two questions are how an unset race movement slot
resolves a form, and how one native consumer prepares and stores `SPED` values.
The findings below come from static instructions and record scans. Retail
position/time measurements have not been performed.

## Target and references

The copied `SkyrimSE.exe` from the existing t3-dev installation has:

| Property | Value |
| --- | --- |
| File and product version | `1.7.104.0` |
| SHA-256 | `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f` |
| File size | 37,910,440 bytes |
| Format | AMD64 PE32+ |
| Preferred image base | `0x140000000` |

The source and copied executable hashes match. Version came from the PE version
resource, rather than the directory name. Addresses below are preferred image
addresses, not observed runtime addresses. The executable has not been launched.

The complete Steam installation was subsequently copied from `fiji-desktop`.
All 187 files, totaling 20,775,438,302 bytes, match Fiji's sizes and SHA-256 hashes;
its executable has the same hash and version as the target above. The copy used
174 checksum-matched APFS clones of existing local Data files and transferred
the remaining files from Fiji. It contains no symlinks or hardlinks and remains
outside the repository. Fiji is Linux x86_64 with installed Proton runtimes;
that establishes available files, not a verified playable measurement session.

Function seeds use [the 1.7.104 mapping][mapping] at revision
`3623ffd2e5a726ee5e4f3492ac36e2c77adbe6ca`; its SHA-256 is
`cb31890ba1a58703fa226938e613513b00b1e8ccc53b37f7a2d474abda6316b9`.
It contains 435,162 entries. Public names come from [Address Library labels][labels]
at `0379cb6fdaa0d68e69ef9ad1a2d39caa6611ac91`. Names guide the search; instructions
in the hashed executable establish the behavior. No 1.6.1170 addresses were used.

The historical [CommonLib Movement][movement] and [TESRace][race] headers are
pinned to `b93280e832f263dbef44e44cbe2936622a02f91a`. The current
[ActorValues][actor-values] header is pinned to
`94faaed0c60eddd8347767f2d4d29a97c93bde8c`.
[Mutagen's MOVT][movt-schema], [RACE][race-schema] and [DOBJ][dobj-schema] schemas
are pinned to `414371eedc94a3e0c28f082180e0492df67ab8ca`; [xEdit's DOBJ keys][xedit]
are pinned to `9fb016884bec138ea6c7b872cec831537d464c3e`.

All 80 local Data plugins match the [package manifest](../player-movement-plugin-manifest.csv).
That manifest describes the converted package order. A live retail `plugins.txt`
has not been verified, so this is not a claim about a running game's load order.

## Tool validation

The isolated toolchain contains REA 6.0.0, Node 24.21.0, Ghidra 12.1.4 and a full
Temurin JDK 21.0.12.1+1. Official archive hashes and npm integrity were checked.
Ghidra's macOS arm64 native tools were built from the shipped source using Gradle
9.7.1. REA's Ghidra-scoped doctor passed all nine checks.

A source-owned, import-free AMD64 PE tested the same provider before Skyrim.
REA recovered a known null-pointer fallback and floating-point multiplication,
and its address-to-file mappings matched an independent PE-header calculation.
The first native query took 38.309 seconds, including cold import.

The fixture also exposed a decompiler limitation: its inferred function signature
produced pseudocode that omitted the known floating-point multiply, although the
instructions were correct. Decompilation completed, but semantic parity failed.
Behavioral conclusions therefore require instruction checks.

REA selected Skyrim successfully, then its first instruction query failed with
`provider_timeout` after 331.180 seconds. The provider limit was 330,000 ms.
It returned no Skyrim instructions. A smaller request would still require the
full cold import. The failed attempt and its evidence bundle were saved; the
same REA import was not repeated.

A separate persistent Ghidra import used the default analysis profile, a
600-second analysis limit and a 4 GiB heap. It reached that analysis limit and
saved the project successfully. Whole-program coverage is partial. A subsequent
read-only query with analysis disabled returned the eleven requested function
entries and four data spans in 5.238 seconds.

All fifteen file mappings and loaded byte spans match the original PE. Independent
LLVM 21.1.8 decoding agrees on the boundaries and encodings of all 540 requested
instructions, after joining its separate `LOCK` prefix lines. This cross-check
supports the bounded findings below. Incomplete whole-program analysis means
missing references cannot establish that no caller exists.

## Native coordinates

Private evidence IDs identify bounded captures in
`native-seed-verification-1.7.104.json`. Each records the exact target hash,
command, capture hash and PE mapping. Raw bytes and disassembly remain outside
the repository.

| Evidence ID / public label | Address Library ID | Preferred VA | RVA | File offset |
| --- | ---: | --- | --- | --- |
| `race-movt-resolver` / TESRace::GetMovementType | 25307 | `0x1403e64a0` | `0x3e64a0` | `0x3e58a0` |
| `default-movt-helper` / GetDefaultMovementType | 23728 | `0x140393860` | `0x393860` | `0x392c60` |
| `default-object-read` / GetDefaultObject | 11436 | `0x140154440` | `0x154440` | `0x153840` |
| `race-sped-selection` / TESRace helper | 25305 | `0x1403e6280` | `0x3e6280` | `0x3e5680` |
| `maximum-speed-setter` / SetMaximumMovementSpeed | 37943 | `0x1406ae3e0` | `0x6ae3e0` | `0x6ad7e0` |
| `movement-coordinator` / UpdateMovementSpeed | 37941 | `0x1406ae230` | `0x6ae230` | `0x6ad630` |
| `active-type-write` / CopyMovementTypeDataTo | 39601 | `0x140702560` | `0x702560` | `0x701960` |
| `type-data-copy` / TypeData::CopyTo | 39734 | `0x14071cc20` | `0x71cc20` | `0x71c020` |

For these `.text` addresses, file offset is RVA minus `0xc00`, derived from the
section's RVA `0x1000` and raw offset `0x400`. Other sections require their own
mapping. Add the actual module base to an RVA when inspecting a running process.

## Unset movement slots

Observed in `race-movt-resolver`: read the pointer at
`race + 0x478 + 8 * slot`. Return it when non-null; otherwise pass the slot to
`default-movt-helper`. The resolver itself does not bounds-check the slot before
reading the race array. The helper accepts slots 0 through 5 and returns null
for other values. Its cases query default-object indices 173 through 178.

The default-object reader returns the pointer at `manager + 0x20 + 8 * index`
when the corresponding initialization flag is set. In this executable, that flag
array begins at `0xbc0`; the historical CommonLib header places it at `0xb80`.
Applying the old complete layout to 1.7.104 would read the wrong flags.

The reference slot order and DOBJ key definitions identify walk/run defaults as
`DMWL`/`DMRN`. The record scan finds both explicitly assigned to
`NPC_Default_MT`, `MOVT 0x0003580D`, by Skyrim.esm's `DOBJ 0x00000031`.
The winning package NordRace has neither `WKMV`/`RNMV` links nor `MTYP`/`SPED`
overrides.

This supports the current package's default-form choice and establishes the
native fallback mechanism. It does not establish the default manager's live
contents: five later DOBJ records omit these keys, and native loading/merging of
omitted keys remains untraced. It also does not identify the slot selected for
the Player during a particular gait.

## Speed preparation and a direct consumer

Observed in `race-sped-selection`: initialize the output name and three `INAM`
fields from the selected MOVT. Search the race's movement override entries for a
matching MOVT pointer. Use the first matching entry's eleven `SPED` floats;
otherwise use the MOVT's eleven floats. This distinguishes selecting a movement
form from selecting its speed data.

Observed in `maximum-speed-setter`, after successful initialization with valid
actor, race, MOVT and output pointers, and for ordinary finite inputs:

1. Multiply the four linear walk/run pairs by the reference scale when that
   scale is nonnegative. The rotation pair and `rotateWhileMovingRun` are excluded.
2. Read actor value 30 through an indirect call. The reference names this value
   `SpeedMult`. Apply its absolute value times the stored `f32` constant
   approximately `0.01`; replace a zero factor with `1.0`. Multiply the four
   linear pairs by that factor.
3. If `CanRun` returns false, copy walk to run for all five pairs, including
   rotation. Otherwise, for a nonnegative movement-weight factor, set each
   linear run field to `max(walk, run * factor)`.

The reference scale call is at `0x1402e6f30`. Its bounded instructions combine
the reference's scale with an NPC scale result when applicable; the NPC scale
calculation remains untraced. The actor-value call target, `CanRun` conditions,
weight curve and live inputs remain unresolved. This function performs no
elapsed-time conversion or world displacement.

The coordinator's reachable selected-type branch calls this setter, then the
coordinator calls `active-type-write` at `0x1406ae2d4`. That helper takes the high
process pointer from process offset `0x10` and forwards its `+0xf0` region to
`type-data-copy`. When the high-process pointer is non-null, the copy writes all
eleven prepared `SPED` floats and the three `INAM` fields without further scaling.
Forward walk/run reach high-process
offsets `0x108`/`0x10c`.

This is a verified consumer of prepared speed data. The final velocity/position
integrator, directional interpolation, animation-driven type selection and the
active Player path remain unknown. No native `1.5` sprint factor was established.

## Effect on Mudcrab

The [movement record map](../player-movement-record-map.md) can now cite the native
unset-slot branch. Its hard-coded form still covers only the documented package;
a general implementation needs default-object assignments, explicit race links
and race speed overrides.

The current converter exports only `NPC_Default_MT`, and the engine's profile
does not represent these native selection and scaling inputs. Supporting them
requires a converter/schema/controller change with independent synthetic cases,
rather than changing a speed constant. The observed preparation rule can guide
those cases: explicit versus unset slots, race overrides, scaled linear fields,
zero `SpeedMult`, inability to run and a run-speed floor at walk speed.

Keep units, gravity, jump launch, directional blending and sprint behavior
provisional until retail measurements or their native consumers support them.
No engine behavior was changed by this investigation.

For this executable and toolchain, use the saved Ghidra project for further
static work. REA's default cold-import deadline prevented it from serving even
the first Skyrim query. Its fixture success does not establish whole-game
readiness; a provider with reusable projects or a configurable startup limit
would need a separate test.

## Retail capture prepared

The private measurement kit provides a console/video endpoint protocol and an
optional original Papyrus sampler for finer position/time samples. It separates
requested frame limits from observed game FPS, records gait, scale, `SpeedMult`,
gear and interruptions, and compares repeated walk/run/sprint trials. Console
endpoints measure mean displacement rates including acceleration; they do not
establish steady-motion speed.

The stdlib reducer passed six checks using independently authored synthetic data:
known velocity with irregular samples, measured FPS distinct from its requested
limit, trimming to actual retained samples, rejection of decreasing and duplicate
timestamps, rejection of an insufficient window, and parsing of sample read
brackets. These are tool checks, not game measurements. The optional sampler is
uncompiled and unattached; its imports and console control require validation.
No playable retail session or runtime capture has been established.

## Private reproduction

Tools, source references, target, original PE fixture, manifests and evidence are
under the local private research root
`~/.local/share/mudcrab-research/rea-20261008`. `scripts/rea.sh` launches the pinned
toolchain; `scripts/rea-session.mjs` connects a persistent MCP client and saves
complete tool responses. No global agent registration is required.

Relevant artifacts are `toolchain/toolchain-manifest.json`,
`evidence/doctor-ghidra.json`, `evidence/skyrim-target.json`,
`evidence/smoke-summary.json`, `evidence/smoke-bundle.json`,
`evidence/skyrim-rea-attempt-bundle.json`, `references/source-provenance.json`,
`references/dobj-movement-scan.json` and
`references/movement-instruction-notes-1.7.104.json`.

The complete retail copy is `retail/fiji-skyrim`; per-file verification is in
`evidence/retail-transfer/transfer-verification.json`. Persistent Ghidra results
are in `evidence/persistent-ghidra/focused-movement.json` and `crosscheck.json`.
`scripts/crosscheck-ghidra.py` reproduces the byte/mapping/decoder comparison.
`evidence/movement-pilot-bundle.json` indexes the private evidence by SHA-256.
The capture protocol, reducer and their checks are in
`references/retail-measurement-protocol.md`, `retail-movement-reduce.py`,
`retail-reducer-verification.json` and `retail-measurement-kit.json`.

The persistent project uses a path without hidden components because Ghidra's
project locator rejects components beginning with a dot. Its commands, heap,
timing, exit status and analysis coverage are recorded privately. A query with
`-noanalysis` does not establish that an earlier import completed analysis.

[mapping]: https://github.com/alandtse/skyrim_vr_address_library/blob/3623ffd2e5a726ee5e4f3492ac36e2c77adbe6ca/offsets-1-7-104.0.csv
[labels]: https://github.com/meh321/AddressLibraryDatabase/blob/0379cb6fdaa0d68e69ef9ad1a2d39caa6611ac91/skyrimae.rename
[movement]: https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/M/Movement.h
[race]: https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESRace.h
[actor-values]: https://github.com/alandtse/CommonLibSSE-NG/blob/94faaed0c60eddd8347767f2d4d29a97c93bde8c/include/RE/A/ActorValues.h
[movt-schema]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/MovementType.xml
[race-schema]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/Race.xml
[dobj-schema]: https://github.com/Mutagen-Modding/Mutagen/blob/414371eedc94a3e0c28f082180e0492df67ab8ca/Mutagen.Bethesda.Skyrim/Records/Major%20Records/DefaultObjectManager.xml
[xedit]: https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas
