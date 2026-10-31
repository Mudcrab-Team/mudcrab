# Retail rendering observations required to close the map

Static native code proves operations and possible connections. It does not reveal
the active object, branch, resource or value in a particular frame. The rendering
map therefore keeps a separate observation receipt for each selected draw and
dispatch. An uncaptured value remains null; it is not replaced with a compiled
initializer, a community default or a coefficient fitted to screenshots.

## Session identity

Record the executable version and SHA-256, loaded module identities, shader
archive and loose shader identities, plugin winners and resource overrides.
Attach the launch route, graphics API and any translation layers. Record all
loaded INI paths, their hashes and effective settings after initialization.
Include display/output mode, resolution, render scale, refresh/present interval,
driver/GPU identity and active anti-aliasing mode.

For each view record the worldspace, CELL, reference or camera identity, position,
orientation, projection, near/far planes, jitter, view origin and clipping state.
Record game time, real time, active/next weather and transition fraction, CLMT,
IMGS, IMAD instances and weights, VOLI, water state and interior/room ownership.
Initializers and configuration-file values remain separate from effective values.

The refreshed Fiji probe establishes an installed executable matching the native
target and a Steam/Proton installation. It found no running Skyrim or RenderDoc
process. It does not prove that the game starts, that the configured settings
take effect or that a translated frame matches Windows output. See the dated
probe in [frame topology](frame-topology.json).

## Frame resource receipt

Capture resource identity, allocation generation, lifetime, extent, mip/slice,
sample count, native DXGI format, bind flags and every view's typed format.
Retain RTV, DSV, SRV and UAV identity separately. A texture format alone does not
determine the sampled or stored transfer function: typed views and shader
arithmetic must also be checked.

Attach copies of constant-buffer bytes at the actual draw/dispatch, buffer slot,
offset, size and update generation. Include dynamic buffers, shader metadata
remapping, samplers, blend/raster/depth-stencil state, viewport/scissor, vertex
streams, strides, offsets, index format/topology and instance streams. Distinguish
cached state changes from the state committed to the device context.

Identify each shader by the original bytecode hash and technique key. If an API
translation changes the shader representation, retain the original D3D11
selection/binding receipt as well as the translated capture identity. A similar
shader name or instruction footprint does not establish identity.

## Draw and dispatch receipt

Record the native callsite, pass owner, view owner, material instance, source
mesh/partition, input layout and permutation selection. For indirect callbacks,
capture the object pointer, adjusted subobject pointer, vtable address, slot and
resolved target. Preserve relevant branch values and settings.

For each material, follow source fields and texture winners through native field
preparation, uploads and the selected shader operands. Include effective sun RGB,
dimmer, SunlightScale, direction and parent transform; ambient rows and their
source reads; local-light identities/count/order; material overrides; texture
formats; authored vertex frame decoding; and alpha/fade controls.

For each full-screen or compute pass, retain input/output resource receipts and
the dispatch/draw dimensions. Track temporal/history buffers across frames,
ping-pong swaps, resets and camera changes. Read intermediate pixel values with
their actual format and view semantics. Preserve saturation/clamp locations and
the final swapchain view; a final screenshot cannot locate an earlier clamp.

## Representative coverage

| View or feature | Required observations |
| --- | --- |
| Exterior clear noon and late afternoon | Fixed camera; original terrain, farmhouse stone/wood/thatch, foliage and sky; selected weather/IMGS, sun and ambient inputs; shadows, fog and postprocess stages. |
| Weather transition and dense fog | Current/next WTHR, interpolation weights, fog/VOLI constants, depth inputs, opaque/transparent distinction and selected composite variants. |
| Interior and room boundary | CELL/LGTM field-specific inheritance, ambient cube, directional/local lights, room/portal ownership, IMGS/IMAD state and transition timing. |
| Near terrain, LOD and trees | Original geometry/layout, layer/atlas winners, morph/fade/wind state, material selections and cast/receive participation. |
| Grass and alpha foliage | Packed streams, deformation, alpha/discard/fade, blend/depth state, shadow/depth/color consistency and fog ownership. |
| Skin, face, hair and eyes | Specialized permutations, tint/normal/mask views, palette/morph inputs, soft/rim/back response and local-light selection. |
| Snow, parallax and environment maps | Exact feature key, source parameters/masks, resource/view formats and branch-specific equations. |
| Water and underwater transition | WATR selection, reflection/refraction/depth targets, displacement/time, selected pass order, fog and temporal state. |
| Particles, effects and decals | Controllers/time, texture overrides, geometry inputs, raster/blend/depth state and ordering around fog/postprocessing. |
| HDR, bloom and adaptation | Effective IMGS/IMAD composition, luminance/history state, reduction/blur targets, selected tone/composite shaders and intermediate values. |
| SAO, shadows, reflections and volumetrics | Settings/quality branches, sample resources, reconstruction inputs, history/blur state and composition targets. |
| TAA, motion blur, depth of field and UI | Enabled callbacks, history ownership/resets, camera/object motion, focus constants, UI ordering and final target/view state. |

Coverage of a row requires its actual branch and resource receipts. A program in
the shader package, a class in RTTI or a registered setting does not count as an
observation. Disabled features need their guard values and skipped path recorded.

## Implementation acceptance

An implementation contract can close when its source identity, native producer,
selected CPU/GPU connection, arithmetic, state and output owner are established
for the stated scope. Match numeric inputs and intermediate outputs before
judging the final image. A mismatch must name the first divergent stage and its
evidence. Keep alternate modes and unsupported permutations as separate gaps.

The old L0 screenshots have incomplete session metadata. They remain useful
visual references but cannot establish exposure, gamma, fog, lighting constants
or postprocess defaults. The rejected Mudcrab captures likewise cannot supply
missing retail values.
