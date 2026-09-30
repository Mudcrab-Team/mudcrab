# OpenSkyrim Modding Architecture & Compatibility Strategy

Skyrim has the largest modding ecosystem in PC gaming. This document outlines how OpenSkyrim handles legacy Skyrim mods (`.esp`, `.esm`, `.esl`, `.bsa`, mesh/texture overrides, Papyrus scripts) while introducing modern, native Lua modding.

---

## 1. Dual-Tier Modding Architecture

```
                               ┌───────────────────────────────┐
                               │     Modder / User Setup       │
                               │  (VORTEX / MO2 / Data Dir)    │
                               └───────────────┬───────────────┘
                                               │
                                               ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    OpenSkyrim Mod Ingestion & Sandbox                       │
│                                                                             │
│   ┌─────────────────────────────────┐   ┌───────────────────────────────┐   │
│   │  Tier 1: Legacy Compatibility   │   │  Tier 2: Native Modern Mods   │   │
│   │  - Unpack `.esp`/`.esl` to DB   │   │  - Direct glTF / KTX2 Assets  │   │
│   │  - On-the-fly .nif/.dds convert │   │  - Native Luau Scripts        │   │
│   │  - Papyrus > Lua Transpilation  │   │  - Hot-reloading & Sandboxing │   │
│   └─────────────────────────────────┘   └───────────────────────────────┘   │
└──────────────────────────────────────────────┬──────────────────────────────┘
                                               │
                                               ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Unified OpenSkyrim Engine Runtime                        │
│                 (Virtual Filesystem + SQLite Mod Layering)                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Handling Legacy Skyrim Mods (`.esp`, `.esl`, `.bsa`, `.nif`, `.dds`)

### A. Mod Plugin Load Order & Database Layering (`.esp` / `.esl`)

In original Skyrim, `.esp` files override records from `Skyrim.esm` based on load order (e.g. `plugins.txt`).

- **SQLite Mod Layering:**
  - OpenSkyrim stores base game data in `skyrim_world.db`.
  - When a user adds an `.esp` mod, the converter imports the plugin's records into a mod table with a higher **load order priority weight**.
  - Database query:
    ```sql
    SELECT * FROM refr_objects WHERE form_id = ? ORDER BY load_order DESC LIMIT 1;
    ```
  - **Benefit:** Instant record resolution with zero engine hacking required!

### B. Dynamic On-the-Fly Asset Conversion (Loose Files & BSA Mods)

- Mods containing loose `.nif` or `.dds` files are detected by the Virtual Filesystem (VFS).
- The converter transpiles mod `.nif` files to `.glb` and `.dds` textures to `.ktx2` in a local cache directory (`mod_cache/`).

### C. Legacy Papyrus Scripts (`.pex` Mods)

- `.pex` bytecode contained within mod `.bsa` archives or `Scripts/` folders is run through the **Papyrus-to-Lua AST Transpiler**.
- Mod functions call the unified OpenSkyrim Lua API bindings seamlessly.

## 3. Integrated Launcher Mod Manager & Storage Structure

Instead of relying solely on external tools, OpenSkyrim includes a **Built-in Mod Manager** inside the Launcher.

### Storage Hierarchy (`OpenSkyrim/`)

```
OpenSkyrim/
├── game_data/               # Unpacked base game & DLC assets
├── transformed_data/        # Transformed base game (glTF, KTX2, SQLite)
├── mods/                    # User installed raw mod folders (installed via Launcher)
│   ├── ModA_HighResArmor/
│   └── ModB_CustomQuest/
├── transformed_mods/        # Transformed mod cache (glTF, KTX2, Lua)
└── config.json   # Mod load order & active plugin list
```

### Built-in Mod Manager Features:

- **Drag-and-Drop Installation:** Drop `.zip` / `.7z` / `.rar` mod archives directly into the Launcher.
- **Visual Load Order Manager:** Drag and drop `.esp` / `.esl` load order priorities with conflict detection.
- **One-Click Transformation:** Clicking "Enable Mod" automatically runs the asset converter for that mod into `transformed_mods/`.

---

## 4. UI Modernization (Flash / Scaleform `.gfx` ➔ Bevy Native `bsn!` UI)

### The Skyrim UI Problem:

Original Skyrim uses **Autodesk Scaleform**, which executes Adobe Flash (`.swf` / `.gfx` files with ActionScript 2.0). Flash is obsolete, insecure, and heavily limits UI customization and performance.

### The OpenSkyrim Native Bevy UI Architecture (`bsn!`):

OpenSkyrim completely eliminates webviews, embedded browser engines, and Flash runtimes. All UI elements (hud, menus, inventory, dialogue choices) are transpiled directly into native **Bevy 0.19 `bsn!` (Bevy Scene Notation)** declarative widget trees for pure zero-overhead GPU execution.

```
┌───────────────────────────┐
│ Original Skyrim Flash UI  │
│  (.swf / .gfx Scaleform)  │
└─────────────┬─────────────┘
              │
              ▼ (Offline Flash/Scaleform Transpiler)
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Bevy 0.19 `bsn!` Declarative UI Tree                     │
│  ┌──────────────────────────┬──────────────────────┬─────────────────────┐  │
│  │   `bsn!` Node Structure  │   Bevy UI Styles     │   Luau UI Handlers  │  │
│  │   (Native Bevy Widgets)  │   (Color / Typography)│   (ActionScript > Luau) │  │
│  └──────────────────────────┴──────────────────────┴─────────────────────┘  │
└─────────────────────────────┬───────────────────────────────────────────────┘
                              │
                              ▼ (Pure GPU Native Execution)
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Bevy Engine Render Pipeline                         │
│                                                                             │
│   Native Bevy UI System (Zero Webview / Zero Embedded Engine Overhead)      │
│   - Direct Bevy ECS Query & Mutator System Integration                      │
│   - Sub-millisecond rendering & frame-rate uncapped GPU drawing             │
│                                                                             │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │   Bevy ECS Bridge: UI Events < Direct ECS Events > Luau / Engine    │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

