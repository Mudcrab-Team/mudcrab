# Lighting and color model

This model tracks ownership from source records and texture samples to displayed
pixels. Its graph describes conceptual data dependencies, not a recovered frame
timeline. Every stage has separate source, CPU, shader and selected-execution
questions. The [JSON graph](world-model.json) links stages to the existing native
contracts, supplied-note claims and current implementation files.

The target remains the pinned SkyrimSE `1.7.104.0` executable and shipped shader
package. LE, other SE/AE builds, ENB and Community Shaders have separate scopes.
No edge is promoted to a complete retail input-to-pixel connection.

N003 extends N001/N002 with bounded surface-lighting and authored-data evidence,
image-space equations from the pinned shader package, and separate mod-source
interventions. It challenges N002-L21’s attenuation expression with a
reconstruction-to-Mudcrab match, while target shader execution remains unknown.
Its XCLL mask and weather/DALC findings refine serialized layouts without
settling runtime ownership or effective values. Read the [N003 intake](note-0003/intake-summary.md)
and linked ledgers for claim-level scopes and unresolved questions.

| Ownership stage | Values and domain | Current boundary |
| --- | --- | --- |
| Resource winners | Winning plugin, record, mesh, DDS and shader package identities | Retained source is available; active retail override winners remain unobserved. |
| World and cell selection | Weather/time weights, CELL/LGTM/REGN/CLMT, room and sky flags | Selected static consumers exist; automatic weather/room ownership and interior precedence remain partial. |
| Material and geometry | NIF flags/constants; authored normal/tangent/bitangent, vertex colors, transforms | Selected native uploads and layouts are mapped. Actual source streams and specialized shader selection remain open. |
| Texture sampling | DDS format, UNORM/sRGB view, sampler, slot and sampled code values | Legacy native diffuse UNORM creation is recovered. Code values do not establish physical linear light; actual draw bindings remain open. |
| Directional and ambient preparation | Byte colors, affine ambient rows, sunlight scale, dimmers and spaces | Native static segments exist. Live extra ambient RGB, dimmer, parent transform and source precedence remain open. |
| Local lights and shadows | Per-geometry candidates, radius/attenuation, LOD fade, masks and visibility | Project camera-nearest lights are a preview. Native ranking, overflow, shadow allocation and most local equations remain unresolved. |
| Surface response | Diffuse/specular/ambient/emission and specialized skin, hair, snow, effects | Ordinary native response is scoped; terrain and unsupported surfaces use Bevy approximations. This mixed route is not full-scene parity. |
| Scene composition | Fog, water/reflections, alpha and volumetrics | Selected fog arithmetic and conditional calls are mapped. Actual pass/resource order remains uncaptured. |
| Image-space composition | Stable IMGS fields, settings and ordered IMAD operations | Static field packing and modifier segments are mapped. Active modifier curves/lifecycle and per-view ownership remain open. |
| Luminance and adaptation | Reduction samples, XY history, rates, time and minimum step | Shipped scoped equations differ from N002's generic log-average/exponential model. Selected history/resources still need retail receipts. |
| Bloom and tone | Component thresholds, discrete blur taps, curve selector and white normalization | Static ordinary HDR route is mapped. Effective settings, samples and enabled branches remain unobserved. |
| Cinematic and output | Saturation, brightness, contrast pivot, tint amount/RGB, gamma, output view | Shipped arithmetic differs from N002's formulas. Final production output and display transfer still require a selected resource chain. |
| Mod interventions | Hooks, replacement shaders, clustered buffers, feature settings | Pinned source describes replacement behavior with finite budgets. It does not establish vanilla behavior or active features. |

## Proof questions carried between layers

The model keeps four checks separate: which authored value wins; how CPU code
packs it; how exact shader bytecode consumes it; and which branch, program,
resource and value actually reaches a selected draw. A missing check leaves the
connection open even when the neighboring arithmetic is known.

For current rejected exterior views, investigate texture-view activation,
native versus fallback material responses and the production output chain.
Interior template, Fade, FX and room findings remain work items with their own
scene scopes. They do not establish the exterior brightness cause.

The supplied grading formulas do not authorize changing the implementation.
N002's adaptation, threshold, contrast and tint claims must be compared with
their exact shipped programs. Likewise, metadata entry 25 is not pixel register
25: its program-specific offset must connect native packing to the chosen
constant-buffer vector. A seven-element reconstruction or project array does
not prove native light ranking or a global frame limit.

## Evidence priority

Close the selected texture/view and production-output connections first, with
the previous pack retained as a control. Then trace effective ambient and sun
inputs and per-material response. Recover local-light candidate/packing and
shadow allocation separately from the mod clustered-light implementation.
Continue interior source precedence, active IMAD curves, specialized material
families and room ownership as their own branches. Record observations beside
the static segments before using them to select visual coefficients.
