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

The winning Player `NPC_` `0x00000007` comes from `ccbgssse018-shadowrend.esl` and has `RNAM = 0x00013746` (`NordRace`). Update.esm supplies the winning NordRace record. Its `RACE` data has no `WKMV` or `RNMV` subrecord and no embedded `0x0003580D` FormID. CommonLibSSE-NG exposes six runtime `baseMoveTypes` slots, but that header does not prove how an unset slot selects a movement form. The Riverwood first pass explicitly selects `NPC_Default_MT` `MOVT` `0x0003580D`, whose winning record is in Skyrim.esm. This selection is a project rule pending native selection-path evidence, not a record link from NordRace. Packages whose Player race supplies `WKMV` or `RNMV` fail with an explicit error until that selection path is supported.

`MOVT` `SPED` is eleven little-endian `f32` values. CommonLib's `Movement::MaxSpeeds` identifies four direction pairs, one rotation pair, and `rotateWhileMovingRun`. The retail `NPC_Default_MT` values are:

| Direction | Walk | Run |
| --- | ---: | ---: |
| Left | 80.09 | 370.00 |
| Right | 79.75 | 370.00 |
| Forward | 80.10 | 370.00 |
| Back | 71.93 | 205.25 |

The last three floats are `3.14159`, `3.14159`, `3.14159`; they are rotation values, not linear speed. The engine treats the first eight raw numbers as Creation units per second with conversion factor `1` for this first guess. The header proves the field order but does not prove the game's elapsed-time equation or physical units. For our fixed-step controller, steady flat-ground distance is `effective_speed * elapsed_seconds`; acceleration and contacts change distance from rest. Thus forward walk/run targets over two steady seconds are `160.20`/`740.00` Creation units under this project rule.

Skyrim.esm contains `GMST` `fMoveCharWalkBase` `0x0001EC72 = 100.0` (`f32`) and `fJumpHeightMin` `0x000ABEF6 = 76.0` (`f32`). The walk setting is still not substituted for a `MOVT` speed. Native jump observations now establish that the loaded jump setting is multiplied by reference scale and NPC/race height before conversion to Havok length and impulse construction. The measured baseline uses `76 × 1 × 1 × 1.03`, yielding a float32 setter argument of `78.27999877929688` Creation units. That value is a launch parameter, not a guaranteed apex.

Startup reads the effective `fJumpHeightMin` by editor ID, canonical Player `NPC_` 7 `ACBS` sex and `NAM6` height, and the normalized `RNAM` race's canonical `DATA` height pair. It attributes records through `plugins.priority`, which is the exported load order; `plugins.id` is a separate database row ID. Modern packages require complete supported records. Malformed data and active template inheritance are rejected. The runtime player currently uses reference scale one. Save mutations, inherited actor traits and swimming are outside this supported profile. Table-less legacy packages and the asset-free fixture explicitly use a synthetic unit actor with height setting 76.

The character law keeps velocity in Havok units. It derives launch speed from `sqrt(2 × abs(gravity) × abs(effective_height))`, applies gravity on that same launch step, then advances position using the updated velocity. Native world gravity is float32 `9.8100004196167` downward. The Creation/Havok conversion constants are float32 `69.99124908447266` and `0.014287499710917473`. Strictly exceeding the previous maximum resolved height uses gravity multiplier one; equal or lower height uses float32 1.35, including the first launch step. The apex diagnostic measures actual resolved height above takeoff.

The production schedule remains fixed at 60 Hz and passes its actual fixed-clock delta to the transition. Native scheduler ownership and high-cadence behavior remain unresolved. The former 900/340 character approximation is retained only in the before-change evidence. Rigid-body gravity remains a separate 900 Creation units/s² policy because native character measurements do not establish rigid-body acceleration. `fJumpFallHeightMin = 600` and `fJumpFallVelocityMin = 700` remain fall-related settings and are not treated as gravity.

Sources: the five ESM hashes above; [`Movement.h`](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/M/Movement.h); [`TESRace.h`](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESRace.h); [Mutagen MOVT schema](https://github.com/Mutagen-Modding/Mutagen/blob/dev/Mutagen.Bethesda.Skyrim/Records/Major%20Records/MovementType.xml). ESM `MOVT`/`GMST` subrecord bytes and FormIDs were read directly from those files.

## Sprint follow-up for the in-house reader

See [sprint evidence and implementation boundary](player-sprint-research.md). The in-house actors schema already remaps sprint links; legacy conversion needs the matching fixed layout. Neither reader establishes the player sprint equation. Future evaluation should use the typed fields and provenance supplied by the in-house reader.
