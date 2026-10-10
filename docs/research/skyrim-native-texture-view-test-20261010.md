# Source-qualified native texture-view test — 2026-10-10

Correcting the native diffuse texture view restores much of the farmhouse roof, wood and stone brightness without changing authored lighting or grading. Whole-scene brightness remains open: foliage and ground stay dark, and the no-fog roof clips many final channels. The [native trace](skyrim-native-texture-transfer-20261010.md) proves the source sampling discrepancy; this test measures its image effect.

## Runtime and asset scope

The native material hook now reads explicit DDS provenance and loads a separate native diffuse URI. Legacy DXT1/3/5 use identity/UNORM sampling. Explicit typed DX10 BC1/2/3/7 UNORM or sRGB formats retain their respective transfer. Unknown, incomplete or conflicting metadata rejects native selection. Older unannotated assets retain their PBR compatibility view. Both resolved asset identity and UV0 are checked; the replacement handle participates in scene and streaming dependencies.

Each annotated `nativeSurface.textureViews` diffuse record adds `nativeSampleTransfer`, `nativeSourceFormat` (`kind` and `value`), `sourceDdsSha256` and `nativeUri`. The PBR `uri`, `sourceUri`, `colorSpace`, authored parameters and sampler remain unchanged. The distinct native alias retains the same KTX2 bytes; explicit loader settings select the typed UNORM view. This preserves filtering before the shader rather than encoding a previously decoded sample.

The [preparer](../../scripts/prepare-native-texture-view-test.py) qualifies the complete base manifest and private original-DDS/KTX2 proof by SHA-256. It annotates only matching authored diffuse slots and current KTX2 hashes, hardlinks the pack, then atomically replaces changed GLBs and the manifest. Eight verified diffuse textures are reused in 301 GLBs / 749 material views. These are file counts, not visible draw counts. The original 85,001 source files remain unchanged; changed GLB binary tails and existing JSON values are preserved. The derived manifest records every changed output hash and an explicit diagnostic qualifier.

This preparation does not implement the normal converter's original-format publication, native URI dependency, pruning, repair, validation or cache contracts. Those remain required before durable package publication. The [evidence JSON](skyrim-native-texture-view-test-20261010.json) records source/binary/package identities, selected regions and control results.

## Verification and image effect

All **447 engine library tests** and strict engine library/binary/example Clippy pass. Loader tests retain separate PBR/native handles, matching compressed bytes, alpha, two mip levels, sampler settings and scene dependencies. Both explicit DX10 transfers are tested; invalid source provenance is rejected.

On Apple M1 Pro Metal, **28 material GPU cases pass**. At a two-texel midpoint, UNORM produces `(0.250977, 0.125488, 0.5)` and sRGB produces `(0.107910, 0.025635, 0.5)`, against independent pre-filter transfer references. Both preserve alpha `0.450928`. Forcing the old extra decode fails only the new UNORM case, with maximum RGB error `0.14307` and exit status 1.

Four current world captures use the same frozen executable, 1920×1080 fixture, clear weather `0x81A`, hour 12, authored-preview lighting, supported IMGS and the default display-encoded output bridge. All exit 0 and settle with zero pending/unavailable dependencies. All seven comparison sets pass 16 recorded controls; 15 independent migration gates pass. The new executable with the original pack reproduces all eight fog-enabled image regions byte-for-byte. Its no-fog repeat matches seven regions exactly; the distant mountain has a small residual difference.

Selected mean relative luminance, measured from final PNG sRGB values:

| With fog/HDR | Original view | Native UNORM view | Ratio |
| --- | ---: | ---: | ---: |
| Roof | 0.04973 | 0.29319 | 5.90 |
| Shaded wood | 0.00961 | 0.03411 | 3.55 |
| Stone | 0.05103 | 0.19656 | 3.85 |
| Grass/soil | 0.02849 | 0.02389 | 0.84 |
| Sky | 0.34716 | 0.31562 | 0.91 |

The corrected wood is about 12.5% below the earlier bright capture; roof and stone remain about 43.7% and 31.2% below it. Grass/soil remains about 90.3% below it. These comparisons preserve the user's brightness reference; they do not establish a retail calibration target.

Fog interacts with the corrected input: shaded wood's mean Y decreases about 8%, while its tenth-percentile Y rises about 37.5% and the fraction of very dark pixels falls from 2.14% to 0.058%. Fog fills the darkest portion and dims other pixels. Roof channels at final value 255 occupy about 0.61% with fog and 24.92% without it. Disabling fog also bypasses geometry clamps/finishing, so these are full-path comparisons rather than isolated fog-blend tests.

Changing the sampled scene input changes HDR adaptation, bloom and grading outputs elsewhere. Unqualified grass and sky therefore change even though their texture bindings remain unchanged. Lighting-report preparation counts differ between runs; visible GPU mesh/shadow-caster counts match. Identical complete preparation or adaptation history is not claimed. Native lighting units, source sun trajectory, foliage response, terrain shading and the final presentation chain still need independent evidence.

A bounded source review found no equivalent extra decode in native sunlight or directional ambient. [Environment preparation](../../crates/engine/src/environment_preview.rs) converts authored RGB bytes with `1/255` and blends them directly; native uniforms retain those values. [Runtime preparation](../../crates/engine/src/lighting_runtime.rs) applies `SunlightScale` once after replacing the input. The current approximate sun reaches zenith at noon, leaving ideal vertical walls with zero direct Lambert contribution. Existing native traces corroborate byte normalization and ambient preparation, but retail sun motion and effective light producers remain untraced. These normalized code values do not establish a physical color domain.

## Local visual review

```sh
/Users/taylor/.local/share/mudcrab-research/brightness-20261010/test-native-views.sh
```

Append `--no-fog` for the second tested path. The launcher selects the frozen test binary and isolated source-qualified asset pack; it leaves the existing world launcher defaults intact. Private captures, metrics, migration inventory and interactive comparison are under `brightness-20261010` in the research directory. The [central plan](../specs/engine/lighting-and-shading.md) owns integration order; the [subsurface audit](skyrim-subsurface-audit-20261010.md) records the missing authored foliage response.
