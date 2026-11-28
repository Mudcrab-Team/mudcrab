# Player movement record map (Skyrim Special Edition masters)

The directional Riverwood WALK speeds remain a first-pass profile. Character jump semantics now follow the [native investigation](research/skyrim-physics-221.md) and [implementation evidence](research/skyrim-physics-221-implementation.md). The record values below came from the retail ESM files at a local unmodded `Skyrim Special Edition/Data` directory and a locally converted Riverwood package (`<package>/assets/skyrim_world.db`). The [source manifest](player-movement-plugin-manifest.csv) lists all 80 package plugins in load order with SHA-256 checksums verified against the installed files. The table below highlights the relevant priorities.

| Priority | Plugin | SHA-256 |
| --- | --- | --- |
| 0 | Skyrim.esm | `e198c3b85e5e48e0c92a6580d8f66e644256b68d812ee32b61735cf9b753df73` |
| 1 | Update.esm | `b298b0f65fa0127fd13c1f5ef6bb30eaf9d21fd19f5e6f8f91f829c9007d3280` |
| 2 | Dawnguard.esm | `fc8f92cb7ed55217046ee96b84caa54c058c12f7c408ac2a71da880a7bd3eab7` |
| 3 | HearthFires.esm | `0f681496771e6d1983c6c1c9dd6602964068837f015bfa21e69a149c3b050047` |
| 4 | Dragonborn.esm | `635f21a938a17b8677f524cf41210933afc7f23820f2e210ab3bb6d3cd837046` |
| 22 | ccbgssse018-shadowrend.esl | `7ffcdfd2882445f25db3c5b4d40de7efa28dcca03d8eceb7a3e155e3c6618cfc` |

The winning Player `NPC_` `0x00000007` comes from `ccbgssse018-shadowrend.esl` and has `RNAM = 0x00013746` (`NordRace`). Update.esm supplies the winning NordRace record. Its `RACE` data has no `WKMV` or `RNMV` subrecord and no embedded `0x0003580D` FormID. CommonLibSSE-NG exposes six runtime `baseMoveTypes` slots. An unset slot falls back to a default movement type from the game's default-object table (see [Default movement types](#default-movement-types)). Skyrim.esm's `DOBJ` maps the walk and run defaults to `NPC_Default_MT` `MOVT` `0x0003580D`, so NordRace walks and runs with `NPC_Default_MT` through that table, not through a record link. The loader resolves an explicit `WKMV` or `RNMV` link and an unset slot independently, and it reports a missing default object or movement type as an error.

`MOVT` `SPED` is eleven little-endian `f32` values. CommonLib's `Movement::MaxSpeeds` identifies four direction pairs, one rotation pair, and `rotateWhileMovingRun`. The retail `NPC_Default_MT` values are:

| Direction | Walk | Run |
| --- | ---: | ---: |
| Left | 80.09 | 370.00 |
| Right | 79.75 | 370.00 |
| Forward | 80.10 | 370.00 |
| Back | 71.93 | 205.25 |

The last three floats are `3.14159`, `3.14159`, `3.14159`; they are rotation values, not linear speed. The engine treats the first eight raw numbers as Creation units per second with conversion factor `1` for this first guess. The header proves the field order but does not prove the game's elapsed-time equation or physical units. For our fixed-step controller, steady flat-ground distance is `effective_speed * elapsed_seconds`; acceleration and contacts change distance from rest. Thus forward walk/run targets over two steady seconds are `160.20`/`740.00` Creation units with this profile.

Skyrim.esm contains `GMST` `fMoveCharWalkBase` `0x0001EC72 = 100.0` (`f32`) and `fJumpHeightMin` `0x000ABEF6 = 76.0` (`f32`). The walk setting is still not substituted for a `MOVT` speed. Native jump observations now establish that the loaded jump setting is multiplied by reference scale and NPC/race height before conversion to Havok length and impulse construction. The measured baseline uses `76 × 1 × 1 × 1.03`, yielding a float32 setter argument of `78.27999877929688` Creation units. That value is a launch parameter, not a guaranteed apex.

