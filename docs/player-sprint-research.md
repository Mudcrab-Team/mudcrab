# Sprint evaluation: evidence and implementation boundary

Investigation date: 2026-10-15. Tracks #264. This is a partial result, not a recovered sprint equation or a claim of retail parity.

## Reproducible baseline

Implementation base: #204 head `a83d61ab10f740f02c0bce3589429c8c4c09590c` (schema-driven in-house reader). Also inspected main `ca33e3ddb571c069ae869cbcb13c655db3a67118` and #246 head `0524aab7a3875651ec57d7f03bfdfa058ef3371f`. The in-house actors schema already classifies all six base movement links as FormIDs, and its decoder remaps them into canonical bytes. The correction here aligns #204's retained legacy `form_id_layout` with that schema. Future sprint runtime resolution should consume #204's typed fields and provenance, not introduce another parser. No executable, installed game data, behavior graphs, INI capture, or retail runtime was available in this environment. No new native disassembly or measurement is claimed.

`MovementTuning::target_speed` currently evaluates:

- Select walk column only when slow is requested and sprint is false; otherwise select run.
- On main, both use the fixed NPC_Default_MT profile. With #246, select the walk or run profile according to that PR's resolver.
- For input `(sx, sy)`, use lateral and longitudinal speeds from that profile: `base = (lateral * abs(sx) + longitudinal * abs(sy)) / max(abs(sx) + abs(sy), 1)`.
- When sprint is latched, return `base * sprint_multiplier`, whose default is `1.5`.

The raw input components here are keyboard values, not an independently recovered native directional blend. `walk_intent_system` normalizes the world-space wish direction separately. `integrate_walk` applies the provisional acceleration and fixed-step displacement; Rapier resolves contacts. The target is therefore distinct from measured displacement per second. A forward run target of 370 produces a sprint target of 555 under the existing rule; that arithmetic is not independent evidence for Skyrim.

The runtime does not currently select SPMV/DMSP, inspect a native sprint graph, evaluate SpeedMult or stamina, or distinguish player and AI sprint equations. Alt toggles a latch during any nonzero movement, including backward movement. Tests asserting `run * 1.5`, including collision tests, establish Mudcrab's behavior only.

## Primary evidence and its limits

