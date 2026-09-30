# OpenSkyrim Architecture & Roadmap

An open-source engine reimplementation for **The Elder Scrolls V: Skyrim (Special Edition)** (and extensible for Creation Engine / Gamebryo titles like Morrowind / Oblivion) built in **Rust** using the **Bevy Engine**.

Inspired by project initiatives like OpenMW (OpenMorrowind), OpenSkyrim aims to provide a modern, multithreaded, high-performance runtime for Bethesda engine assets.

---

## 1. Core Architecture Overview

```
                          ┌───────────────────────────┐
                          │   Original Skyrim Assets  │
                          │ (.esm, .esp, .bsa, .nif)  │
                          └─────────────┬─────────────┘
                                        │
                                        ▼ (Offline Transpilation Pipeline)
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Modern Converted Asset Storage                           │
│  ┌──────────────────────────┬──────────────────────┬─────────────────────┐  │
│  │   glTF 2.0 (.glb)        │   KTX2 Textures      │   SQLite 3 Database │  │
│  │   (mesh_tools Exporter)  │   (basis-universal)  │   (R-Tree + rkyv)   │  │
│  └──────────────────────────┴──────────────────────┴─────────────────────┘  │
└────────────────────────────────────────┬────────────────────────────────────┘
                                         │ (Instant Memory Mapped / Multi-threaded Streaming)
                                         ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    OpenSkyrim Bevy Engine Runtime (ECS)                     │
│  ┌──────────────────────────┬──────────────────────┬─────────────────────┐  │
│  │   Vercidium Instanced    │   WebGPU / Vulkan    │   Luau (mlua) JIT   │  │
│  │   Indirect Render Pool   │   Bevy 0.19 bsn! UI  │  (Web API + Sandbox)│  │
│  └──────────────────────────┴──────────────────────┴─────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Workspace Crate Architecture

During Phase 2, OpenSkyrim is organized into a **4-crate Cargo Workspace**. Phase 3 adds the isolated `scripting` crate as a fifth member:

```
OpenSkyrim/
├── Cargo.toml                  # Workspace Root Manifest
├── crates/
│   ├── launcher/    # UI App (Setup Wizard, Mod Manager, Launcher)
│   ├── converter/   # Converter Pipeline (.nif ➔ .glb, .dds ➔ KTX2, .esm ➔ libSQL, .pex ➔ Luau)
│   ├── shared/      # Versioned database/cache contracts shared by converter and engine
│   └── engine/      # Bevy Game Engine Runtime (Render Pipeline, Physics, Audio, ECS Systems)
```

| Crate Name       | Binary / Crate Type | Key Responsibilities                                                                                                            |
| :--------------- | :------------------ | :------------------------------------------------------------------------------------------------------------------------------ |
| **`launcher`**   | GUI Executable      | First-run setup wizard, game path detection, built-in mod manager UI, triggers converter progress bar, and launches the engine. |
| **`converter`**  | Library & CLI       | Heavy transformation logic (`mesh_tools`, `basis-universal`, `ddsfile`, `nom` parsers). Invoked directly by `launcher`.         |
| **`shared`**     | Library             | Stable `rkyv` cache structures, schema versions, and converter/runtime data contracts. |
| **`engine`**     | Game Executable     | Light, hyper-fast game binary (Bevy 0.19+, `wgpu`, `libsql`). Orchestrates transformed game assets and Bevy ECS systems at 60+ FPS.     |

---

## 3. Modder Developer Experience (DX) & Type Safety

OpenSkyrim prioritizes a modern, developer-friendly modding ecosystem:

- **Isolated Scripting Crate (`crates/scripting`):** Prevents script runtime changes from forcing full game engine recompilations.
- **Phase boundary:** `crates/scripting` is introduced in Phase 3; the Phase 2 Papyrus compatibility module remains inside `engine` until that migration.
- **Type Definitions (`.d.lua` / EmmyLua / LuaLS):** Generates static type definitions for IDEs (VS Code, Zed, Neovim, Cursor), providing instant autocompletion, inline API docs, and static type checking.
- **Luarocks Package Distribution:** Enables distribution of type defs and community mod libraries via standard Lua package management tools.

---

## 4. Technology Stack & Tools

- **Language:** Rust (2024 Edition)
- **Engine Framework:** [Bevy](https://bevyengine.org/) (v0.19+)
- **Graphics Backend:** `wgpu` (Vulkan / Metal / DirectX 12 / WebGPU)
- **Scripting Engine:** Dedicated `scripting` crate (Luau via `mlua` crate with `luau-jit` feature)
- **Modder Tooling:** EmmyLua / LuaLS `.d.lua` type definitions + Luarocks package manager
- **3D Exporter:** `mesh_tools` (`GltfBuilder`)
- **Texture Encoder:** `basis-universal` + `ddsfile`
- **Database & Cache:** SQLite 3 (`rusqlite` + `rtree`) + `rkyv` zero-copy memory mapping
- **UI Engine:** Bevy 0.19 `bsn!` (Bevy Scene Notation) declarative macro trees
