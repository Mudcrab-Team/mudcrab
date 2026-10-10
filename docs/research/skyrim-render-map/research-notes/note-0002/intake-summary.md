# N002 intake: rendering architecture and color grading

The article is preserved unchanged. Its [section map](source-sections.json)
records literal byte spans and related claim IDs, including assertions not yet
audited individually. The audits cover native lighting/materials, image-space
operations and mod interventions. They seed the [lighting/color model](../world-model.md);
they do not establish a complete rendering reconstruction.

## Findings that change the investigation

The supplied grading equations conflict with scoped shipped programs. The
audited luminance path uses arithmetic reduction; adaptation has two components
and a bounded update; bloom tests color components; contrast uses the adaptation
value as its pivot; tint interpolates toward luminance multiplied by tint RGB.
The original formulas and exact excerpts remain in the
[image-space ledger](claims-imagespace.json). Effective selected resources and
values still need their own receipts.

The supplied constant table resembles source metadata IDs, which are separate
from shader-specific GPU offsets. Existing native evidence maps metadata entry
25 to pixel `cb1[4]` for two package-associated Lighting programs, and entry 28
to `cb1[7].xy` for one. Other entries, actual selected programs and committed
buffers remain separate checks. The [native-lighting audit](native-lighting-audit.md)
also preserves the gap between shader array capacity, per-object light selection
and global shadow allocation.

The mod comparison needs configuration and revision boundaries. The pinned
Community Shaders Light Limit Fix source has finite buffers with a 1,024-light
bound; it does not substantiate the article's over-two-billion ceiling. ENB's
author documentation exposes a setting to retain original postprocessing.
Capabilities listed in a comparison table do not establish enabled features.
The [mod ledger](claims-mod-interventions.json) keeps these source interventions
separate from vanilla behavior and unresolved runtime observations.

Weather inputs contain four stored color keys per NAM0 category and four DALC
chunks in the retained layout. Those fields do not establish eight distinct
authored stages or a weather-stored sun direction vector. Existing sun/CLMT
trajectory segments and active weather transition state remain separate links.

## Retained work

Trace source fields through CPU packing, exact shader operands and selected
resources. Continue per-object light ranking, attenuation/fade, shadow-mask
allocation, interior/room precedence, active modifier curves and specialized
material families. For current exterior captures, retain the separate texture
activation, mixed material response and production-output questions from the
[remediation plan](../../../../specs/engine/skyrim-rendering-remediation-plan.md).

This intake made no renderer, asset-pack or coefficient changes. Its checks
cover provenance and reference coherence. Previous numeric or GPU tests do not
establish the supplied note's claims or improve the rejected screenshots.
