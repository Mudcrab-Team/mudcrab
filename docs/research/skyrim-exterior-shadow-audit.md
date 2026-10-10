# Exterior shadow audit

The visual reference is vanilla Skyrim Special Edition / Anniversary Edition.
The current renderer had two source mismatches that could produce the reported
large mountain shadows: it treated every converted NIF mesh as a shadow caster,
and it expanded directional shadow coverage to the entire streamed grid.

## Source caster participation

The converter already retains `shaderFlags1` in each glTF material's
`extras.openSkyrim`. Bit 9 is `Cast_Shadows` in the NIF schema, also named
`kCastShadows` in the pinned CommonLib `BSShaderProperty` definition.
The engine did not previously consume that bit.

A live audit of the converted vanilla mountain assets found 20 models with
53 materials: 47 materials have the caster bit clear and 6 have it set.
`MountainCliff01`, `MountainCliff02`, `MountainCliff03`, `MountainCliff04`,
`MountainPeak01`, `MountainPeak02`, and the ordinary ridge models have it clear.
Crevasse, trailer, and some slope materials have it set. There are 63 mountain
references in the resident grid around the Riverwood test start.

The glTF primitive hook now adds Bevy's `NotShadowCaster` when an authored
Lighting or Effect material explicitly clears that bit. This applies to each
primitive, survives scene cloning, and leaves caster-enabled materials eligible
for the shadow pass. Missing flags, invalid flags, and generic glTF materials
retain their existing policy. Existing exclusions are also preserved.
The change does not alter shadow reception or remove mountain geometry.

Private evidence: `implementation/mountain-shadow-source-audit.json` in the
fog research directory contains the source hashes, material flags, and nearby
reference counts. Repository deliverables contain interpreted findings only.

## Shadow distance and quality

The copied SE quality presets contain these `[Display]` values:

| Preset | `fShadowDistance` | `iShadowMapResolution` |
|---|---:|---:|
| Low | 3000 | 1024 |
| Medium | 3000 | 2048 |
| High | 8000 | 2048 |
| Ultra | 10000 | 4096 |

The shipped `Skyrim/SkyrimPrefs.ini` template also has 8000 and 2048. It is a
template, not evidence of the user's active retail profile.

The engine now defaults to those High values and reads both settings through
`--ini`. The directional cascade range stays independent of streaming radius
and camera altitude. Its previous default range was about 23,200 Creation
units because it fitted the far corner of the resident grid.
Near object shadows remain enabled.

The four Bevy cascades, their split distribution, overlap, filtering, and bias
remain adapters. This change does not claim to reproduce Skyrim's native
shadow map construction or every caster-culling rule.

## Remaining evidence and validation

The copied template has `bDrawLandShadows=0`, but its exact SE consumer is still
untraced. Its name alone does not establish a terrain caster or receiver rule,
so this change does not invent one. Full-detail LAND and generated terrain LOD
retain their existing Bevy shadow policy. They are distinct from the NIF
mountain assets whose source participation is now enforced.

Tests cover layered INI quality settings, invalid settings, range independence
from camera altitude and streaming, startup wiring, and scene-cloned caster
participation using the source mountain flags. Build and GPU validation are
recorded with the enclosing image-space implementation.

The fast fog test package omits distant terrain LOD. The screenshot's bright,
flat distant mountain faces need separate material and coverage validation;
caster participation alone does not explain their displayed color. Matched
retail captures remain necessary to establish final visual parity.
