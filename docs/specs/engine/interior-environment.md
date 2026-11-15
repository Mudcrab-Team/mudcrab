# Authored interior environment (#264 / #364)

`EnvironmentCatalog` resolves CELL/XCLL/LTMP/LGTM inputs into one `SceneEnvironment`.
`ActiveSpace.interior` selects the cell. The selection runs in PostUpdate, after scene switches
and before transform propagation and render extraction. Show Sky and Use Sky Lighting are reported unchanged. No sky dome or clear colour consumer exists in this PR: the registry has no retail entry for Show Sky behaviour (xEdit and CommonLibSSE-NG flag names only). One authored lighting
value is applied: the directional light direction, and only for a cell with a local XCLL of 72
bytes or more. All other lighting values are reported only (see "What is applied and reported").
Door crossings also run in PostUpdate: `DoorCrossingSet` precedes `EnvironmentSet`, including its
deferred scene-switch commands, so the selection changes in the same frame as `ActiveSpace`.

## Selection and inheritance

- CELL DATA bit `0x80` means Show Sky; `0x100` means Use Sky Lighting. These are independent.
  Show Sky and Use Sky Lighting are reported unchanged. No sky dome or clear colour consumer exists in this PR: the registry has no retail entry for Show Sky behaviour (xEdit and CommonLibSSE-NG flag names only). Selecting weather from XCCM, region, climate and time remains outside this slice.
- An XCLL inheritance bit selects the linked LGTM field group, including valid black and zero.
  A clear bit selects the cell field. A missing selected template or field stays missing; it does
  not silently borrow the unselected cell/template value. The resolver retains the source per
  field, the original mask/link, and a list of missing fields.
- Groups (bits `0x1`, `0x2`, `0x4`, `0x20`, `0x40` and `0x400` come from the retail trace,
  RF-LIGHT-CPU-001; bits `0x80`, `0x100` and `0x200` come from the retail trace too,
  RF-LIGHT-CPU-009, high; bits `0x8` and `0x10` are covered by neither registry row, keep the
  xEdit definitions, and are reported but unused): `0x1` ambient and ambient cube; `0x2` directional color; `0x4` both fog colors;
  `0x8` fog near; `0x10` fog far; `0x20` both rotations; `0x40` directional fade;
  `0x80` fog clip; `0x100` fog power; `0x200` fog max; `0x400` both light fade distances.
- Inherited ambient cubes use the separate LGTM DALC subrecord. The embedded LGTM DATA ambient
  block is marked **Unused** by xEdit and is not a fallback. A present black cube is not missing.
  Specular color and the final DALC float (xEdit's Fresnel Power, historically named `scale` in
  this schema) are retained as authored.
- Compatibility policies, not verified native behavior: an LTMP-only row uses template groups
  (reported only: see the sun direction rule below); a short XCLL without an inheritance mask
  uses its local fields. A database-open failure or
  missing environment tables, cells or flags selects a closed interior with missing authored
  light, not the previous scene. The startup warning distinguishes an open failure. Missing
  fields stay missing in the diagnostics; no substitute value is selected.
- Rejected XCLL blocks below 72 bytes retain their authored length (including zero), select no
  implicit template groups, and report `MalformedCellXcll` sources with missing lighting fields.
  An absent XCLL alone selects the LTMP-only policy. Older packs can have already collapsed these
  cases; recovering that distinction requires regeneration from source, not a runtime guess.
- Fog and light fade distances are resolved and inspectable, but no renderer consumer applies
  them (see "What is applied and reported"). Interior weather fog remains disabled.

## What is applied and reported

Sources: `q5-interior.md` (REA 2026-11-14, SkyrimSE.exe 1.7.104.0, static CPU-side trace) and
the registry rows RF-LIGHT-CPU-001 to RF-LIGHT-CPU-006, RF-LIGHT-GPU-001, RF-LIGHT-GPU-003,
RF-LIGHT-GPU-006 and RF-IMGS-001. No number or mapping rule is used
without a Skyrim data or retail source. Each value has its source ID in a code comment.

