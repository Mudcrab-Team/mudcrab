# Third-party code

Mudcrab is licensed under the GNU General Public License v3 or later ([LICENSE](LICENSE)), except
for the files below, which are derived from other projects and keep those projects' terms. Each
file names its source in an SPDX header and a comment at the top.

## Community Shaders

[Community Shaders](https://github.com/community-shaders/skyrim-community-shaders) re-implements
and extends Skyrim Special Edition's shaders in HLSL for Direct3D 11. Its code is licensed under
the GNU General Public License **version 3** (no "or later" grant), with a Modding Exception and a
GPL-3.0 Linking Exception (its `EXCEPTIONS.md`). Those exceptions cover linking against Skyrim
and SKSE, which Mudcrab does not do. As `EXCEPTIONS.md` allows, Mudcrab's derived files drop
them and are licensed under plain **GPL-3.0-only**.

Because these files are GPL-3.0-only, a Mudcrab build that includes them can be distributed
under GPL-3.0 only, not under a later version.

| Mudcrab file | Derived from (Community Shaders path) | License |
|---|---|---|
| `crates/engine/src/shaders/height_fog.wgsl` | `features/Exponential Height Fog/Shaders/ExponentialHeightFog/ExponentialHeightFog.hlsli`, `features/Exponential Height Fog/Shaders/ExponentialHeightFog/VolumetricFogCommon.hlsli`, `package/Shaders/ISSAOComposite.hlsl` | GPL-3.0-only |

Code that Community Shaders itself took from elsewhere under other licences (see its
`package/SKSE/Plugins/CommunityShaders/Licenses/THIRD_PARTY.md`) is not used here. Mudcrab does
not use the Community Shaders logo or name it as an endorsement.

### Adding a file derived from Community Shaders

1. Start the file with `// SPDX-License-Identifier: GPL-3.0-only`, then name the Community
   Shaders source files it ports, the copyright line
   `Copyright (C) the Community Shaders contributors`, and what Mudcrab changed.
2. Add a row to the table above.
3. Check the source file's own header and the Community Shaders `THIRD_PARTY.md` first: parts
   under MIT or Apache-2.0 keep that licence and its notice, and need their own row.
4. Do not port from `features/Upscaling` (NVIDIA Streamline and AMD FidelityFX have their own
   licences), `features/RenderDoc`, or the Community Shaders logo.
