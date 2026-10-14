# Skyrim lighting notes compared with Mudcrab

The supplied notes identify missing runtime behavior. Several authored fields
survive conversion but still have approximate or absent consumers. The current
renderer remains a mixed native/Bevy preview, and the rejected screenshots have
not passed visual acceptance. This comparison changes no lighting coefficients.

The input is `/Users/taylor/Downloads/skyrim_lighting_technical_notes.md`, SHA-256
`b169c56c7222f7d22c801e08c802090f1295f2109917aa9146130a8007bb6ba2`.
The four principal sources were independently fetched through their primary
MediaWiki APIs. Their saved revisions are CELL format 3570761, Lighting Template
24291, Cell Lighting 24539 and Lights and FX 7605. Page fetches returned 403;
the APIs returned 200. Private input, source and code audits are under
`lighting-remediation-20261010/notes-review-01/` in the existing research root.

## Data and rendering comparison

| Topic | Current implementation | Consequence |
| --- | --- | --- |
| Modern XCLL and field inheritance | The parser reads the 92-byte layout and the catalog resolves all eleven serialized inheritance groups. CELL and template cubes are retained separately. | Data preservation is implemented; individual consumers still need qualification. |
| Template base Ambient | Exterior lighting uses WTHR DALC without multiplying by scalar NAM0 Ambient. Interior scalar-Ambient fallback is explicit preview policy. | The pack has DALC in all 133 templates and zero successful interior scalar-Ambient fallback cases. Adding base Ambient would not repair an identified active error. |
| Six-axis ambient blend | Recovered native preparation builds affine rows; the ordinary shader evaluates them outside direct shadow visibility. | This inspected arithmetic has stronger evidence than the notes' open question. Effective retail rows and the extra native RGB operand remain unknown. |
| Interior cube inheritance | The preview chooses template DALC using the Ambient inheritance bit. | This coupling is approximate. In 411 inherited-ambient interiors, CELL and template cubes differ. Fog getter inheritance does not prove cube ownership. |
| Template NONE | The lighting catalog rejects a nonzero inheritance mask without valid template inputs. | Forty-two of 786 interior records have null LTMP and nonzero masks. Selecting one causes fixed preview lighting. Existing native null-template proof covers seven fog fields; ambient/directional handling remains open. |
| Directional Fade and rotation | Fade is retained but unapplied. Interior angles use a stated approximation. | Of 744 resolvable interiors, 524 have nonunit Fade and nonblack directional RGB. The two CK pages disagree on Fade's meaning; multiplying by it would be an assumption. |
| Show Sky / Use Sky Lighting | Flags affect environment mode and sky display; authored interior coefficients still ignore their lighting precedence. | The pack has 282 ShowSky interiors. Their ambient/directional selection remains unsupported. |
| Local light types and shadows | All placed locals become unshadowed PointLights. Native preview takes seven camera-nearest candidates; fallback has a separate 64-light budget. | Omni shadows, spotlight and hemispherical distinctions are absent. Neither budget implements the tutorial's four-caster statement. |
| Local light fading | XCLL/LGTM distances are retained but unused by both local-light consumers. Related INI setting objects and compiled initializers are inventoried; effective values remain unresolved. | Effective zero fallbacks, fade interpolation and shadow-caster exemption need a native consumer trace. |
| FX external emittance | Raw XEMI bytes and FormID remapping survive. Typed linkage and the documented FX light-selection policy are absent. | An FX reference containing only XEMI is excluded from the typed lighting snapshot. This is a typed/runtime gap, not demonstrated source-byte loss. |
| Room overrides | LNAM/INAM links survive. Explicit room fog endpoints can blend; automatic room selection and room ImageSpace selection are absent. | CELL-level template/XCIM selection does not implement room ownership or transitions. |
| Template Specular/Fresnel | Those template controls are separate from active NIF material specular. | Their documented no-op does not justify disabling material highlights or reflection families. |
| Fog Max | The CK pages conflict. Existing native getter/shader evidence supplies an active fog maximum. | Preserve the scoped native implementation; the Cell Lighting page alone cannot justify disabling Max. |