**Applied (A): the directional light direction only.** RF-LIGHT-CPU-004 (medium): the retail
rotation is `R_z(rotation XY) x R_y(rotation Z)`, both in degrees x pi/180, and the light shines
along its local X axis (RF-LIGHT-GPU-006, high: the shader constant is
`DirLightDirection = -(NiLight +0x140)`; `DirLightColor` is NiLight diffuse x fade x IMGS sunlight
scale and is not applied). In Creation axes the direction is `d = (cos a cos b, sin a cos b, -sin b)`
with `a` = rotation XY and `b` = rotation Z. The engine's existing Creation-to-engine mapping
`(x, y, z) -> (x, z, -y)` (`creation_to_bevy`) converts `d`. The result is written to the
rotation of the marked sun (`WorldSun`) only. It applies only when the selected interior has a
local XCLL of 72 bytes or more (`xcll_size` is `Some(size)` with `size >= 72`) and both rotation
fields are present. An LTMP-only cell (no XCLL) and a rejected block under 72 bytes leave the sun
unchanged and restore the exterior rotation: the template-only inheritance mask has no retail
source, and #695 tracks the LTMP-only behaviour. Their template or local values are still
reported. A missing field changes nothing. The sun's exterior rotation is saved on the
first interior write and restored when the selected interior no longer supplies the direction.
`ExteriorSunRotation` is the hook for the clock sun system (#588): that system should write its
rotation there while the component exists and skip the Transform write, so the restore is never
stale.
Nothing rewrites the sun rotation each frame in the exterior. The sign and axis convention is
unverified (medium).

Colour, illuminance, shadow cascades and `GlobalAmbientLight` are never written by the selection.

**Reported only (B), no render effect:**

- `ambient_cube` (RF-LIGHT-CPU-002, retail `0x1414ec3b0`, high).
  Per channel, with the six DALC colours in record order c0 to c5 (bytes x 1/255): mean
  `(c0+c1+c2+c3+c4+c5)/6`; half-differences `(c1-c0)/2`, `(c3-c2)/2`, `(c5-c4)/2` for X, Y, Z;
  specular colour passes through; `scale = max(DALC scale, 1.0)`. The pure function
  `AmbientCube::ambient_at(normal)` is `mean + n.x*hx + n.y*hy + n.z*hz`. The sign is settled
  by RF-LIGHT-GPU-003 (high arithmetic): record order, no negation, no clamp. A +X normal leans
  to record colour 1, +Y to colour 3 and +Z to colour 5 (xEdit "X+" is light travelling toward
  +X). The normal is in world space (medium-high). RF-LIGHT-GPU-001 (high): this DALC cube is the
  only ambient input of the retail Lighting shader. The registry row RF-LIGHT-CPU-002 still says
  "sign unverified"; RF-LIGHT-GPU-003 settles it.
- `ambient_color` and `directional_color`: the authored RGBA8 bytes x 1/255 (q5-interior.md,
  "Inheritance", high).
- `light_fade`: the selected begin and end (inheritance bit `0x400`, RF-LIGHT-CPU-001) and their
  squares (RF-LIGHT-CPU-003). The pure function `point_light_fade_factor(q, fade)` follows
  RF-LIGHT-CPU-003 with `q = |camera - light|^2 - r^2`: 1 below `begin^2`,
  `1 - (q - begin^2) / (end^2 - begin^2)` up to `end^2`, 0 beyond. Retail applies it only to a
  light whose BSLight `+0x63` flag is set. That flag is set unless the LIGH reference has REFR
  flag `0x00010000` ("Never Fades"), and defaults to 1 (RF-LIGHT-CPU-007, high). The function is
  reported only and is not applied to any light.
- Directional fade: the raw selected value only. RF-LIGHT-CPU-008 (supersedes RF-LIGHT-CPU-005):
  the getter has no runtime reader and retail only stores the value (medium-high, absence of
  evidence).
- `image_space` on `SceneEnvironment`: the base image space source per RF-IMGS-001 (high for the
  getter; s3-image-space.md "Cell image space for interiors (XCIM)", cell getter `0x1402bf340`).
  An interior with
  `CELL DATA flags & 0x101 == 0x001` reports source `xcim` with the form id of
  `cell_environment.image_space_id`, or `default_image_space` with form `0x167` when XCIM is
  absent. An exterior, or an interior that uses sky lighting (`flags & 0x101 != 0x001`), reports
  `weather_global` with no form id. A database without the `image_space_id` column, or without
  the cell flags, reports it as missing (null). Reported only: no image space is applied. The
  reference image space override (extra `0x77`, RF-IMGS-001, medium) is not implemented. The
  override pointer and IMAD blending are not traced.
- Everything else the resolver already reported: sources, missing fields, fog values, flags.

A missing source field serializes as null. No substitute value is reported or applied.

**Waits, with the reason:**

- Ambient application: the cube needs a normal-dependent shader term. RF-LIGHT-GPU-001 finds
  that the retail Lighting shader reads the DALC cube only (the XCLL ambient colour reaches
  grass, effect and distant-tree shaders). The engine has one uniform ambient colour. Neither
  ambient is proven as the cause of dark interiors.
- Directional colour and intensity: the unit conversion from colour bytes to render units and the
  combine are unresolved (S3).
- Point-light fade application: the per-light flag is named now (BSLight `+0x63`,
  RF-LIGHT-CPU-007), but the local-light path belongs to #488. It receives the resolved begin
  and end values and the flag rule.
- Fog colours: no fog consumer is in this slice.
- Cell image space application (XCIM): the selection is reported. Applying it needs the image
  space chain, see #696.
- The renderer-wide output scale `cb2[16].w`: not traced.
- Directional fade: no runtime reader (RF-LIGHT-CPU-008).
- Fog colours, clip distance, fog power and fog max (RF-LIGHT-CPU-009) are reported only.

Waits for #693 to #696 and #706 (#706 is the retail interior lighting path under the skyrim
profile):

