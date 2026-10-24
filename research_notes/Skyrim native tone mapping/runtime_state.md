# Observed Skyrim tone-map state and comparison captures

Follow-up: [captured GPU state](gpu_state.md) identifies the selected program,
bound inputs, and a five-pixel arithmetic check for the later frame 59881.
Statements below about unobserved GPU state describe this earlier CPU/capture
campaign. The historical reference screenshot's effective inputs remain unknown,
including its native controlling shadow-distance input.

Research date: 2026-10-15. The isolated Fiji session now supplies observed
settings, resolved image-space fields, and CPU parameter tuples. Independent
native traces close [parameter packing](gpu_binding_trace.md) and
[the reciprocal-gamma exponent](output_transfer_trace.md). The approved Mudcrab
TonyMcMapface/sRGB preset remains the default; this work changes no engine code.

## Observed values

The collector read executable `1.7.104.0`, SHA-256
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
It verified the mapped file identity, relocated PE headers, exact HDRParameters
instructions, setting-name pointers, object vtables, and bounded parameter counts.
Wine rewrote only the PE ImageBase header field to the actual mapped base; that
exact change is allowed and recorded. Other header changes fail collection.

| Item | Observed process value | Evidence and meaning |
| --- | ---: | --- |
| `bUseFilmicCurve:Display` | `0` | Setting byte; ordinary CPU curve tuple also contains branch flag `0`. |
| `fReinhardWhiteScale:Display` | `1.100000023841858` | Effective binary32 setting, also shown as `1.10` by console. |
| `fFilmicWhiteScale:Display` | `1.399999976158142` | Effective binary32 setting; filmic branch was not selected by these CPU inputs. |
| Resolved White | `1.0` | Manager resolved data at `+0xC8`, White at `+0x14`. |
| Ordinary curve coefficient | `0.8264462351799011` | CPU float4 index 1, component y; agrees with the binary32 White/scale formula. |
| `fGamma:Display` | `1.0` | Derived final exponent is `1` on the audited route; GPU contents were not captured. |
| Global adaptation speed scale | `2.0` | Named live setting. |
| Global adaptation strength scale | `0.4000000059604645` | Named live setting. |
| Menu adaptation timer setting | `0.30000001192092896` | Named live setting; reading it alone does not establish selection. |

The complete decoded resolved tuple was:

| Field | Observed value, rounded |
| --- | ---: |
| Eye Adapt Speed | 45 |
| Bloom Blur Radius | 7 |
| Bloom Threshold | 0.8 |
| Bloom Scale | 4 |
| Receive Bloom Threshold | 0.8 |
| White | 1 |
| Sunlight Scale | 2.8 |
| Sky Scale | 0.05 |
| Eye Adapt Strength | 5 |
| Saturation | 1.5 |
| Brightness | 1.135 |
| Contrast | 1.4 |
| Tint amount | 0.42 |
| Tint RGB | `(0.8941176534, 0.8392156363, 0.7725489736)` |

These fields closely agree with the archived ClearDAY record. Resolved tint
green/blue are one binary32 ULP below the base/record values
`0.8392156958580017` and `0.772549033164978`. The record-to-runtime comparison
therefore is not bitwise equality. Agreement alone does not identify an active
IMGS record or exclude copied/blended/override data.
Console frames separately read `GetIsCurrentWeather 81a = 1.00`,
`GetCurrentWeatherPercent = 1.00`, and `gamehour = 12.0000`.
`PrintHDRParam` independently prints the selected base fields and global boosts;
it does not substitute for the resolved-memory observation.

Fresh Fiji `Skyrim.esm` and `Update.esm` SHA-256 values match the archived plugin
checksums documented in [the authored extraction](authored_settings.md). The
configuration receipt also hashes all five ESMs and five loaded native-plugin
DLLs. This strengthens the current record-source comparison; it does not recover
the historical screenshot's effective state.

The ordinary CPU tuples were `(0.8, 0.8264462352, 0, 0)`,
`(1.5, 0, 1.4, 1.135)`, and
`(0.8941176534, 0.8392156363, 0.7725489736, 0.42)`, respectively.
The static uploader maps those indices to `cb2[2]`, `cb2[3]`, and `cb2[4]`.
The sampled fade predicate selected the ordinary route. No live draw or GPU
constant-buffer contents were observed.

## Capture linkage and remaining limits

