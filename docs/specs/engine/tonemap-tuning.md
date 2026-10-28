# Tone-map tuning with frozen visual references

Every rendering change must keep three images available: the previously approved
Mudcrab setup, the fixed Skyrim screenshot, and the current capture. Use
`scripts/capture_tonemap_tuning.py` to preserve those references and capture named
variants from one camera and INI. The runner updates a self-contained HTML
comparison after each validated stage.

The approved look remains the engine default. Experimental record grading and
tone-map changes are separate variants. A successful capture validates the
Mudcrab scene and its receipts; it does not establish Skyrim fidelity. The
reference screenshot's effective weather, exposure, modifiers, adapted luminance,
and GPU constants have not been observed.

## Capture a tuning campaign

Build the engine before starting the runner. It does not build or modify the
engine. Finish source edits before capture: the runner refuses source or
executable changes during a campaign. Use a fresh output directory for each
campaign so failed or older captures cannot be mistaken for new results.

```sh
python3 scripts/capture_tonemap_tuning.py \
  --config data/visual-comparisons/tonemap-tuning-config.json \
  --engine target/debug/engine \
  --assets /Users/taylor/Projects/mudcrab/modern_assets_204 \
  --output data/visual-comparisons/tonemap-tuning-YYYYMMDD/stage-01
```

The script requires Python 3 and its standard library. It adds the fixed
`--assets`, `--shots`, `--shots-out`, `--ini`, `--hidden-window`, and
`--shots-settle-timeout` arguments. Scene flags shared by every variant belong in
`common_args`. Each variant supplies its own rendering arguments and optional
expected receipt fields. Pass argument arrays, not shell command strings.

Do not change a variant's camera, INI, asset pack, or shared scene flags to improve
its comparison. Those changes need a separate scene campaign with explicit
reference limitations. Source changes need a fresh output directory; the same
pinned reference configuration should be reused.

## Pinned inputs and named variants

The configuration format is:

```json
{
  "schema_version": 1,
  "title": "Sleeping Giant tone-map tuning",
  "approved": {"path": "/absolute/path/approved.png", "sha256": "64 lowercase hex characters"},
  "skyrim": {"path": "/absolute/path/skyrim.png", "sha256": "64 lowercase hex characters"},
  "approved_receipt": {"path": "/absolute/path/approved/shots.log", "sha256": "64 lowercase hex characters"},
  "camera": {"path": "/absolute/path/camera.json", "sha256": "64 lowercase hex characters"},
  "ini": {"path": "/absolute/path/capture.ini", "sha256": "64 lowercase hex characters"},
  "common_args": [
    "--lights", "--stream-radius", "2", "--sun-position", "180,45",
    "--max-upload-mib-per-frame", "4", "--no-render-diagnostics"
  ],
  "variants": [
    {
      "name": "approved-default",
      "label": "Current approved default",
      "notes": "Approved sRGB grade, Cool Gray sky/fog, and current default tone mapper.",
      "args": [],
      "expect_receipt": {"color_grade": "snapshot-srgb", "visual_preset": "cool-gray"}
    },
    {
      "name": "cloudy-record",
      "label": "CloudyDAY authored grading",
      "notes": "Only CNAM/TNAM selection changes; this does not identify the Skyrim reference's weather.",
      "args": ["--color-grade", "record-srgb", "--image-space", "0x12F87"],
      "expect_receipt": {
        "color_grade": "record-srgb",
        "grading_source": {"loaded_record": {"form_id": 77703, "winner_plugin": "Update.esm"}}
      }
    }
  ]
}
```

Paths may be absolute or relative to the configuration file. Every input requires
a SHA256: a replaced reference fails instead of silently creating a new baseline.
The camera file must contain exactly one shot. All three images must have the same
dimensions. `expect_receipt` matches a subset of the captured JSON without filling
in omitted fields. Missing expected fields fail validation.

For JSON files consumed by future rendering options, add a top-level `files`
object. Each entry uses the same pinned input format. An argument consisting of
`{input:NAME}` expands to that file's frozen copy:

```json
{
  "files": {
    "retail-parameters": {"path": "retail-parameters.json", "sha256": "64 lowercase hex characters"}
  },
  "variants": [
    {
      "name": "curve-experiment",
      "args": ["--tone-map", "retail-curve", "--tone-map-params", "{input:retail-parameters}"]
    }
  ]
}
```

This is an extension to the full configuration, not a complete configuration by
itself. The runner does not interpret rendering option names or parameter schemas;
the selected engine must support them. Pin any file that can change the rendering
arguments, rather than passing a mutable external file path.