| Evidence | Established fact | Not established |
| --- | --- | --- |
| [xEdit RACE definition](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L9463-L9468) | WKMV, RNMV, SWMV, FLMV, SNMV, SPMV are nullable MOVT FormIDs. | Which player/AI consumers use each slot or which SPED column is evaluated for sprint. |
| [CommonLib TESRace](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESRace.h) | Six runtime slots include `kSprint`. | An end-to-end sprint equation. Runtime C++ layout is not a serialized byte layout. |
| [CommonLib Movement](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/M/Movement.h) | `MaxSpeeds` has left/right/forward/back/rotation pairs indexed walk/run, plus rotate-while-moving-run. `TypeData` includes MNAM and INAM fields. | There is no separate sprint float in this structure; the header does not prove that sprint uses the selected MOVT's run column directly. |
| [xEdit MOVT definition](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L7480) | MOVT SPED contains directional and angular values, with INAM thresholds separate. | Thresholds are not proven speed multipliers. Angular values must not be consumed as linear speeds. |
| [Native selection findings from #246](https://github.com/Mudcrab-Team/mudcrab/blob/0524aab7a3875651ec57d7f03bfdfa058ef3371f/docs/player-movement-record-map.md#native-default-movement-selection) | Reported race getter selects non-null slot, otherwise slot-specific DOBJ. DOBJ loading merges keys. | That getter's callers, player/AI applicability, actor overrides, mixed profiles, or speed evaluation. This investigation reviewed the report, not the executable. |
| [CommonLib ActorState](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/A/ActorState.h) | Runtime sprinting state exists. | The speed equation or the complete rules for entering that state. |
| [TrueDirectionalMovement sprint hook](https://github.com/ersh1/TrueDirectionalMovement/blob/57b913ae17fed857c9e281799174898cd66a3e82/src/Hooks.cpp#L1568-L1636), [offsets](https://github.com/ersh1/TrueDirectionalMovement/blob/57b913ae17fed857c9e281799174898cd66a3e82/src/Offsets.h) | This mod's replacement gate consults stamina, encumbrance, attack/controller state, directional input, mount state and sprint flags. It references `fSprintStopThreshold` for lateral stopping. | This is modified plugin behavior, not an instruction-backed reconstruction of every retail condition. The threshold is not evidence for a sprint-speed multiplier. |
| [CommonLib actor/controller declarations](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/B/bhkCharacterController.h) | Controller has velocity, scale and speedPct state; Actor exposes speed getter virtuals. | Names alone do not establish multiplication order, units or participation in sprint. |

#246's native report scopes its getter evidence to SkyrimSE.exe 1.7.104.0, SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`, getter RVA `0x3e64a0` and default selector RVA `0x393860`. It reports DMSP -> MOVT `000F3469` (`AIControlledNPC_Sprinting_MT`) in the examined masters. The EditorID is a warning to investigate consumers, not proof that the player does or does not use it. Neither those RVAs nor these findings are established for LE or VR.

## Source and precedence contract

1. Canonicalize plugin-local FormIDs using the plugin's actual master list and active normal/light load-order mappings before retaining records. Preserve the winning RACE and MOVT identity and contributor plugin.
2. At the *race base-slot getter boundary described by #246*, a non-null SPMV precedes DMSP. DMSP merges by key across DOBJ contributors; a later DOBJ with unrelated keys must not erase it. Missing/uninitialized defaults may yield null natively. Do not invent a vanilla FormID fallback and call it authored resolution.
3. xEdit also defines RACE repeated `MTYP` + `SPED` override pairs and `MTNM` names. Native selection and override application for those pairs, animation movement type selection, and player-versus-AI paths still need tracing. Base-slot precedence is not a complete actor precedence rule.
4. Actor SpeedMult, scale, equipped/weapon/combat state, graph/root motion, stamina, encumbrance, mounts, and relevant settings are candidates to test separately. This report does not assert that each participates in the final speed magnitude. Separate eligibility from speed, animation rate and camera effects.
5. Only after identifying the actual consumer should a typed profile calculate the native speed and adapt units for Rapier. Mudcrab currently assumes MOVT linear values are Creation units/s with factor 1. This investigation has not independently verified that time base, Havok conversion, diagonal response, clamp or actor modifier order.

No evidence found here supports replacing `1.5` with a GMST, a race acceleration value, an INAM threshold, a copied vanilla speed, or an unconditional SPMV run column. The existing multiplier remains an explicitly provisional legacy fallback, not a successful resolution of #264.

## Supported correction in this change

#204's legacy `form_id_layout` recognized WKMV and RNMV but omitted the other four base movement slots. Its in-house decoder already handles all six through `records/families/actors.json` and `decoder.rs`. Consequently SPMV could retain a plugin-local identity in the archived winning RACE even after the referenced MOVT had been remapped. This change extends the legacy fixed layout for all six according to the pinned serialized definition and reuses #204's existing clear/drop/remap policy. The in-house schema and decoder are unchanged. No schema or runtime formula changes are introduced.

Regression coverage varies global plugin index (3 and 7), normal versus light plugin mapping, and all six link values. It also checks a null link and unrelated DATA bytes remain unchanged. An additional procedural-plugin publication test compares legacy and in-house canonical links in both returned records and archived SQLite output. It varies normal/light ownership and authored target IDs (0x340 and 0x440 families), and checks winning load-order metadata. Existing walk/run coverage remains. These are identity regressions, **not** converter-to-runtime sprint override acceptance. Old archives must be regenerated from source plugins to repair already-retained local links; an annotation pass cannot generally recover the original local master context. No cache transformation/version claim is made by this partial fix.

## Required native trace and discriminating experiments

Before changing the runtime equation:

1. Obtain a legally installed executable and masters, record edition/version/executable SHA-256, plugin hashes and ordered master lists, winning assets/graphs, settings, actor race/sex/scale and relevant actor values. Keep proprietary binaries out of the repository.
2. Trace all callers of the reported race getter (the report identifies five direct callers), plus indirect/controller consumers. Identify the player sprint branch separately from AI and mounted paths. Record function/RVA, branch condition, loaded fields and arithmetic, including which MOVT column and any RACE override table lookup. Follow graph/animation-driven translation rather than assuming every movement is controller-driven.
3. Create procedural overrides with unmistakably distinct walk/run values in fresh MOVTs. Change one input at a time: RNMV; SPMV; DMRN; DMSP; matching MTYP/SPED; then actor SpeedMult and scale. Include several non-vanilla values, null/unset/deleted/wrong-type links, and a DOBJ override listing only an unrelated key. Repeat in player and AI contexts. A no-effect result is useful evidence about scope, not a reason to force participation.
4. Independently vary INAM and sprint-stop settings to distinguish transition/eligibility changes from steady speed. Vary stamina and encumbrance for state gating. Hold animation/assets constant, then vary a winning animation/graph separately where legally feasible.
5. Measure elapsed time and flat unobstructed steady-state displacement after acceleration settles; log controller/graph/actor sprint state and target versus actual velocity. Include forward, lateral, backward and diagonal probes, standing starts, release and blocked contacts. Establish tolerances before fitting. Record first/third person, FPS/time scale and player input mode. Do not infer velocity from FOV or animation cadence.
6. Once the equation is recovered, add procedural-plugin -> conversion -> SQLite -> typed resolver -> `walk_intent_system`/fixed-step consumer tests. Assert runtime sensitivity to each *verified* authored dependency and independence from irrelevant fields, provenance, state updates and old-package fallback. Retain matched retail probes as an opt-in oracle. Until then the sprint runtime acceptance criterion remains open.

Provenance at that future boundary must retain the selected race and MOVT, field/column, winning plugins and DOBJ contributor if used; raw override values; effective actor state; transformation/version and units; final target; and a disposition such as authored/native-derived/legacy fallback/unsupported. Do not attach the walk/run source string to the multiplier as if it explained sprint.
