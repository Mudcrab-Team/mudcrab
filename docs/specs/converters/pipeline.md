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
- **Async Progress Reporting:** Sends `ProgressPhase` updates across `tokio::sync::mpsc::UnboundedSender` to the launcher UI or CLI without thread blocking.
- **Unified Interface:** Exposes `AssetPipeline::run_async(config, progress_tx).await` as the single high-leverage entry point for modernizing game assets.

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
progress callback for front ends.

---

## 5. Asset Layout & Data Integrity Invariants

1. **VFS Path Normalization (`strip_leading_kind`):**
   - BSA archives and loose mod files use mixed-case conventions (`Textures/`, `Meshes/`, `Scripts/`).
   - The asset pipeline normalizes the leading folder component case-insensitively to ensure flat target mappings (`textures/`, `meshes/`, `scripts/`) without double-nested subfolders (e.g. preventing `textures/Textures/...`).
2. **ESM FormID Remapping Isolation (`is_form_id_subrecord`):**
   - Subrecord remapping during multi-plugin merges is record-type and payload-length aware (`len == 4`).
   - Prevents unintended integer remapping on text strings (`TES4` `CNAM`/`SNAM`), physics parameters (`TREE` `CNAM`), and RGBA color structs (`CLFM`/`AACT`).
3. **Strict Little-Endian ESM Binary Parsing:**
   - All Bethesda ESM multi-byte numeric primitives (integers, floats, FormIDs, and subrecord payloads such as `ACHR` `PDTO`) are parsed as little-endian bytes (`from_le_bytes`).