Optional `capture_timeout_seconds` and `settle_timeout_seconds` default to 600 and
300. `source_paths` defaults to `Cargo.toml`, `Cargo.lock`, `crates/engine/src`, and
`crates/shared/src`; add relevant repository-relative files when changing another
source dependency.

## Sleeping Giant references

Keep these originals throughout the current tuning work:

| Role | File under `data/visual-comparisons/` | SHA256 |
|---|---|---|
| Approved Mudcrab | `brightness-plus20-20261014T162641Z/capture/sleeping-giant-snow-fog.png` | `21c20f2b0b4c77d86c923d676a1c84ea78c3dc90d952d6021f3c0e82c5ecb768` |
| Skyrim | `skyrim-reference-recovered-20261014/fiji-sleeping-giant-8000.png` | `00a81e2a104aa6299fee6dc53a26abe5e5bb5e6c64fc4400c96f82edc48d8015` |

The approved receipt is `capture/shots.log` inside the approved capture directory.
Its `camera.json` and `capture.ini` provide the scene inputs. Their SHA256 values
must also be pinned in the configuration. These local reference images remain
outside the tracked repository.

The approved pose uses Creation position `[21750, -44530, 140]`, yaw 90 degrees,
pitch -3 degrees, and horizontal FOV `72.73435310118231` at 1920×1080. This yields
the measured 45-degree vertical FOV. Preserve that projection; replacing the
horizontal FOV with 80 changes the frame.

Automated shots pin water time to 1.0 seconds. The shared command keeps the fixed
sun at `180,45`. `--no-render-diagnostics` disables GPU diagnostic queries used by
Bevy; it avoids the previously observed Metal counter-buffer failure. Captures
with that flag do not provide GPU timing evidence.

## Receipts and review

Each campaign contains:

- `references/`: byte-identical copies of the approved PNG, Skyrim PNG, approved
  receipt, camera, INI, and any additional parameter files.
- `config.json` and `manifest.json`: the frozen configuration, commands,
  timestamps, independent engine/source hashes, asset provenance, per-stage input
  hashes, and validation results. `passed` remains false until all variants pass.
- `source-provenance.json`: Git head/status/diff hash and individual hashes of the
  selected source files. These identify the source state separately from the
  executable; they do not prove that executable was built from that state.
- Each named variant directory: `native.log`, `run.json`, original engine
  `capture/shots.log`, PNG, and complete `receipt.json`.
- `comparison.html`: original images embedded as data URLs. The three-image view
  always retains approved / Skyrim / current. Stage selection, whole-frame A/B,
  and a wipe compare current against approved, Skyrim, or the previous tuning
  stage at an equal fitted scale. Colors and image pixels are not edited.
- `comparison-preview.html`: the same page with absolute local PNG paths for
  T3's `html_preview` and `html_render` tools, which inline those images. Use this
  smaller source for inline review. The console reports checks for stage
  selection, A/B, wipe, frozen references, image dimensions, scale, aspect ratio,
  and horizontal overflow. Keep `comparison.html` for a self-contained offline
  copy.
- `comparison-{stage-name}-preview.html`: one stage and the same two frozen
  references, for inline services with an image-size limit. The complete offline
  gallery retains every stage without resizing or altering the original PNGs.

Validation checks the requested and captured camera position, orientation,
vertical FOV, render origin, PNG and receipt dimensions, settled state, timeout,
and pinned water time. The captured renderer must be ready, specialization
evidence must be known, and every pending/failure counter must be zero. Grading,
resolved values, and source evidence must agree with the immutable captured-frame
receipt. A later screenshot callback cannot relabel the frame with another grade.

The world database and available conversion/LOD manifests are hashed once. Their
size and modification time must stay unchanged during capture. The rest of the
asset pack is not exhaustively hashed. Capture readiness and equal camera inputs
do not prove equal geometry, materials, sunlight, weather, exposure, or native
processing between Mudcrab and Skyrim.

Review the gallery after each rendering change. Keep the prior campaign so an
improved local appearance is not confused with progress toward the Skyrim
reference. Repeated captures can vary even with the same binary and resolved
grading; different PNG hashes alone do not establish a rendering regression.

To revalidate a completed campaign and regenerate its gallery without launching
the engine:

```sh
python3 scripts/capture_tonemap_tuning.py \
  --validate-only data/visual-comparisons/tonemap-tuning-YYYYMMDD/stage-01
```

Focused workflow tests use synthetic receipts and a fake engine process:

```sh
python3 -m unittest discover -s scripts/tests -p test_capture_tonemap_tuning.py -v
```
