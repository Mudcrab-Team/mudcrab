# Player movement record map (Skyrim Special Edition masters)

This is the first Riverwood WALK profile, not a recovered Skyrim movement equation. Values below came from the retail ESM files at `/home/dev/skyrim/Skyrim Special Edition/Data` and the current `/home/dev/riverwood-pkg/assets/skyrim_world.db`. The [source manifest](player-movement-plugin-manifest.csv) lists all 80 package plugins in load order with SHA-256 checksums verified against the installed files. The table below highlights the relevant priorities.

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

Skyrim.esm contains `GMST` `fMoveCharWalkBase` `0x0001EC72 = 100.0` (`f32`) and `fJumpHeightMin` `0x000ABEF6 = 76.0` (`f32`). Neither value is substituted for a `MOVT` speed or jump impulse: their names and stored type alone do not establish controller use or units. No clearly named gravity GMST was found in the five masters. The current controller's `900` Creation units/s² gravity and `340` Creation units/s jump launch remain provisional; with its ballistic integration, their continuous-model apex is `340² / (2 * 900) = 64.22` Creation units. `fJumpFallHeightMin = 600` and `fJumpFallVelocityMin = 700` are fall-related settings and are not treated as gravity.

Sources: the five ESM hashes above; [`Movement.h`](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/M/Movement.h); [`TESRace.h`](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESRace.h); [Mutagen MOVT schema](https://github.com/Mutagen-Modding/Mutagen/blob/dev/Mutagen.Bethesda.Skyrim/Records/Major%20Records/MovementType.xml). ESM `MOVT`/`GMST` subrecord bytes and FormIDs were read directly from those files.
