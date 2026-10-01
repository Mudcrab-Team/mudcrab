# OpenSkyrim Technical Specifications (`docs/specs/`)

Comprehensive technical architecture specifications, asset converter pipelines, rendering optimizations, and modding subsystem designs for **OpenSkyrim**.

---

## 📁 Specifications Directory Structure

```
docs/specs/
├── README.md                          # Specifications Directory Index
├── converters/                        # Offline Asset Transpilation Specs
│   ├── pipeline.md                    # Asset & Modernization Pipeline Overview
│   ├── esm-to-sqlite.md               # Master Database (SQLite 3, R-Tree, rkyv cache)
│   ├── nif-to-gltf.md                 # 3D Mesh Exporter (BSTriShape, skinning, PBR)
│   ├── dds-to-ktx2.md                 # Texture Transcoder (Basis Universal KTX2)
│   ├── pex-to-lua.md                  # Papyrus Bytecode to Luau Transpiler
│   └── data-formats.md                # Legacy Skyrim vs Modern Format Matrix
├── engine/                            # Bevy Engine Core & Rendering Specs
│   ├── architecture.md                # Workspace Architecture & Bevy ECS Core
│   ├── vercidium-optimizations.md     # Instanced Indirect Draw Pool & HZB Culling
│   └── bevy-examples.md               # Bevy 0.19 Pattern Mappings
├── modding/                           # Mod Manager, Scripting & UI Specs
│   ├── launcher.md                    # Setup Wizard & GUI Launcher Specifications
│   ├── mods-and-ui.md                 # Built-in Mod Manager & Flash-to-Bevy bsn! UI
│   └── features.md                    # Unlocked Features (AI Dialogue, Co-op, Zero Loading)
└── meta/                              # Domain & System Context
    ├── context.md                     # Domain Glossary & Canonical Definitions
    ├── platforms.md                   # Platform Compatibility Target Matrix
    └── requirements.md                # System Hardware Requirements Comparison
```

---

## 🛠️ Category Summary & Quick Links

### 🔄 1. Asset Converters (`converters/`)

Offline asset transpilation specs handled by the `converter` crate:

- 📄 **[`pipeline.md`](converters/pipeline.md)** — Master offline conversion architecture and pipeline matrix.
- 📄 **[`esm-to-sqlite.md`](converters/esm-to-sqlite.md)** — `Skyrim.esm` parsing strategy, libSQL sync, and `rkyv` zero-copy terrain cache.
- 📄 **[`db-schema.md`](converters/db-schema.md)** — Complete SQLite 3 database DDL schema (`skyrim_world.db`), table constraints, and indices.
- 📄 **[`nif-to-gltf.md`](converters/nif-to-gltf.md)** — Converting `.nif` geometry, material shaders, and skin weights to `glTF 2.0`.
- 📄 **[`dds-to-ktx2.md`](converters/dds-to-ktx2.md)** — DirectDraw surface transcoding to Basis Universal `KTX2`.
- 📄 **[`pex-to-lua.md`](converters/pex-to-lua.md)** — Papyrus `.pex` bytecode decompilation to sandboxed `Luau`.
- 📄 **[`data-formats.md`](converters/data-formats.md)** — Binary comparison matrix between legacy and modern asset formats.

### ⚙️ 2. Engine, Scripting & Rendering (`engine/`)

Core runtime specifications handled by the `engine` and `scripting` crates:

- 📄 **[`architecture.md`](engine/architecture.md)** — 4-crate Cargo workspace structure, Bevy ECS setup, and `crates/scripting` isolation.
- 📄 **[`vercidium-optimizations.md`](engine/vercidium-optimizations.md)** — GPU instanced indirect rendering (`DrawMeshInstancedIndirect`) and GPU culling.
- 📄 **[`bevy-examples.md`](engine/bevy-examples.md)** — Code examples mapping Bevy 0.19 patterns to engine subsystems.

### 🎮 3. Modding, UI & Launcher (`modding/`)

Scripting, type definitions, UI, and launcher specifications:

- 📄 **[`launcher.md`](modding/launcher.md)** — First-run setup wizard and client launcher.
- 📄 **[`mods-and-ui.md`](modding/mods-and-ui.md)** — Integrated mod manager and Flash `.gfx` to Bevy `bsn!` UI node transpilation.
- 📄 **[`features.md`](modding/features.md)** — Unlocked engine innovations (Ollama LLM dialogue, zero loading screens, native co-op).

### 📐 4. System & Domain Metadata (`meta/`)

Project domain glossary and system benchmarks:

- 📄 **[`context.md`](meta/context.md)** — OpenSkyrim canonical terms and domain glossary.
- 📄 **[`platforms.md`](meta/platforms.md)** — Cross-platform target matrix (Windows, Linux, macOS, Android ARM64, WASM).
- 📄 **[`requirements.md`](meta/requirements.md)** — Official Skyrim SE vs OpenSkyrim hardware system specs comparison.