Startup reads the effective `fJumpHeightMin` by editor ID, canonical Player `NPC_` 7 `ACBS` sex and `NAM6` height, and the normalized `RNAM` race's canonical `DATA` height pair. It attributes records through `plugins.priority`, which is the exported load order; `plugins.id` is a separate database row ID. Modern packages require complete supported records. An inventory-only template leaves local jump traits in effect. The loader rejects malformed data and players with Use Traits set. It reports a missing or zero template separately from an unsupported inherited trait source. The runtime player currently uses reference scale one. Save mutations, inherited actor traits and swimming are outside this supported profile. Table-less legacy packages and the asset-free fixture explicitly use a synthetic unit actor with height setting 76.

The retained recordings cover stationary launches on flat ground with unit controller scale. In that setup the character law keeps velocity in Havok units. It derives launch speed from `sqrt(2 × abs(gravity) × abs(effective_height))`, applies gravity on that same launch step, then advances position using the updated velocity. Native world gravity is float32 `9.8100004196167` downward. The Creation/Havok conversion constants are float32 `69.99124908447266` and `0.014287499710917473`. Strictly exceeding the previous maximum resolved height uses gravity multiplier one; equal or lower height uses float32 1.35, including the first launch step. The apex diagnostic measures actual resolved height above takeoff. When launching from a descending glide, Mudcrab replaces the prior vertical velocity with the launch impulse. The recordings do not establish how Skyrim carries glide velocity into takeoff.

The production schedule remains fixed at 60 Hz and passes its actual fixed-clock delta to the transition. Native scheduler ownership and high-cadence behavior remain unresolved. The former 900/340 character approximation is retained only in the before-change evidence. Rigid-body gravity remains a separate 900 Creation units/s² policy because native character measurements do not establish rigid-body acceleration. `fJumpFallHeightMin = 600` and `fJumpFallVelocityMin = 700` remain fall-related settings and are not treated as gravity.