The verified native camera export contains position `[21750, -44530, 140]`,
rotation `[6.2308254, 1.5707964]` radians, FOV `80`, and roll `0`.
Normalized pitch is approximately -3 degrees; yaw is approximately 90 degrees.
This reproduces the previous Skyrim camera export. Skyrim-to-Mudcrab projection,
sunlight, weather, fog and exposure calibration remains incomplete.
The captured native Documents INI on disk contains `fShadowDistance=23200`,
while the frozen Mudcrab INI uses `8000`. A later campaign's console readback
reported `getini "fShadowDistance:Display"` as `8000` (see [the later captured
GPU state](gpu_state.md#capture-identity-and-scene-controls)); it does not show
that Skyrim consumed that value for these earlier captures. The native
controlling input for the primary and repeat captures remains unresolved, as it
does for the historical reference screenshot. The INI and later console values
cannot establish matched shadow-distance inputs or an override mechanism.

Two fresh 1920×1080 Gamescope captures were retained with Photo Mode UI and HUD
visually hidden. Their SHA-256 values are:

- Primary: `10568f53d80864fa2a5c043201c143f368fdbe3cae7f8fb51d6bdcad9e419e77`.
- Repeat: `e0f41401e4ec0cdc6e5dd381098e669fee5817a4f4b677abb8c89fac8fc22490`.

Process reads bracketed these captures. Each collection's two sequential samples
agreed, with consistent pointer/selector guards. Settings, resolved fields, and
ordinary/fade tuples also agreed between the before/after collections.
Adaptation CPU coefficients changed: index 2 was
`(0, 0, 1, 0.9953946471)` before and
`(0, 0, 0.6837722063, 0.2583801150)` after. The timer selector, its units, sampled
adaptation texels and history were not read. Do not infer a selected timing branch
or adaptation history from those tuples.

Reads and captures are sequential, not synchronized to one GPU frame. The primary
and repeat PNGs differ. Freeze state has no independent native readback, and no
bitwise reproducibility claim follows. These observations do not establish the
settings of the historical Skyrim reference screenshot. Both original reference
hashes remain unchanged.

## Mudcrab comparisons

Three new native Metal captures use the original approved and Skyrim references:

1. Unchanged approved Tony/sRGB control.
2. Observed Reinhard scales with loaded ClearDAY White and approved sRGB grading.
3. The same curve with ClearDAY record grading.

Stage 2 changes the curve relative to stage 1. Stage 3 changes only the grading
source relative to stage 2. The curve parameter JSON derives its branch/scales
from the retained live receipt; White remains loaded from IMGS. Camera, sun,
sky/fog, EV100, INI and water time stay fixed within this Mudcrab campaign.
The native record grading remains visibly darker in the inn's shadowed areas.
This comparison does not isolate grading from cross-renderer lighting differences.

`P.x=P.y=1` remain explicit, unmeasured experiment inputs. Native adaptation,
bloom, sampled contrast pivot and complete output transfer are not reproduced.
The earlier ratio `1.25` and White scale `2.0` remain artistic overrides; the new
captures do not promote them to recovered Skyrim values. No default was changed.

## Reusable collectors and validation

- [Memory collector](../../scripts/inspect_skyrim_tonemap_memory.py): read-only
  `/proc/PID/mem` access, ASLR/code/name/vtable guards, two samples and raw bytes.
- [Console collector](../../scripts/capture_skyrim_tonemap_state.py): 13 fixed
  read-only queries, fresh complete CRC-checked PNGs, raw OCR and failure receipts.
  Numeric OCR remains unparsed. Review echoed commands and results together.
- [Operational commands](../../tools/skyrim-comparison/README.md).

The Skyrim Python test selection passed 51 tests, and the existing tuning runner's
10 tests also passed. The Mudcrab capture runner
validated all three stages, including readiness, camera, source/frame metadata
and frozen-reference hashes. Engine binary and recorded source hashes match the
previous validated build; no engine rebuild or new Rust/GPU numerical test run
was needed for unchanged engine code.

Fiji's deployed runner and display helper were stale. Their previous versions
were backed up, then replaced with the already-tracked versions. Only comparison
services started by this task were relaunched. An isolated `steam_appid.txt` was
added after the first cold launch redirected to Steam's normal launcher. A warm
retry also occurred, so this is a recorded workaround, not an isolated root-cause
finding. Normal Skyrim/FUS game files were not edited, and no game save was made.

## Local receipts

Evidence root: `tonemap-runtime-20261015T204321Z` (private historical evidence outside Git: `data/visual-comparisons/tonemap-runtime-20261015T204321Z`).
Original PNGs, raw native reads, logs and deployed-helper backups remain outside
Git. Important files under that root are:

- `collected/tonemap-runtime-20261015T204321Z-memory-before-capture/receipt.json`
  and the corresponding `memory-after-capture/receipt.json`.
- `collected/tonemap-runtime-20261015T204321Z-console/receipt.json` and all 14
  initial/query PNGs with OCR.
- `collected/tonemap-runtime-20261015T204321Z-final/receipt.json`, including native
  camera export, process/display identity and before/after links.
- `collected/tonemap-runtime-20261015T204321Z-config/receipt.json`, with active INIs,
  native plugin hashes, five ESM hashes and isolated launch-helper provenance.
- `mudcrab/manifest.json`, per-stage receipts and `mudcrab/comparison.html`.

Remaining evidence priority: selected GPU programs and buffers, adaptation/history
texels, scene/bloom resource domains and formats, and the complete output chain.
Those inputs are needed before a fuller native pipeline can be validated.
