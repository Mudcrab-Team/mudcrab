# Player movement record map (Skyrim Special Edition masters)

This is the first Riverwood WALK profile, not a recovered Skyrim movement equation. Values below came from the retail ESM files at a local unmodded `Skyrim Special Edition/Data` directory and a locally converted Riverwood package (`<package>/assets/skyrim_world.db`). The [source manifest](player-movement-plugin-manifest.csv) lists all 80 package plugins in load order with SHA-256 checksums verified against the installed files. The table below highlights the relevant priorities.

| Priority | Plugin | SHA-256 |
| --- | --- | --- |
| 0 | Skyrim.esm | `e198c3b85e5e48e0c92a6580d8f66e644256b68d812ee32b61735cf9b753df73` |
| 1 | Update.esm | `b298b0f65fa0127fd13c1f5ef6bb30eaf9d21fd19f5e6f8f91f829c9007d3280` |
| 2 | Dawnguard.esm | `fc8f92cb7ed55217046ee96b84caa54c058c12f7c408ac2a71da880a7bd3eab7` |
| 3 | HearthFires.esm | `0f681496771e6d1983c6c1c9dd6602964068837f015bfa21e69a149c3b050047` |
| 4 | Dragonborn.esm | `635f21a938a17b8677f524cf41210933afc7f23820f2e210ab3bb6d3cd837046` |
| 22 | ccbgssse018-shadowrend.esl | `7ffcdfd2882445f25db3c5b4d40de7efa28dcca03d8eceb7a3e155e3c6618cfc` |