Sources: the five ESM hashes above; [`Movement.h`](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/M/Movement.h); [`TESRace.h`](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESRace.h); [Mutagen MOVT schema](https://github.com/Mutagen-Modding/Mutagen/blob/dev/Mutagen.Bethesda.Skyrim/Records/Major%20Records/MovementType.xml). ESM `MOVT`/`GMST` subrecord bytes and FormIDs were read directly from those files.

## Default movement types

Question: which `MOVT` does a race use when its `WKMV` or `RNMV` is unset?

Evidence: static analysis of SkyrimSE.exe 1.7.104.0 (SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`). Addresses are virtual addresses at the image base `0x140000000`.

- The race loader (`0x1403e3da0`) reads `WKMV`, `RNMV`, `SWMV`, `FLMV`, `SNMV` and `SPMV` into six consecutive slots. An absent subrecord leaves its slot empty.
- The race movement-type getter (`0x1403e64a0`) returns the slot of the given index when it is set. Otherwise it returns the default object for that index (`0x140393860`).
- That function maps the indexes 0 to 5 to the default objects "Default MovementType: Walk", "Run", "Swim", "Fly", "Sneak" and "Sprint". Their `DOBJ` use codes are `DMWL`, `DMRN`, `DMSW`, `DMFL`, `DMSN` and `DMSP`.
- The default-object loader (`0x14031b8b0`) sets the slot of each `DNAM` (use code, FormID) pair that a `DOBJ` lists. It does not clear the table first, so a later `DOBJ` override changes only the keys it lists.

Data: Skyrim.esm `DOBJ` `00000031` maps `DMWL` and `DMRN` to `0x0003580D` (`NPC_Default_MT`). Update.esm, Dawnguard.esm, HearthFires.esm and Dragonborn.esm override the same `DOBJ` with lists that hold no `DMWL` or `DMRN`. Last-record-wins would therefore drop the defaults, so both record readers merge `DOBJ` overrides key by key. For retail data both slots resolve to `NPC_Default_MT`, which is the value that the earlier project rule chose.

What the converter and engine do: the converter exports every `MOVT` into `movement_types` (with zero speeds kept; a selected zero forward speed loads and is flagged with a warning) and the merged pairs into `default_objects`. The engine uses the race's own link when it is set, and otherwise `DMWL` for the walk slot and `DMRN` for the run slot. A default object that is missing, or that names a movement type the package lacks, is an error. The engine does not choose a movement type for it. A package with no `default_objects` table was converted before the table existed. It is the only case that keeps the earlier rule, `NPC_Default_MT` for an unset slot. The speed source labels it, and the engine logs one warning at startup that says a metadata rebuild (`--reuse-assets`) publishes the table. A package that has the table but no row for the key, including an empty table, is an error.

Not established: which callers use the getter, whether actor state (combat, weapon or sneak overrides) replaces its result, and how the game blends walk and run speeds when the two slots name different movement types. The engine takes the walk speeds from the walk slot's profile and the run speeds from the run slot's profile. That split is an engine rule. No in-game measurement was taken. The `DOBJ` scan covered the five masters; the other plugins in the [manifest](player-movement-plugin-manifest.csv) were not checked.

Tests: `both_record_readers_publish_the_same_default_movement_types` (converter), `default_object_overrides_merge_by_key`, and `a_race_without_links_takes_the_default_movement_types` (engine). The opt-in test `default_movement_types_of_the_real_masters_survive_overrides` reads the retail masters.

## Actor speeds and the `ACBS` speed multiplier

Issues #1065 and #1066. Code: `crates/engine/src/movement_speeds.rs`, `MovementTuning::target_speed`, and `crates/engine/src/actor_drive.rs`.

- `NPC_.ACBS` bytes 14..16 are a `u16` percentage (UESP NPC_ ACBS table; xEdit NPC_ ACBS "Speed Multiplier"; CommonLibSSE-NG `TESActorBaseData::speedMult`). The factor is `percent / 100`. The player's own value scales every gait that `target_speed` returns, including the sprint latch and legacy packages without `movement_types` when `records` holds Player `NPC_ 00000007`. Each resolved actor base's value scales its walk and run speed.
- When `ACBS` template flag bit 1 (`Use Stats`, `0x0002`; [Q10 NPC templates](research/retail/q10-npc-templates.md)) is set on an NPC base, `NpcSpeedTable` follows `TPLT` chains across `NPC_` records to the root template's `ACBS` speed multiplier. Broken, zero, cyclic, or non-`NPC_` (`LVLN`) targets leave that base unresolved rather than guessing. On Player `NPC_ 00000007`, `Use Stats` is rejected with an error just like `Use Traits`.
- An actor's walk speed is the forward walk column of its race's walk slot (`WKMV`, or `DMWL` when unset). Its run speed is the forward run column of the run slot (`RNMV`, or `DMRN` when unset). This is the same split that the player uses.
- `spawn_cell` attaches `ActorMovementSpeeds` to every resolved actor (composed humanoid bodies and custom-body/creature actors), while `ActorDrive` stays on composed-body actors.
- The run clip starts at the actor's own run speed (`RNMV`). Before this change it started at a fixed 300 for every actor.
- Not established, so not changed: which `MOVT` column applies in each actor state, the sprint equation (#1067, #264), the order of the actor modifiers, and sneak, swim and fly speeds. The engine has no sneak, swim or fly movement.
- Data the engine needs: `NPC_` records (`records.data`), `race_movement_links`, `movement_types`, and `default_objects`. A package whose `movement_types` table lacks a race's movement type leaves that race's actors on the labelled stand-in gait thresholds, and the engine logs one warning at startup. `movement-profile-annotate` or a metadata rebuild (`--reuse-assets`) publishes the full table. No new converter field is needed.
- Guard: `engine_movement_speeds_have_no_unlabelled_literal` fails on a bare walk, run, sprint, sneak, swim or fly speed literal in non-test engine source unless a stand-in label with an open issue number covers it.

## Sprint follow-up for the in-house reader

See [sprint evidence and implementation boundary](player-sprint-research.md). The in-house actors schema already remaps sprint links; legacy conversion needs the matching fixed layout. Neither reader establishes the player sprint equation. Future evaluation should use the typed fields and provenance supplied by the in-house reader.
