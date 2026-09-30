# Phase 3: Luau Scripting Runtime, UI & Modding Layer (`scripting` & `launcher`)

> **Goal:** Embed Luau execution in an isolated crate (`crates/scripting`), transpile legacy Flash UI to Bevy `bsn!`, generate `.d.lua` type definitions for modder DX, and create the launcher setup wizard.

---

## 🎯 Key Deliverables & Specifications

### 3.1 Embedded Luau Scripting Crate (`crates/scripting`)

- Dedicated `scripting` library crate (`mlua` + `luau-jit`) isolating script VM execution from game engine recompilations.
- Secure sandboxed execution environment with strict host system isolation.
- Engine API bindings (`Engine.Player`, `Engine.Weather`, `Engine.Cell`, `Engine.UI`).
- Live hot-reloading watching `.lua` script edits in real-time without restarting the client.

### 3.2 Modder Developer Experience (DX) & Type Safety

- Automated `.d.lua` EmmyLua / LuaLS type definitions generator (`openskyrim.d.lua`).
- **Luarocks** package distribution (`luarocks install openskyrim-types`) enabling autocompletion, inline docs, and static type checking in VS Code, Zed, Neovim, and Cursor.

### 3.3 Async Web API Integration

- Non-blocking HTTP (`reqwest`/`tokio`) and WebSocket interfaces for Luau scripts.
- Live web mod capabilities (e.g. real-world local weather synchronization, online leaderboards).

### 3.4 Native Declarative UI Engine (`bsn!`)

- Transpilation pipeline for legacy Flash `.gfx` UI layouts to Bevy 0.19 `bsn!` (Bevy Scene Notation) macro nodes.
- ActionScript 2.0 ➔ Luau UI event handlers running on the unified Luau engine.
- Hardware-accelerated HUD, Inventory, Magic, Dialogue, and Map menus.

### 3.4 OpenSkyrim Launcher & Mod Manager (`launcher`)

- GUI setup wizard: Auto-detect Skyrim SE installation path on system.
- Built-in Mod Manager: Drag-and-drop `.esp`/`.lua` mod archives, priority order load configuration, and automated invocation of `converter`.
