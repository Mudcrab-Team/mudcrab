# Papyrus (.pex/.psc) to Luau Transpilation Specification

This document details the technical specification for transpiling Bethesda Papyrus compiled bytecode (`.pex`) and source scripts (`.psc`) into modern, high-performance **Luau** scripts for OpenSkyrim.

---

## 1. Overview & Objectives

- **Input:** Skyrim `.pex` bytecode (extracted from BSA archives or loose script files) or `.psc` source code.
- **Output:** Clean **Luau** script files (`.lua`).
- **Goal:**
  1. Replace Skyrim's slow, single-threaded Papyrus Virtual Machine with the lightning-fast Luau VM.
  2. Map Papyrus language features (Events, States, Properties, Native functions, Async Waits) into idiomatic Luau tables and coroutines.

### Selecting the Optimal Lua Engine in `mlua`:

The `mlua` crate supports multiple Lua backends via Cargo feature flags:

| `mlua` Feature Flag      | Backend Engine           | Performance & Suitability for OpenSkyrim                                                                                                                                                                         |
| :----------------------- | :----------------------- | :--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **`luau` / `luau-jit`**  | **Luau (Roblox)**        | ⭐ **Mandatory Engine for OpenSkyrim.** Built specifically for AAA game engines. Has a JIT compiler, built-in sandboxing (prevents malicious mod scripts), optional static typing, and ultra-fast C/Rust FFI bridges. |
| **`luajit`**             | **LuaJIT 2.1**           | 🔥 **Fast raw compute execution.** Ideal for complex math, but lacks Luau's modern sandboxing and static typing features.                                                                                      |
| **`lua54` / `vendored`** | **Standard PUC Lua 5.4** | Highly portable & compliant interpreter, but lacks JIT compilation (5x-7x slower than LuaJIT/Luau) and sandboxing.                                                                                              |

---

## 2. Papyrus Bytecode to Luau Concept Mapping

| Papyrus Concept         | Papyrus Syntax / Opcode                | Transpiled Luau Code                                  |
| :---------------------- | :------------------------------------- | :---------------------------------------------------- |
| **Script Class**        | `Scriptname QuestScript extends Quest` | `local QuestScript = Class("QuestScript", Quest)`     |
| **Properties**          | `Actor Property PlayerRef auto`        | `self.PlayerRef = Engine.getForm(0x00000014)`         |
| **Event Handlers**      | `Event OnInit() ... EndEvent`          | `function QuestScript:onInit() ... end`               |
| **States**              | `State Active ... EndState`            | Luau state table switching (`self:setState("Active")`) |
| **Async Wait**          | `Utility.Wait(2.5)`                    | Coroutine yield timer: `task.wait(2.5)`               |
| **Global Native Calls** | `Debug.Notification("Hello")`          | `Engine.Debug.notification("Hello")`                  |
| **Function Calls**      | `TargetRef.Enable()`                   | `self.TargetRef:enable()`                             |

---

## 3. Transpilation Workflow Architecture

```
┌───────────────────────────────┐
│ Skyrim Papyrus Script (.pex)  │
└───────────────┬───────────────┘
                │
                ▼ (nom Binary Parser / Champollion Decompiler)
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Decompile & Parse Abstract Syntax Tree (AST)                             │
│    - Read Header (Magic `0xFAEA4CB5`, Major/Minor Version)                  │
│    - Parse String Table, Structs, Variables, Properties, Functions, States  │
│    - Construct Papyrus AST (Abstract Syntax Tree) Nodes                     │
└───────────────┬─────────────────────────────────────────────────────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 2. AST Transformation & Code Generator (Rust Compiler Pass)                 │
┌─────────────────────────────────────────────────────────────────────────────┐
│ 3. Output Transpiled Luau File (.lua)                                       │
│    - Save to `transformed_data/scripts/` ready for `mlua` runtime execution │
└───────────────┴─────────────────────────────────────────────────────────────┘
```

---

## 4. Complete Code Transpilation Example

### Original Skyrim Papyrus Source (`QF_MQ101_0003372B.psc`):

```papyrus
Scriptname QF_MQ101_0003372B extends Quest

ReferenceAlias Property Alias_Ralof auto
ReferenceAlias Property Alias_Hadvar auto

Event OnInit()
    Debug.Notification("Helgen Execution Quest Started!")
    Utility.Wait(1.5)
    Alias_Ralof.GetReference().Enable()
EndEvent

Auto State Waiting
    Event OnTouch(ObjectReference akActionRef)
        GoToState("Active")
    EndEvent
EndState

State Active
    Event OnTouch(ObjectReference akActionRef)
        ; Do nothing
    EndEvent
EndState
```

### Transpiled OpenSkyrim Luau Output (`QF_MQ101_0003372B.lua`):

```lua
local QF_MQ101_0003372B = Class("QF_MQ101_0003372B", Quest)

function QF_MQ101_0003372B:initProperties()
    self.Alias_Ralof = self:getAlias("Alias_Ralof")
    self.Alias_Hadvar = self:getAlias("Alias_Hadvar")
end

function QF_MQ101_0003372B:onInit()
    Engine.Debug.notification("Helgen Execution Quest Started!")
    coroutine.yield(1.5) -- Async delay without blocking CPU thread
    local ralof_ref = self.Alias_Ralof:getReference()
    if ralof_ref then
        ralof_ref:enable()
    end
end

-- State Machine Implementation
QF_MQ101_0003372B.States = {
    Waiting = {
        onTouch = function(self, akActionRef)
            self:setState("Active")
        end
    },
    Active = {
        onTouch = function(self, akActionRef)
            -- Do nothing
        end
    }
}

return QF_MQ101_0003372B
```

---

## 5. Runtime Architecture (`crates/scripting`)

Inside OpenSkyrim, the **`scripting` crate (`crates/scripting`)** encapsulates the `mlua` JIT VM, isolates script execution from main engine recompilations, and generates IDE type annotations:

```rust
// Inside crates/scripting/src/engine.rs
use mlua::{Lua, Result};
use bevy::prelude::*;

pub struct LuauScriptEngine {
    lua: Lua,
}

impl LuauScriptEngine {
    pub fn new() -> Result<Self> {
        let lua = Lua::new();

        // Register Native Rust Engine API to Luau namespace
        let globals = lua.globals();
        let engine_table = lua.create_table()?;

        // Bind Engine.Debug.notification
        engine_table.set("notification", lua.create_function(|_, msg: String| {
            println!("[Skyrim Debug]: {}", msg);
            Ok(())
        })?)?;

        globals.set("Engine", engine_table)?;
        Ok(Self { lua })
    }

    pub fn execute_script(&self, script_code: &str) -> Result<()> {
        self.lua.load(script_code).exec()
    }
}
```

### 6. Modder Intellisense & Type Safety (`.d.lua` Generator)

`crates/scripting` includes an automated type definition exporter (`openskyrim_scripting::generate_typedefs()`):

- Exposes all `Engine.*` API methods as EmmyLua / LuaLS type definitions (`openskyrim.d.lua`).
- Distributable via **Luarocks** for one-command mod developer setup (`luarocks install openskyrim-types`).
- Gives mod developers instant autocompletion, inline documentation, and static type checking in VS Code, Zed, Neovim, and Cursor.
