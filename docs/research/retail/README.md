# Retail behaviour notes (static analysis of SkyrimSE.exe)

> Drafted with an AI assistant (Claude); research notes, not yet checked line by line by a human. A human owner must read the evidence behind any claim before building on it. See [AI_POLICY.md](../../AI_POLICY.md).

These notes record how the retail Skyrim Special Edition executable computes values that open PRs need. Each fact has a stable registry ID (`RF-<AREA>-NNN`) in [registry.md](registry.md). Implementations should cite the ID, not copy a number from another engine.

## Build

| Item | Value |
| --- | --- |
| Executable | `SkyrimSE.exe`, file and product version 1.7.104.0 |
| SHA-256 | `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f` |
| Image base | `0x140000000`; every address in these notes is a virtual address in this image |
| Game data | Skyrim.esm, Update.esm and the DLC masters of the same install |

Addresses are valid for this build only. Another build moves them.

## Method

- Static analysis only. The executable was never run or modified. Tools: Ghidra 12.2 (headless, full auto-analysis, native decompiler) and capstone disassembly, cross-checked against each other.
- Settings: the executable's default for every INI setting and game setting was read from its setting records. Skyrim.esm overrides some game settings; the notes always state which value applies (for example fZKeyMaxForce: executable 250, Skyrim.esm 100).
- Confirmation: a second path where possible (decompiler and disassembly, or two callers), a data cross-check (records, INI, record names), and numeric reproduction of derived claims.
- "Not found" claims are limited to the functions that were read in full, and say so.
- **No decompiler output is published.** The notes give addresses, formulas and values in our own words.

Confidence: **high** = direct instruction trace; **medium-high** = traced with one inferred link; **medium** = partly inferred or from a reconstruction; **low** = lead only.

## Other sources (leads only)

| Source | Licence | Use here |
| --- | --- | --- |
| [spud](https://github.com/chasmlol/spud) (commit `e5f2de4b`) | MIT | Each spud value is classed as derived (it names a game setting, record field or traced formula) or invented. Derived values were confirmed in the binary before use. |
| [OpenSky](https://github.com/jjgroenendijk/opensky) (commit `d7339ee4`) | Apache-2.0 | Same classification. Its renderer is Metal-native, so its shader choices can differ from retail. |
| [Community Shaders](https://github.com/doodlum/skyrim-community-shaders) `Water.hlsl`, `RunGrass.hlsl` | GPL-3.0 | Reconstructions of the retail shaders, used to name shader constants. Shader-side claims based on them are marked medium. |
| CommonLibSSE-NG, xEdit, UESP | various | Field and flag names only. |

No code from these projects was copied. Facts and formulas are cited where used.

## Contents

| File | Topic | PRs | Summary |
| --- | --- | --- | --- |
| [q1-water.md](q1-water.md) | WATR noise scroll | #299 | u = frac(sin(dir)·speed·t), v = frac(cos(dir)·speed·t); bearing 0 = north, the pattern travels toward dir + 180°; the RiverWaterFlow names agree |
| [q2-grass.md](q2-grass.md) | Grass wind and lighting | #539, #541, #611 | amplitude 5 + 120·wind, capped at 60; sun colours passed unscaled; shadow floor 0.3 |
| [q3-carry.md](q3-carry.md) | Grab and carry | #689 | Havok mouse spring 0.5 / 0.2 / 0.75 / 100 × mass scale; drop rules; no Z-key throw |
| [q4-sun.md](q4-sun.md) | Sun path and day/night | #588, #653 | sun GMSTs define the path; light direction normalize(−0.5x, −20, −150); time-of-day colour weights |
| [q5-interior.md](q5-interior.md) | Interior lighting, CPU side | #480 | XCLL/LGTM inheritance; DALC ambient cube form; light fade; directional fade stored only |
| [q6-camera.md](q6-camera.md) | First-person stair smoothing | #511, #619, #520 | per-frame second-order lag spring 0.001 / 0.6; snap at 300 units; no slope rule |
| [q7a-camera-states.md](q7a-camera-states.md) | Player camera states | camera | FOV, mouse 0.000785 rad/count yaw, 0.00106 pitch, third-person offsets, auto vanity |
| [q7b-camera-scripted.md](q7b-camera-scripted.md) | Scripted camera control | scripts | Papyrus natives; Helgen and Thieves Guild sequences |
| [q7c-controller-interp.md](q7c-controller-interp.md) | Physics step and controller | #619 | player controller runs per rendered frame; rigid bodies in 1/60 s steps |
| [q8-mods.md](q8-mods.md) | Alternate textures (MODS) | #508 | matched by 3D Index (depth-first geometry count); whole texture set replaced |
| [registry.md](registry.md) | All facts by ID | | |

This publication covers the areas WATER, GRASS, CARRY, SUN, LIGHT-CPU, CAM, PHYS and TEX. Other areas are researched separately and are not included here.