#### How it works step-by-step:

1. **Flash Extraction & AST Transpilation (Offline Phase):**
   - **Flash Layouts ➔ `bsn!` Widgets:** Vector shapes, buttons, text fields, and container layouts inside `.swf`/`.gfx` are exported (via AST extraction in `openskyrim_converter`) and transpiled into native Bevy UI nodes using `bsn!` declarative syntax.
   - **ActionScript 2.0 ➔ Unified Luau Handlers:** ActionScript UI scripts (e.g. inventory filtering, stats calculations, menu navigation) are transpiled into standard **Luau** functions. OpenSkyrim uses Luau as the **single, unified scripting engine** for both UI handlers and quest/game logic—eliminating secondary UI scripting runtimes.

2. **Rendering & Execution inside Bevy (Runtime Phase):**
   - **Bevy 0.19 `bsn!` Native UI:**
     - UI layout structures load directly into native **Bevy 0.19 `bsn!` widget trees**.
     - `bsn!` provides clean, patchable UI widget trees with automatic asset dependency management directly in Rust/Bevy without requiring any browser runtimes, webview overhead, or HTML rendering libraries.

3. **Direct ECS Event Integration (UI ↔ Bevy ECS ↔ Luau Engine):**
   - Interacting with UI widgets triggers standard Bevy ECS events directly in Rust.
   - Game state updates (e.g. player health changes, item acquisitions) instantly mutate Bevy UI components without IPC serialization overhead.

---

### Code Architecture Example: Transpiling Flash UI to `bsn!`

#### Original ActionScript 2.0 Event (inside Skyrim's `InventoryMenu.gfx`):
```actionscript
on (release) {
    _root.EquipItem(this.itemID);
    this.gotoAndStop("Equipped");
}
```

#### Transpiled Unified Luau UI Handler (`modern_assets/scripts/ui/inventory.lua`):
```lua
local InventoryUI = {}

function InventoryUI:onItemSlotReleased(slot)
    Engine.Player:equipItem(slot.itemID)
    slot:setState("Equipped")
end

return InventoryUI
```

#### Generated Bevy 0.19 `bsn!` Native Rust Component (`src/ui/inventory_menu.rs`):
```rust
use bevy::prelude::*;

// Transpiled from InventoryMenu.gfx to Bevy 0.19 bsn!
pub fn spawn_inventory_menu(mut commands: Commands, assets: Res<AssetServer>) {
    commands.spawn(bsn! {
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        } [
            // Left Side: Inventory Item List Container
            Node {
                width: Val::Px(400.0),
                flex_direction: FlexDirection::Column,
                background_color: Color::srgb(0.05, 0.05, 0.05).into(),
                ..default()
            } [
                Text::new("INVENTORY"),
                ButtonNode {
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                } [
                    Text::new("Iron Longsword"),
                ]
            ],
            // Right Side: Item 3D Preview Frame
            Node {
                width: Val::Px(500.0),
                height: Val::Px(500.0),
                ..default()
            }
        ]
    });
}
```