The winning Player `NPC_` `0x00000007` comes from `ccbgssse018-shadowrend.esl` and has `RNAM = 0x00013746` (`NordRace`). Update.esm supplies the winning NordRace record. Its `RACE` data has no `WKMV` or `RNMV` subrecord and no embedded `0x0003580D` FormID. CommonLibSSE-NG exposes six runtime `baseMoveTypes` slots. An unset slot falls back to the race's default movement type in the game's default-object table (see [Native default movement selection](#native-default-movement-selection)). Skyrim.esm's `DOBJ` maps the walk and run defaults to `NPC_Default_MT` `MOVT` `0x0003580D`, and no other plugin listed here changes them. NordRace therefore walks and runs with `NPC_Default_MT` through the default-object table, not through a record link. The package loader resolves explicit `WKMV` and `RNMV` links independently. For unset links it uses the converted `default_objects` `DMWL`/`DMRN` rows, or `0x0003580D` for packages converted before that table existed.

`MOVT` `SPED` is eleven little-endian `f32` values. CommonLib's `Movement::MaxSpeeds` identifies four direction pairs, one rotation pair, and `rotateWhileMovingRun`. The retail `NPC_Default_MT` values are:

| Direction | Walk | Run |
| --- | ---: | ---: |
| Left | 80.09 | 370.00 |
| Right | 79.75 | 370.00 |
| Forward | 80.10 | 370.00 |
| Back | 71.93 | 205.25 |

The last three floats are `3.14159`, `3.14159`, `3.14159`; they are rotation values, not linear speed. The engine treats the first eight raw numbers as Creation units per second with conversion factor `1` for this first guess. The header proves the field order but does not prove the game's elapsed-time equation or physical units. For our fixed-step controller, steady flat-ground distance is `effective_speed * elapsed_seconds`; acceleration and contacts change distance from rest. Thus forward walk/run targets over two steady seconds are `160.20`/`740.00` Creation units under this project rule.

Skyrim.esm contains `GMST` `fMoveCharWalkBase` `0x0001EC72 = 100.0` (`f32`) and `fJumpHeightMin` `0x000ABEF6 = 76.0` (`f32`). Neither value is substituted for a `MOVT` speed or jump impulse: their names and stored type alone do not establish controller use or units. No clearly named gravity GMST was found in the five masters. The current controller's `900` Creation units/s² gravity and `340` Creation units/s jump launch remain provisional; with its ballistic integration, their continuous-model apex is `340² / (2 * 900) = 64.22` Creation units. `fJumpFallHeightMin = 600` and `fJumpFallVelocityMin = 700` are fall-related settings and are not treated as gravity.

Sources: the five ESM hashes above; [`Movement.h`](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/M/Movement.h); [`TESRace.h`](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESRace.h); [Mutagen MOVT schema](https://github.com/Mutagen-Modding/Mutagen/blob/dev/Mutagen.Bethesda.Skyrim/Records/Major%20Records/MovementType.xml). ESM `MOVT`/`GMST` subrecord bytes and FormIDs were read directly from those files.

## Native default movement selection

Question: which `MOVT` does a race use when its `WKMV` or `RNMV` is unset?

Build: SkyrimSE.exe 1.7.104.0, SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.

Static evidence (Ghidra 12.0 and radare2; RVAs are function starts confirmed against `.pdata`):

- `0x3e3da0` (TESRace vtable slot 6, Load) reads `WKMV`, `RNMV`, `SWMV`, `FLMV`, `SNMV` and `SPMV` as raw FormIDs into six consecutive slots at race `+0x478`. An absent subrecord leaves its slot zero.
- `0x3e5930` (slot `0x13`, InitItemImpl) resolves each nonzero slot and casts it to `BGSMovementType`. Zero slots stay null; nothing substitutes a default here.
- `0x3e64a0` returns `slot[i]` when it is non-null and otherwise tail-calls `0x393860(i)`.
- `0x393860` maps `i` = 0..5 to default-object indexes `0xAD`..`0xB2`, then calls `0x154440`. `0x154440` returns `objects[index]` from the default-object manager when that entry is initialised, otherwise null.
- The 24-byte default-object descriptors at `0x14207bf40` (372 entries) name indexes `0xAD`..`0xB2` "Default MovementType: Walk/Run/Swim/Fly/Sneak/Sprint". Their form type is `0x7F` (MOVT), and their keys are `DMWL`, `DMRN`, `DMSW`, `DMFL`, `DMSN` and `DMSP`.
- `0x31b8b0` (BGSDefaultObjectManager Load) matches each `DOBJ` `DNAM` (key, FormID) pair against the descriptor keys and writes that entry. It never clears the table, so a later `DOBJ` override changes only the keys it lists.

Cross-check:

- Data: Skyrim.esm `DOBJ` `00000031` maps `DMWL` and `DMRN` to `0x0003580D` (`NPC_Default_MT`). The other defaults are `DMSW` to `0x00036D42` `NPC_Swimming_MT`, `DMFL` to `0x0003580D`, `DMSN` to `0x0003580B` `NPC_Sneaking_MT`, and `DMSP` to `0x000F3469` `AIControlledNPC_Sprinting_MT`.
- Update.esm, Dawnguard.esm, HearthFires.esm, Dragonborn.esm and ccQDRSSE001-SurvivalMode.esl override `DOBJ` without any `DM**` key. The other installed plugins have no `DOBJ`.
- The default-object array (`0x14219d1a0`) and its initialised flags (`0x14219dd40`) are `0xBA0` = 372 × 8 bytes apart, matching the descriptor count.
- `default_movement_types_of_the_real_masters_survive_overrides` (converter, opt-in) merges the five masters and exports `NPC_Default_MT` for both keys. With last-override-wins merging it loses `DMWL`.

Runtime oracle: none. No in-game measurement was taken.

Result: confirmed for the code path above. An unset race slot uses the default object for that slot. For retail data, walk and run both resolve to `NPC_Default_MT`.

Confidence and scope: this covers how the race-level slot is selected. Not established: which callers use `0x3e64a0` (5 direct call sites) and whether actor-level state (combat, weapon or sneak `MOVT` overrides) replaces the result. Also not established is how the game blends walk and run speeds when the two slots name different forms. The engine's split into walk and run tables is its own rule. Only the ten plugins of the analysing install were scanned for `DOBJ`; the other 70 plugins in the [manifest](player-movement-plugin-manifest.csv) were not checked here.

Native mapping: `converter` exports every `MOVT` and a key-merged `default_objects` table (`merge_default_objects`, `esm/mod.rs`). `engine::physics::default_movement_type` reads `DMWL`/`DMRN` for unset links. Tests: `default_object_overrides_merge_by_key`, `remaps_default_object_form_ids_but_not_keys`, and `packaged_movement_profile_resolves_and_legacy_package_falls_back`.

Next discriminating experiment: in game, give the Player's race an explicit `RNMV` and leave `WKMV` unset. Then compare measured walk and run speeds to see how mixed slots blend.
