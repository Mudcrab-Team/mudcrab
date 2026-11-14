# CPU grass parity checker

Run both generators for the same cells, then compare individual plants within
each cell and form. The dumper executes the production generation code with an
observer; it does not create an application, renderer, graphics device, or game
process. Python tooling uses only the standard library.

Keep all game inputs read-only and all dumps, caches, logs, and receipts outside
the source repository. Use explicit paths on the authorized data drive. On
Windows set `TEMP` and `TMP` to the authorized temporary directory before
building or running. Use `python -B` to avoid creating bytecode caches.

## Prepare inputs

Build the example from the revision being measured using the project's normal
build environment and shared build lock:

```text
cargo build -p engine --release --example grass_dump
```

Retain the exact build command, source revision, working diff, and binary hash.
A binary hash alone does not establish which source revision produced it.
The example uses `EngineConfig::default()` and the Tamriel world (`0x3c`).
Both input caches must describe the same source content and load order; changing
one side's inputs invalidates a claim about generator parity.

The cell list is a headerless UTF-8 CSV, one integer `cell_x,cell_y` pair per line. The MudCrab dumper also accepts an optional third hexadecimal worldspace ID for input-alignment diagnostics; the spud list remains two columns.
Cells are 4096 Skyrim units wide. Floor division determines cells for negative
coordinates. Include cells with zero grass; their dump files contain a header.
Use a grass-dump-enabled `skytool` executable and its compatible world cache.

## Rerun both dumps and compare

In PowerShell, assign the variables below to existing executable/input paths
and a **new** output directory, then run from the repository root:

```powershell
python -B scripts/grass-parity/run.py --ours-bin "$oursBin" --grass-cache "$grassCache" --skytool "$skytool" --cache "$spudCache" --world "$spudWorld" --cells "$requestedCells" --out "$newRunDirectory"
```

`--grass-cache` is our converted grass-cache file. `--world` is spud's world
file; `--cache` is spud's explicit cache root. The wrapper passes `--cache` to
spud on every run. The wrapper currently requires Windows `LOCALAPPDATA` and
refuses to run if its default `spud` directory exists. It checks again after
each completed command and records the final presence check. It does not
delete that directory. These checks detect an accidental default-cache write;
they are not a filesystem sandbox.

The wrapper runs these commands sequentially:

```text
SKYTOOL --cache SPUD_CACHE grass-dump SPUD_WORLD REQUESTED_CELLS OUTPUT/spud
GRASS_DUMP GRASS_CACHE REQUESTED_CELLS OUTPUT/ours
python -B scripts/grass-parity/compare.py --ours OUTPUT/ours --spud OUTPUT/spud --out OUTPUT/comparison
```

It retains `command-0.log` through `command-2.log`, a copy of the requested
cells on each side, and `receipt.json` with UTC time, commands, input/binary
SHA-256 hashes, command exit codes, default-cache checks, and CSV output hashes
on success. A failed command stops subsequent commands and retains the receipt
and logs. Source/build provenance should accompany this receipt.

## Compare existing dumps

This command is portable across platforms with Python; it reads the existing
dumps and writes only comparison outputs:

```text
python -B scripts/grass-parity/compare.py --ours OUR_DUMP_DIRECTORY --spud SPUD_DUMP_DIRECTORY --out COMPARISON_DIRECTORY
```

The comparator reads only `cell_<x>_<y>.csv` instance dumps. Aggregate files
such as `cell-form-counts.csv` are not instance inputs. Optional headerless
`requested-cells.csv` manifests make a cell absent from both sides visible.
Missing cells and malformed records mark the result incomplete. Malformed
records are skipped with bounded detailed warnings and a total skipped count;
valid neighbouring records remain available for comparison. Exit status is
`0` for complete input, `1` for an incomplete comparison, and `2` for a fatal
CLI/input/output failure. Inspect warnings before interpreting match rates.

Required shared columns are `cell_x`, `cell_y`, `ordinal`, `form_id`,
`x_su`, `y_su`, `z_su`, `scale_x`, `scale_y`, `scale_z`, `brightness`, and the
nine row-major matrix fields `r00` through `r22`. Our rows also supply these
sixteen numeric fields with `packed_` prefixes, `stage` (`base` or `layer`),
and `obstructed` (`0` or `1`). Position aliases `world_x`, `world_y`, `world_z`
and their packed equivalents are accepted. Extra reference columns are
ignored. The dumps share the relevant decoded numeric columns; ours does not
reproduce spud's raw half-word/packed-word representation. `form_id` is the identity.

