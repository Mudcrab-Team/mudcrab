# Authored interior environment (#264 / #364)

`EnvironmentCatalog` resolves CELL/XCLL/LTMP/LGTM inputs into one `SceneEnvironment`.
`ActiveSpace.interior` selects the cell. The selection runs in PostUpdate, after scene switches
and before sky updates, transform propagation and render extraction. The sky consumes only the
CELL DATA flags. No Bevy light consumes the authored lighting (see "No interior light is
applied"). Door crossings also run in PostUpdate: `DoorCrossingSet` precedes `EnvironmentSet`,
including its deferred scene-switch commands, so the selection changes in the same frame as
`ActiveSpace`.

## Selection and inheritance

- CELL DATA bit `0x80` means Show Sky; `0x100` means Use Sky Lighting. These are independent.
  Both flags are reported unchanged in `SceneEnvironment`. Only Show Sky has a consumer (the sky
  dome and clear colour). Selecting weather from XCCM, region, climate and time remains outside
  this slice.
- An XCLL inheritance bit selects the linked LGTM field group, including valid black and zero.
  A clear bit selects the cell field. A missing selected template or field stays missing; it does
  not silently borrow the unselected cell/template value. The resolver retains the source per
  field, the original mask/link, and a list of missing fields.
- Groups: `0x1` ambient and ambient cube; `0x2` directional color; `0x4` both fog colors;
  `0x8` fog near; `0x10` fog far; `0x20` both rotations; `0x40` directional fade;
  `0x80` fog clip; `0x100` fog power; `0x200` fog max; `0x400` both light fade distances.
- Inherited ambient cubes use the separate LGTM DALC subrecord. The embedded LGTM DATA ambient
  block is marked **Unused** by xEdit and is not a fallback. A present black cube is not missing.
  Specular color and the final DALC float (xEdit's Fresnel Power, historically named `scale` in
  this schema) are retained as authored.
- Compatibility policies, not verified native behavior: an LTMP-only row uses template groups;
  a short XCLL without an inheritance mask uses its local fields. A database-open failure or
  missing environment tables, cells or flags selects a closed interior with missing authored
  light, not the previous scene. The startup warning distinguishes an open failure. Missing
  fields stay missing in the diagnostics; no substitute value is selected.
- Rejected XCLL blocks below 72 bytes retain their authored length (including zero), select no
  implicit template groups, and report `MalformedCellXcll` sources with missing lighting fields.
  An absent XCLL alone selects the LTMP-only policy. Older packs can have already collapsed these
  cases; recovering that distinction requires regeneration from source, not a runtime guess.
- Fog and light fade distances are resolved and inspectable, but their renderer consumers remain
  outside this slice. Interior weather fog remains disabled; showing sky alone does not add it.

## No interior light is applied

The engine resolves and reports the authored interior lighting, but it applies none of it to the
renderer. Selecting an interior cell writes no Bevy light: the directional light (colour,
illuminance, rotation), its shadow cascades and `GlobalAmbientLight` keep the exterior baseline
and the user preferences. No Skyrim source for the mapping from authored values to the renderer
exists yet. Open research:

- #693: directional fade and rotation semantics.
- #694: six-face DALC ambient and the colour domain of authored colour bytes.
- #695: missing template and short XCLL behaviour.
- #696: interior image space (CELL XCIM) through #361.

Colours are reported as the raw authored bytes and missing fields are reported as missing.
Sky visibility composes Show Sky with the existing user sky toggle. A cell's lighting choice
never forces the dome on. The clear colour follows the selected sky visibility domain using the
existing sky presentation policy. No image-space or default-colour correction is included.

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
masks, old databases, separate template DALC, all sky flag combinations and actual dome
visibility with preferences. One test checks that selecting an interior leaves the directional
light, its shadow cascades and `GlobalAmbientLight` unchanged. The opt-in
`managed_farm_cells_reach_the_scene_environment_without_modifying_pack` test opens only the
explicitly provided `MUDCRAB_INTERIOR_TEST_DB` with read-only SQLite flags. It asserts package
selection, not displayed output. Test logs and source identities belong in task evidence.

Remaining research: #693 to #696 above, retail template and ambient selection, raw-domain light
transport, dynamic weather/time and XCCM selection, fog consumers, native render behavior,
performance and matched Skyrim captures.