The authoring claims above are supported by the [CELL format page](https://en.uesp.net/wiki/Skyrim_Mod:Mod_File_Format/CELL),
[Lighting Template](https://skyrimck.uesp.net/wiki/Lighting_Template),
[Cell Lighting](https://ck.uesp.net/wiki/Cell_Lighting) and
[Lights and FX tutorial](https://ck.uesp.net/wiki/Bethesda_Tutorial_Lights_and_FX).
The tutorial revision predates SE. None observes a selected retail draw.

The owning code is the converter [lighting parser](../../crates/converter/src/esm/lighting.rs),
[lighting catalog](../../crates/engine/src/lighting_catalog.rs),
[environment preparation](../../crates/engine/src/environment_preview.rs),
[local lights](../../crates/engine/src/lights.rs),
[lighting runtime](../../crates/engine/src/lighting_runtime.rs) and
[ordinary native response](../../crates/engine/src/nif_material/native_common.wgsl).
The [input contracts](skyrim-render-map/input-contracts.md) retain the native
proof limits; retained fields do not establish active rendering behavior.

## What applies to the rejected exterior views

The Riverwood comparisons select exterior Tamriel, clear weather `0x81A`, noon
and stable IMGS `0x12F88`. The interior-only inheritance, Fade and room findings
do not establish their darkness or glow. The exterior DALC path has no scalar
Ambient multiplier or source-preparation fallback. Its extra native ambient RGB
operand, live sun dimmer and selected retail world direction remain unobserved.

Supported native materials use unexposed response units with output scale one
when stable IMGS is active. Terrain, generic glTF and unsupported NIF families
use separate Bevy approximations. Fallback ambient keeps only the affine rows'
constant components. Its exposure bridge normalizes a Lambertian reference;
that does not match every PBR material, normal, specular or emissive response.
The run's preparation report contains native, fallback and other standard
primitives; actual per-draw coverage is still unobserved.

The displayed instrumented captures use the preserved producer-26 pack with
producer-25 meshes. Implementing producer-27 publication does not update that
pack. Asset activation must be checked independently before presenting another
capture as evidence of the source correction.

The corrected JSON inventory parsed all 30,061 GLBs without errors. It found
63,981 `nativeSurface` materials and 63,160 diffuse views. None of those views
declares `nativeUri`, `nativeSampleTransfer`, `nativeSourceFormat`,
`sourceDdsSha256` or `nativeQualificationStatus`. The runtime's legacy branch
therefore retains the compatibility texture projection. This is a concrete
activation gap in the new replays, not proof of their entire brightness cause.
Earlier private source-qualified packs also failed visual acceptance, as
recorded in the [coverage audit](skyrim-native-diffuse-coverage-20261010.md).

The inventory covers 25,388 manifest GLB outputs plus 4,673 additional LOD GLBs.
It reads JSON declarations; it does not verify original DDS blocks, KTX payloads
or selected GPU bindings. The private
`captured-pack-native-view-inventory-v2.json` receipt is SHA-256
`355b54882c43bb806eeca5860e85ea34d8003428f5b2fd9b479b011dc45cca5d`.

The selected stable IMGS arithmetic precedes an output bridge and later FXAA.
Diagnostic float replay is not a production-output dump. Request/view joins
pass, but the screenshot GPU-copy identity and image-frame join remain pending.
The earlier preparation report's EV9.7/TonyMcMapface/exponential-fog description
belongs to the older preview route; current code and receipts own this route.

## Corrections and validation order

1. Verify the actual test pack's native-view declarations and selected bindings.
   Preserve the old pack as a control. Converter fixture success is insufficient.
2. Capture actual native/fallback draws and production outputs for one material
   at a time. Resolve texture transfer, ambient operands, effective sunlight and
   the final output chain before changing brightness, saturation or fog.
3. Preserve per-field CELL candidates and unresolved template ownership for the
   42 rejected interiors. A rendered fallback needs an explicit policy; do not
   present fog's null-template behavior as proof for all lighting fields.
4. Close interior cube/rotation/Fade/sky ownership, then local-light selection,
   shadows/fading, typed external emittance and automatic room overrides against
   their native consumers. Use controlled tests for the observed rules.

No renderer coefficients, asset pack, build, GPU capture or game session were
changed for this comparison. Existing native getter bytes were rechecked within
their fog-only scope: 154 instructions matched the pinned executable. Invalid
intermediate scanner results are preserved and explicitly superseded by the
corrected inventory receipt.