## Meaning of the measurements

`source` compares our observed candidates before obstruction with spud's
decoded exported values. Our observer retains production selection order,
source positions and appearance randomness. The dumper reconstructs the
reference's binary16-rounded rotation, brightness, and height delta. Thus
source means reference export precision, rather than an unrounded mathematical
placement. Numeric CSV fields are recovered as IEEE binary32 before matching,
so two different decimal spellings that round to the same float do not create
an exact mismatch.

`packed` instead decodes our actual production instance representation into
the same coordinate frame and compares it with the **same spud reference**.
This includes position quantization, byte angle/scale/brightness, and quantized
normals. It measures the combined generator and packing difference. It is
not a comparison between two equivalently quantized encodings. The CPU decode
mirrors the default shader formulas; it does not measure GPU arithmetic,
wind, lighting, mesh transforms, frustum culling, or distance/fade selection.

Positions are world-space Skyrim units with Z up. Each cell/form gets a
deterministic one-to-one greedy nearest-neighbour assignment in **three**
dimensions. Exact positions match first; duplicate-position ties use generation
ordinal. Remaining candidate edges within eight units are sorted by distance,
then ours ordinal, then reference ordinal, and accepted when both endpoints are
unused. A 3D spatial hash bounds the neighbourhood search. This is greedy
nearest-neighbour matching, not an optimal or maximum-cardinality assignment.

All position thresholds are inclusive Euclidean distances: exact (`0`),
`<=1`, `<=2`, and `<=8` Skyrim units. Each threshold reports disjoint matched
pairs, recall (`matches / spud_count`), and precision (`matches / ours_count`).
The convention for an empty denominator is `1`; use the counts alongside rates
when either side is empty. Global and cell statistics aggregate per-form
matches; rows never match across forms or cells.

For pairs accepted at each threshold, position reports scalar distance error;
scale, rotation, and brightness report absolute component errors with mean,
maximum, and RMS. Rotation error uses nine matrix entries, **not** angular
degrees; scale and brightness are dimensionless. Aggregates weight means and
RMS by their component counts. Missing matched components produce blank CSV
values / JSON `null`, rather than a fabricated zero error.

`full` includes every pre-obstruction candidate. `unobstructed` removes only
our flagged candidates and compares the retained production set with spud's
unmasked reference. Spud has no corresponding obstruction mask. Report these
views separately: obstruction differences are expected and the mask is retained.

`uncapped` is the primary measurement. `capped` is a diagnostic counterfactual:
apply the selected mask, sort valid candidates by generation ordinal, then
retain the first 49,152 per cell across all forms. Spud is independently capped
in ordinal order. **Our production generator does not impose this record cap.**
The diagnostic does not alter either generator or claim to reproduce downstream
runtime filtering. If no cell reaches the cap, capped and uncapped results are
identical. Skipped malformed rows also make a capped comparison incomplete.

## Outputs and regression checks

- `COMPARE.md`: all representation/mask/cap totals and cells ranked by exact
  unmatched rows (`ours + spud - 2 * exact_pairs`) for source/full/uncapped.
- `summary.json`: totals, completeness/warnings, cell diagnostics, ranked cells,
  and small matched/unmatched ordinal samples for source/full/uncapped tracing.
- `per-cell.csv`, `per-cell-form.csv`, `per-form.csv`: counts, threshold recall
  and precision, and matched error statistics for every measurement view.
- `cell-diagnostics.csv`: candidate counts, obstruction removals, malformed-row
  counts, and records removed by each cap diagnostic.

Run synthetic tests without game fixtures:

```text
python -B -m unittest discover -s scripts/grass-parity -p "test_*.py" -v
```

The fixtures distinguish reused neighbours, XY-only matching, duplicate
positions, threshold boundaries, component-weighted aggregation, packing loss,
mask/cap ordering across forms, missing requested cells, malformed records, and
different decimal representations of the same binary32 value. Numeric parity
does not by itself establish visual parity with Skyrim.