- #693: directional fade and rotation semantics.
- #694: six-face DALC ambient and the colour domain of authored colour bytes.
- #695: missing template and short XCLL behaviour.
- #696: interior image space (CELL XCIM) through #361.

This PR does not change the sky dome or the clear colour. Show Sky and Use Sky Lighting are reported unchanged. No sky dome or clear colour consumer exists in this PR: the registry has no retail entry for Show Sky behaviour (xEdit and CommonLibSSE-NG flag names only).

`SceneState` reports the selected values, sources and missing fields under `environment`;
selection changes also emit one diagnostic log record. Catalog and space changes refresh the
selection.

## Evidence

Format definitions inspected on 2026-10-31:

- [xEdit TES5 definitions](https://github.com/TES5Edit/TES5Edit/blob/dev-4.1.6/Core/wbDefinitionsTES5.pas),
  Git blob `937db63510050e41b38f43434cfea306898b6dd5`: CELL DATA flags, XCLL inheritance groups,
  LGTM DATA unused embedded ambient, and separate DALC.
- [xEdit common definitions](https://github.com/TES5Edit/TES5Edit/blob/dev-4.1.6/Core/wbDefinitionsCommon.pas),
  Git blob `b93975dab798f054b75497fedca0869fe80a2ffa`: six directional colors, specular and Fresnel Power.
- [CommonLibSSE-NG InteriorData.h](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/I/InteriorData.h)
  and [TESObjectCELL.cpp](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/src/RE/T/TESObjectCELL.cpp):
  matching inheritance names and independent Use Sky Lighting guard in cell fog setters. These
  declarations/wrappers establish neither retail light equations nor a native source-selection trace.
- Existing converter/schema: [environment.rs](../../../crates/converter/src/esm/environment.rs)
  and [db-schema.md](../converters/db-schema.md). The converter now keeps the authored `xcll_size` of a rejected XCLL block (under 72 bytes) instead of NULL. The schema version and the table shape do not change. A pack converted before this change keeps the old value and must be regenerated from source to recover the distinction.

Read-only inspection of registered `user-testing-main-b5259d33` found both cells 78791/78792 with
DATA 129 (`0x81`), LTMP 659862 (`FarmLightingTemplate`), inheritance 159 (`0x9f`). Thus ambient,
directional color and the selected fog groups inherit; rotation and fade stay local. The template
directional RGB is 65/70/80, local fade is 0, and local Z rotations are respectively 0/30 degrees.
The separate template DALC is nonzero; its unused DATA cube is zero. This is package evidence,
not a retail live-source trace. No registered pack, settings file or playtest checkout is modified.

## Validation scope

Procedural plugin overrides pass through converter publication, SQLite loading and resolution
into `SceneEnvironment`. Additional CPU tests cover local/inherited/zero/missing values, absent
masks, old databases, separate template DALC and the reported Show Sky and Use Sky Lighting flag
combinations (no dome or clear colour is tested: no consumer exists). Image space selection tests cover an interior with XCIM, an interior that uses sky lighting, an
interior without XCIM, and a database without the `image_space_id` column. Three registered-pack
cells (428953, 86654 and 78792, copied from
`cell_lighting` and template 659862 of `user-testing-main-b5259d33-clusters` with read-only
sqlite3) check the reported ambient cube, colour bytes, fade squares and the applied sun
direction against hand-computed values. The ambient test checks `ambient_at(+X)`, `+Y` and `+Z`
against the record colour pairs (1, 0), (3, 2) and (5, 4). Unit tests cover the DALC scale rule, the fade factor,
a missing rotation field, an LTMP-only cell and a 40-byte rejected block (both leave the sun
unchanged), and the exterior restore. One test checks that selecting an interior
leaves the directional light colour and illuminance, its shadow cascades and
`GlobalAmbientLight` unchanged. The opt-in
`managed_farm_cells_reach_the_scene_environment_without_modifying_pack` test opens only the
explicitly provided `MUDCRAB_INTERIOR_TEST_DB` with read-only SQLite flags. It asserts package
selection, not displayed output. Test logs and source identities belong in task evidence.

Remaining research: #693 to #696 and #706 above, the rotation order and sign check for the sun
direction (RF-LIGHT-CPU-004, medium), retail template and ambient selection, raw-domain light
transport, dynamic weather/time and XCCM selection, fog consumers, native render behavior,
performance and matched Skyrim captures.
