# Asset & Data Modernization Pipeline Strategy

This document outlines the conversion pipeline to ingest legacy Skyrim formats (`.bsa`, `.dds`, `.nif`, `.hkx`, `.esm`) and output modern asset standards suitable for modern web & native runtimes (Bevy Engine, glTF 2.0, KTX2, WebGPU).

---

## 1. Pipeline Overview Diagram

```
┌───────────────────────────────┐
│     Legacy Skyrim Data        │
│  (.bsa, .dds, .nif, .hkx)     │
└───────────────┬───────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                       OpenSkyrim Converter Pipeline                         │
│                                                                             │
│   ┌────────────────-┐     ┌────────────────┐     ┌──────────────────────┐   │
│   │ Archive Unpacker│ ──► │ Mesh & Texture │ ──► │ Record / World Data  │   │
│   │ (BSA / BA2)     │     │ Converter      │     │ Serializer           │   │
│   └────────────────-┘     └────────────────┘     └──────────────────────┘   │
└───────────────────────────────┬─────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                     Modern Asset Output Formats                             │
│  ┌──────────────────────────┬──────────────────────┬─────────────────────┐  │
│  │   glTF 2.0 / glb         │   KTX2 / Basis       │   JSON / BSON / Ron │  │
│  │   (Meshes, PBR, Skeleton)│   (GPU Textures)     │(Cell & Scene Graphs)│  │
│  └──────────────────────────┴──────────────────────┴─────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Format Conversion Matrix

| Legacy Format                 | Target Modern Format                         | Purpose                                                            | Conversion Tool / Library              |
| :---------------------------- | :------------------------------------------- | :----------------------------------------------------------------- | :------------------------------------- |
| **`.bsa` / `.ba2`**           | Directory Hierarchy / VFS                    | Extracted compressed virtual filesystem                            | Rust (`flate2`, `lz4_flex`)            |
| **`.nif`**                    | **`glTF 2.0` (`.glb`)**                      | Standard 3D geometry, mesh hierarchies, PBR materials, skinning    | `nif-parser` ➔ `gltf` crate            |
| **`.dds` (BC1-BC7)**          | **`KTX2` / Supercompressed Basis Universal** | Fast GPU texture streaming & compressed VRAM footprints            | `image` crate / `basis-universal`      |
| **`.hkx` (Havok)**            | **glTF Animations & Rapier 3D Colliders**    | Skeletal animation clips & physics collision meshes                | Havok XML exporter / custom parser     |
| **`.esm` / `.esp`**           | **`SQLite3` / Binary (Bincode / rkyv)**      | High-performance spatial indexing, memory-mapped zero-copy queries | `rusqlite` / `rkyv` / `zerocopy`       |
| **`.pex` / `.psc` (Papyrus)** | **`Luau` Scripts**                           | High-performance, sandboxed scripting engine                       | `mlua` / Papyrus-to-Luau AST Transpiler |

---

## 3. Detailed Conversion Modules

### A. Mesh Converter (`.nif` ➔ `.gltf` / `.glb`)

- **Geometry Conversion:**
  - Extract `BSTriShape` vertex buffers (Positions, Normals, UVs, Tangents).
  - Combine multi-part meshes into single glTF primitives.
- **Material Mapping:**
  - Map Skyrim `BSLightingShaderProperty` texture slots:
    - Diffuse Map ➔ glTF `baseColorTexture`
    - Normal Map ➔ glTF `normalTexture`
    - Specular Map / Environment ➔ glTF `metallicRoughnessTexture`
- **Skinning & Rigging:**
  - Map `NiSkinInstance` & `NiSkinData` bone weights directly to glTF `skins` and `joints`.

### B. Texture Transcoder (`.dds` ➔ `.ktx2`)

- Converts DirectDraw Surface files into supercompressed **Basis Universal KTX2** textures.
- Derives sRGB, linear-normal, or linear-data encoding from the texture slot recorded by the
  published GLB/world database; filenames and extensions do not determine color space.
- Preserves source alpha and authored mip levels, validates the published KTX2 metadata/hash, and
  aborts the transactional publication when any required texture fails.
- Asset paths use one lowercase canonical key. Archive overlays follow plugin load order, loose
  files have highest priority, and normalized collisions inside one source layer are fatal.
- Supports runtime transcoding into WebGPU/Vulkan native compressed formats (BC1/BC7 for Desktop, ASTC/ETC2 for Mobile/Web).

### C. World & Scene Database (`.esm` ➔ `SQLite3` + `rkyv` Zero-Copy Cache)

- **SQLite 3 Database (`skyrim_world.db`):**
  - **Spatial R-Tree Indexing (`rtree` module):** Query cell objects instantly using 3D Bounding Boxes (`X, Y, Z` coordinates) based on player camera position.
  - **Relational FormID Lookup:** Fast `O(1)` index table mapping 32-bit `FormID` ➔ Record payload.
  - **Tables:** `cells` (Grid coords, lighting, worldspace), `references` (Placed glTF model URIs, transforms, flags), `npcs` (Stats, race, dialogue trees).

- **Zero-Copy Hot Storage (`rkyv` / `zerocopy`):**
  - Frequently requested terrain meshes and cell data can be serialized into binary files mapped directly into RAM with `mmap`.
  - Eliminates deserialization CPU overhead during fast player movement across Skyrim's terrain.

### D. Scripting Engine (`.pex` Papyrus Bytecode ➔ `Luau` Scripts)

- **Papyrus Decompiler / Transpiler:**
  - Parse `.pex` binary bytecode or decompiled `.psc` source code into an Abstract Syntax Tree (AST).
  - Translate Papyrus features (Events, States, Properties, Native functions) directly to Luau equivalent tables and functions.
- **Luau Runtime Integration (`mlua` crate):**
  - Luau provides extreme execution speed (near C/Rust speed with JIT or optimized bytecode) and native sandboxing.
  - Expose game engine API bindings (e.g. `Game.getPlayer()`, `Actor.addItem()`, `ObjectReference.enable()`) to Luau via `mlua`.
- **State Preservation & Save Games:**
  - Modern Luau state serialization enables light, fast save-game state snapshots without Papyrus VM thread corruption.

---

## 4. Unified Async Pipeline Orchestration (`tokio`)

The converter pipeline is orchestrated asynchronously using **`tokio`** task concurrency and channels (`tokio::sync::mpsc`):

- **Non-blocking Concurrency:** Heavy I/O decompression and file transformations execute concurrently using `tokio::spawn` and `tokio::task::spawn_blocking` for CPU-bound transcode tasks (`dds` ➔ `ktx2` and `nif` ➔ `glb`).
- **Async Progress Reporting:** Sends `ProgressEvent` updates across a `tokio::sync::mpsc::Sender` to the launcher UI or CLI without thread blocking.
- **Unified Interface:** Exposes `AssetPipeline::run_async(config, progress_tx).await` as the single high-leverage entry point for modernizing game assets, and `run_async_with_cancel(config, progress_tx, cancellation)` for a caller that can stop it (the command-line converter's Ctrl+C handler).

### Checking a converted output

`converter check <output directory> [--full]` answers "is this converted folder still good?" without
converting anything. It reads `conversion-manifest.json` in the folder and compares every artifact
it lists with what is on disk:

- **Quick** (the default, seconds): each artifact exists and has the recorded size. Only file
  metadata is read.
- **`--full`**: also re-hashes each artifact (SHA-256, in parallel) against the recorded hash.

It also reports a manifest written by another converter schema, a conversion not marked complete,
recorded input failures, and a missing `skyrim_world.db`. It prints `All good` or the problems (up to
20 lines, then a count), and exits `0` when all is good, `1` when problems were found and `2` when the
manifest is missing or unreadable. The check never writes to the folder, and a manifest path that is
absolute or leaves the folder (`..`) is reported instead of read.

A damaged artifact does not need a full reconversion: running the converter again on the same `Data`
folder and output re-hashes each published artifact before reusing it, so only the missing or
changed files are converted again. The library entry point is `converter::check_output`, with a
progress callback for front ends. `converter::check_output_with_cancel` takes the same arguments and
an `&AtomicBool` stop flag: once the flag is set no new artifact is started, the call returns after
the reads already in flight, and the result is an `Err` holding `converter::CheckCancelled` (test it
with `error.is::<CheckCancelled>()`), never a partial report.

---

## 5. Runtime Pack vs Build Workspace

Staging is a build workspace; the published output is a runtime pack. They
are not the same directory.

- Extraction writes originals to `staging/vfs/`. Archive-ingestion blobs
  persist to `<output>.assets-cache/.ingestion-cache/` outside the pack
  (overridable with `PipelineConfig::cache_dir`). Neither ships.
- Conversion writes runtime artifacts (`textures/`, `meshes/`, `scripts/`,
  `skyrim_world.db`, `cell_cache.rkyv`, `integration-report.json`,
  `conversion-manifest.json`) inside staging.
- Publication copies only `report.artifacts` plus the manifest into the
  output directory, then removes non-resumed staging. Hard links are
  preferred with a copy fallback; size audits must deduplicate inodes.
- Resume staging keeps the same layout; retained GLB invalidation still
  excludes `vfs/`.

## 6. Asset Layout & Data Integrity Invariants

1. **VFS Path Normalization (`strip_leading_kind`):**
   - BSA archives and loose mod files use mixed-case conventions (`Textures/`, `Meshes/`, `Scripts/`).
   - The asset pipeline normalizes the leading folder component case-insensitively to ensure flat target mappings (`textures/`, `meshes/`, `scripts/`) without double-nested subfolders (e.g. preventing `textures/Textures/...`).
2. **ESM FormID Remapping Isolation (`is_form_id_subrecord`):**
   - Subrecord remapping during multi-plugin merges is record-type and payload-length aware (`len == 4`).
   - Prevents unintended integer remapping on text strings (`TES4` `CNAM`/`SNAM`), physics parameters (`TREE` `CNAM`), and RGBA color structs (`CLFM`/`AACT`).
3. **Strict Little-Endian ESM Binary Parsing:**
   - All Bethesda ESM multi-byte numeric primitives (integers, floats, FormIDs, and subrecord payloads such as `ACHR` `PDTO`) are parsed as little-endian bytes (`from_le_bytes`).
4. **One File per Extracted Entry (`link_or_copy`):**
   - In the staging workspace, every extracted archive entry is stored twice, as `vfs/<path>` and as the content-addressed blob in `.ingestion-cache/sha256/<xx>/<hash>`, and on a fresh install and on a cache hit alike those two names are a hard link on one file rather than two copies (a cross-volume or linkless filesystem falls back to a copy). Neither ships in the published runtime pack (§5).
   - Every writer into `vfs/` or the cache replaces the path (unlink, then write or copy) instead of writing through it, so a write under one name never changes the other.
   - The exception is a loose-asset override: `overlay_loose_assets` replaces the `vfs` entry with the loose file's own bytes, so that path is no longer a link to the archive entry's blob.
   - Resume and re-conversion writers follow the same replace-not-write rule: pack publication links staging files into the output, so a resumed run must unlink before rewriting any staged artifact it shares with a previous pack.

---

## 7. Command-Line Progress, Interruption and Resuming

`converter <Data> [output]` writes progress to **stderr** and the final summary to stdout, so a
pipeline can keep the outcome while the status goes to the terminal.

### Progress events

`ProgressEvent` (`crates/converter/src/progress.rs`) carries what a status line or a GUI needs:

| Field | Meaning |
| :--- | :--- |
| `stage` | `Discovering`, `Extracting`, `Database`, `Meshes`, `Textures`, `Scripts`, `Validating`, `Publishing`, `Complete` |
| `completed`, `total` | Items done and expected for the stage; `fraction()` is `completed / total` |
| `current_file` | The asset or archive in flight |
| `bytes_completed`, `bytes_total` | Bytes done and expected, where the stage knows them cheaply: an archive's file table, or the source sizes a conversion batch sums before it starts |
| `stage_fraction` | The stage's completion when the counts understate it (extraction counts archives but works file by file) |
| `overall` | Whole-run completion in `0.0..=1.0`, from the per-stage weights in `STAGE_WEIGHTS` |

The weights are the measured shares of a fresh Skyrim SE conversion (242,969 assets in 5 h 7 m:
textures about 4 h 30 m of it, extraction 24.2 GB written, meshes about 3 minutes, validation
re-reading every artifact), which is what makes `overall` a time-based bar rather than a
stage-count one. A reconversion that reuses most outputs moves through the early stages faster
than the weights assume, so the bar runs ahead of the wall clock; that is expected.

Extraction reports every 512 files (its file table gives both the count and the bytes), each
conversion batch reports every asset with its source size, and validation reports per artifact.

### The status line

On a terminal, the converter keeps one line on screen and redraws it at most four times a second —
on every batch of progress events and on a 250 ms timer, so the elapsed time and the estimate keep
moving while one large asset is being converted — printing a finished line when the stage changes:

```
Textures     61%  [overall  72%]  412 items/s  61.2 MB/s  00:12:31 elapsed  ~00:04:50 left
```

`~… left` is an EWMA estimate over `overall` and appears only once the rate has settled (three
samples and five seconds). The shown `overall` never moves backwards, even when a stage finishes
short of its total. A redraw puts the cursor back at the start of the row and pads the line with
spaces over the one it replaces: no escape sequences, which legacy Windows consoles print
literally.

When stderr is not a terminal (a log file, CI), one plain line per stage is printed every few
seconds, and on every stage change, each stamped with the run's elapsed time. `--verbose` restores
the old behaviour: one line per converted asset.

A warning about one asset (`ProgressEvent::notice`, sent when a dangling texture reference is
pruned) is printed as its own line: on a terminal the renderer ends the open status line first, so
a warning never splices into a redrawn line.

### The summary

A finished run prints the converted, reused and failed counts, the total time, the size of the
converted artifacts, the manifest and `--report-json` paths, and when each stage ran (first event
to last, so overlapping stages are still readable). An incomplete run names the first few skipped
inputs and exits non-zero.

### Interruption and failure

The command-line converter installs a Ctrl+C handler (`tokio::signal::ctrl_c`). The first Ctrl+C
sets the pipeline's cancellation flag: the asset in flight finishes, no new asset is started, and
the run stops with the staging folder kept. The flag is checked between archives, between stages,
in front of every asset and once more after the last stage, so an interrupt that lands while the
run is packing up still stops it before the publish rename. A second Ctrl+C exits immediately.

**A failed run keeps its staging folder too.** The folder is everything the run has done, and
either way the converter prints what went wrong, the first few assets that failed, where the folder
is, and the exact command that resumes from it:

```
Conversion failed after 0:12:31 during Textures: failed to convert textures\rock.dds: ...
  assets that failed (first 1):
    - textures\rock.dds
  The staging folder was kept: <output>.staging-<pid>-<stamp>
  Resume where it stopped with:
    converter "<Data>" "<output>" --resume-staging "<output>.staging-<pid>-<stamp>"
  Delete that folder to free the space if you would rather start over: <output>.staging-<pid>-<stamp>
```

`--resume-staging` accepts the folder and re-verifies the work already in it, so a stop costs the
asset in flight rather than the run. The manifest is written once, at the end of a run, so a
stopped run has recorded nothing as converted: the resume re-checks each staged file instead, and
deleting the folder only costs the work the next run has to redo.
