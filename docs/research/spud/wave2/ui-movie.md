# Wave 2 — UI movie conversion and host (rank 5)

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Wave 2 — UI movie conversion and host (rank 5)

Spud commit `8c990420`. Mudcrab `main` as of 2026-11-13.
Read-only audit. All counts are measured with `rg`/`wc` unless marked estimated.

## Summary

Spud converts the user's `Skyrim - Interface.bsa` SWF movies into a `.osu` cache format and runs them in a
hand-written Flash 8 / ActionScript 2 (AVM1) interpreter with the Scaleform GFx 3.x extensions the menus use.
It is a real interpreter, not an art extractor: 88 AVM1 opcode case labels, 176 native method definitions and
a 153-site delegate dispatch table.

Size, measured: `src/ui/*.cpp src/ui/*.hpp` 33 files / 18,077 lines; `src/ui/player/*` 19 files / 10,884
lines; together 52 files / 28,961 lines. `src/importer/convert/ui_*.cpp` 8 files / 3,062 lines. `docs/UI.md`
157 lines; `docs/formats/osu.md` 94 lines. Total in scope ≈ 32k lines.

Maturity: impl everywhere on the read path, no tests anywhere (no UI test target in either `CMakeLists.txt`;
the only compile-time checks in `src/ui` are three `static_assert` size checks in [`movie_renderer.cpp:32-91`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L32-L91)).
The only gate is `skytool ui play --all`, which reports script errors. Several game-data sources are capture
playbacks, not live systems ([`menus_hud.cpp:1-9`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L1-L9), [`menus_items.cpp:78-162`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L78-L162)).

The five findings that matter for Mudcrab:
1. The player↔host boundary is a **single virtual function**. `PlayerHost::external_call` ([`player.hpp:86`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L86)) is
   the only call out of the interpreter; everything else is one-way host→movie. That is a cheap contract to
   design against even if Mudcrab never writes an interpreter.
2. The whole movie contract is **110 distinct delegate names** dispatched from one string map
   ([`menu.hpp:164`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.hpp#L164), [`:178`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.hpp#L178), [`menu.cpp:160-184`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L160-L184)), plus **67 distinct host→movie callbacks**. A minimal HUD needs 4
   of the delegates; a dialogue menu 5; an inventory 27.
3. Vanilla behaviour lives in **C++ menu classes reading movie variables**, not in the movie. The HUD drives
   the movie by writing `CompassTargetDataA`, calling `SetCompassMarkers`, the meter `Set*Percent` methods and
   `ShowNotification` ([`menus_hud.cpp:549-563`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L549-L563), [`:673-720`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L673-L720), [`:593-620`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L593-L620)). Reimplementing the movie in Bevy means
   reimplementing that frame-by-frame engine side, which is the expensive part, not the drawing.
4. Movie drawing is small: 12 draw item kinds ([`draw.hpp:12-33`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/draw.hpp#L12-L33)), 6 supported blend modes, 4 supported filter
   kinds, stencil masks ([`movie_renderer.cpp:44-45`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L44-L45), [`:1113-1118`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1113-L1118), [`:1534-1596`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1534-L1596)).
5. Where spud does **not** transfer: it executes AS2. Mudcrab's roadmap decides Flash → `bsn!` transpile and
   AS2 → Luau (`docs/roadmap/03-luau-and-ui.md:3.4`). The interpreter, the AVM1 semantics and the GFx
   display-list emulation are therefore reference material only. The importer geometry stage, the delegate
   table, the capture-replay discipline and the headless movie runner do transfer.

## Scope read

Read, with `wc -l`:

| file | lines | how much read |
|---|---|---|
| `docs/UI.md` | 157 | all |
| `docs/formats/osu.md` | 94 | all |
| `src/ui/player/player.hpp` | 378 | all |
| `src/ui/player/player.cpp` | 2780 | 340-470, 740-790, 1130-1180 |
| `src/ui/player/vm.hpp` | 143 | all |
| `src/ui/player/vm.cpp` | 1345 | 990-1345 (the opcode switch) |
| `src/ui/player/builtins_display.cpp` | 2571 | method-registration list (all), 645-960, 1160-1470, 2140-2570 bodies sampled |
| `src/ui/player/builtins_core.cpp` | 767 | 153-193 |
| `src/ui/player/text.hpp` | 72 | all |
| `src/ui/player/text.cpp` | 804 | 40-270, 750-800 |
| `src/ui/player/display.hpp` | 294 | 1-120 |
| `src/ui/player/draw.hpp` | 59 | all |
| `src/ui/player/CMakeLists.txt` | 3 | all |
| `src/ui/menu.hpp` | 184 | all |
| `src/ui/menu.cpp` | 247 | 30-247 |
| `src/ui/menus_internal.hpp` | 102 | all |
| `src/ui/menus.cpp` | 384 | 1-60, 346-360 |
| `src/ui/menus_items.cpp` | 736 | all |
| `src/ui/menus_hud.cpp` | 1638 | 1-760 |
| `src/ui/menus_stats.cpp` | 1696 | 1440-1696 (Dialogue Menu) |
| `src/ui/ui.hpp` | 774 | 275-425 (UiHost), 438-460 |
| `src/ui/ui.cpp` | 1767 | 374-520, function index |
| `src/ui/movie_renderer.cpp` | 1792 | 44-50, 1090-1135, 1526-1600 |
| `src/importer/convert/ui_import.cpp` | 899 | 300-600 |
| `src/importer/convert/ui_script.cpp` | 272 | all |
| `src/importer/convert/ui_shapes.cpp` | 214 | 1-60 |
| `src/importer/convert/ui_images.cpp` | 332 | 1-50 |
| `apps/skytool/cmd_ui.cpp` | 533 | 355-420 |

Not read, and why:
- `menus_magic.cpp` (1127), `menus_journal.cpp` (807), `menus_world.cpp` (2046), `menus_loading.cpp` (571),
  `menus_saves.cpp` (116): outside the HUD/dialogue/inventory question. Handler names were counted but bodies
  not read.
- `ui.cpp` lines 520-1767 (input routing, console, captures, automation): only the function index and the
  message queue were read. Input routing details below are from [`ui.cpp:374-520`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L374-L520) and `ui.hpp` declarations.
- `movie_renderer.cpp` beyond the three regions: the pass recorder, layer atlas allocate and bitmap upload were
  not read line by line.
- `player.cpp` most of 2780 lines: draw, text layout, hit test and load internals were not read; claims about
  them are restricted to declarations I did read.
- `display.cpp` (721), `object.cpp` (134), `images.cpp` (93), `json.cpp` (33): not read.
- `scene.cpp`, `item3d.cpp`, `lock3d.cpp`, `book3d.cpp`, `starfield.cpp`, `ui_scenes.cpp`: the 3D UI slots are
  out of this topic's scope; only `Item3D`'s use from the inventory is reported.
- `ui_controls.cpp` (218), `ui_strings.cpp` (259), `ui_loading.cpp` (782), `ui_worlds.cpp` (86): counted, not
  read. The control-map and string-table claims come from `docs/formats/osu.md` only and are marked doc-only.
- Mudcrab `crates/converter`, `crates/engine`: searched (`rg -i "swf|interface|controlmap"`), not read
  exhaustively.

## Architecture

Five layers, each with one owner. Data flows up on the way in (conversion) and down on the way out (draw).

```
Skyrim - Interface.bsa  (.swf movies, translate_*.txt, controlmap.txt, fontconfig*.txt)
        |
        v  importer/convert/ui_import.cpp  (SWF tag walk, 3062 lines total across 8 ui_*.cpp)
  .osu files in <cache>/interface/   kind UIMV (movie), UITX (strings/translations),
        |                            UICT (controls), UIFM (font map)   [docs/formats/osu.md]
        v  player::Library  (one per process: movies, fonts map, translations, GMSTs, globals, ImageRegistry)
  player::Player  (one per open movie, own Vm + own _global)   [player.hpp:118]
        |            ^
        |            | VmHooks (12 virtuals: get_display, set_display, goto_frame, resolve_target, ...)
        v            |
  Vm / AVM1  (vm.cpp, 88 opcodes)   <-- builtins_core + builtins_display (176 native methods)
        |
        v  Player::render -> DrawList (meshes, glyphs, masks, layers in stage pixels)  [draw.hpp:12-33]
        |
        v  MovieRenderer::record  (render::Overlay: one 4x MSAA+stencil layer, layer atlas for filters)
  Ui  (menu stack, message queue, input routing, cursor, captures)   [ui.cpp]
        |
        +--> Menu subclasses (36 menu names, 153 delegate handler sites)   [menus*.cpp]
        |
        +--> UiHost (53 virtuals) --> App (src/engine/app_menus.cpp: 22 overrides)

  Reverse direction (engine -> movie): Menu::send(name, args) 93 sites / 67 distinct names
  Forward direction  (movie -> engine): PlayerHost::external_call -> Menu::external_call -> m_handlers
```

Diagram source lines: [`src/ui/player/player.hpp:118`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L118); [`src/ui/player/draw.hpp:12-33`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/draw.hpp#L12-L33).

### Lifecycle of one menu

1. `Ui::open(name)` pushes a show message and drains the queue ([`ui.cpp:374`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L374), [`:441`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L441)).
2. `Ui::show` builds the menu with `create_menu`, which calls `Menu::load` ([`ui.cpp:462`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L462), [`menus.cpp:346`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus.cpp#L346)).
3. `Menu::load` ([`menu.cpp:45-81`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L45-L81)) does the loader order: create the `Player` and run frame 1 with the default
   `showAll` scale; set scale mode and `Stage.visibleRect`/`safeRect`; `register_handlers`; `send("SetButtonMapping")`;
   `Advance(0)`; `invoke("InitExtensions")`; `invoke("SetPlatform", {gamepad, false})`. Then `on_loaded`.
4. Each frame `Menu::advance` writes `_root.CurrentTime = engine_ms` and calls `Player::advance(dt)`
   ([`menu.cpp:84-92`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L84-L92)), which steps at most two frames of the movie's frame rate and runs intervals
   ([`player.cpp:348-368`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L348-L368)).
5. The subclass's `advance` also runs its own engine side (HUD: `update()`; items: `Item3D::update`).
6. `Ui::overlay` walks menus from the topmost `menu_top_most_renderer` flag downward, builds each `DrawList`
   and hands them to `MovieRenderer::record` (`ui.hpp`, [`docs/UI.md:28-43`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L28-L43)).
7. `Ui::close` / `hide` calls `on_hide()`; a movie that animates out returns false and later calls
   `CloseMenu`, which re-queues the hide with force ([`ui.cpp:417`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L417), [`:498`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L498), [`menu.hpp:84-86`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.hpp#L84-L86)).

### Frame and event order (the part most reimplementations get wrong)

`Player::frame` marks each clip newest-first and runs load or enterFrame then the timeline
([`player.cpp:379-397`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L379-L397)). Actions go through a 3-priority queue: 0 normal, 1 constructors, 2 initialize events
([`player.hpp:305`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L305)). `queue_frame_actions` splits a frame's DoAction blocks around the place commands that
precede them, because the importer records `FrameAction::after` ([`player.cpp:457-497`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L457-L497), [`ui_import.cpp:490-492`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L490-L492)).
Loads requested by script land after the frame's actions in the same Advance ([`player.cpp:399-420`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L399-L420)).

### Threading and ownership

The player is single-threaded and main-thread. The only thread is `Library::m_reader`, which pre-reads `.osu`
files ahead ([`player.hpp:64`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L64), [`:78`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L78); guarded by `m_early_mutex`). Bitmap upload happens on the renderer's background threads
and a bitmap is drawn from the frame after it arrives ([`docs/UI.md:28-43`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L28-L43); the upload code is in
`movie_renderer_images.cpp`, not read). No UI state is shared with the simulation across threads.

### Runtime boundary, enumerated

**(a1) The single outbound call.** A movie reaches the engine only through `ExternalInterface.call`. Spud
implements `flash.external.ExternalInterface.call` in [`builtins_display.cpp:2307`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L2307), which calls
`PlayerHost::external_call` ([`player.hpp:85-86`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L85-L86)). `Menu::external_call` ([`menu.cpp:160-184`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L160-L184)) treats
`args[0]` as a response id, dispatches `args[1..]` to `m_handlers[method]`, and catches exceptions so an engine
error cannot unwind through the script. An unknown method logs `unhandled delegate call` and returns
undefined. That is the complete boundary: **1 virtual, 110 dispatch names**.

**(a2) The reverse direction is not a callback.** The engine calls into the movie by name:
`Menu::send` prepends the callback name and calls `Player::invoke_or_callback("call", ...)`
([`menu.cpp:94-112`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L94-L112)), i.e. the movie's registered `ExternalInterface.addCallback("call", ...)`, or its
ActionScript path `call`. `Menu::respond` answers a pending delegate call. 93 send sites, 67 distinct names.
A movie with neither registration (`SafeZone`, `Titles`, `LoadWaitSpinner`) silently ignores the call
([`menu.cpp:106`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L106)).

**(a3) Player builtins are not host calls.** The 176 native methods (159 display + 17 core) execute inside the
interpreter. Nine of them read host-set player state (`Key.last_key`, `Selection` focus, `Stage` rect,
`Mouse` position); none of them call the engine.

**(a4) The delegate handler inventory, by menu family** (counted with
`rg -o 'handle\("[^"]+"' src/ui/*.cpp | wc -l` = 153 sites, `| sort -u` = 110 distinct names):

| menu file | sites | names |
|---|---|---|
| `menus_world.cpp` | 35 | map, crafting, race/sex, tutorial, safezone |
| `menus_items.cpp` | 27 | inventory, container, barter, gift |
| `menus_journal.cpp` | 23 | quests, stats tab, system tab |
| `menus_magic.cpp` | 21 | magic, favorites, tween |
| `menus.cpp` | 15 | main, fader, message box, console, credits |
| `menus_stats.cpp` | 11 | stats, level up, book, dialogue |
| `menus_saves.cpp` | 10 | save/load, shared by main and journal |
| `menus_hud.cpp` | 9 | HUD, sleep/wait, training, lockpicking |
| `menus_loading.cpp` | 2 | loading menu |

**(a5) The engine side of the boundary.** `UiHost` declares 52 `ui_*` virtuals plus a virtual destructor
([`ui.hpp:275-417`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.hpp#L275-L417)); `App` overrides 45 of them (`rg -o "App::ui_[a-z_]+\(" src/engine/*.cpp | sort -u`). The
7 left at their default are the ones a spud session does not provide: `ui_journal`, `ui_saves`,
`ui_text_setting`, `ui_worldspace`, `ui_cell_north`, `ui_mods_loaded`, `ui_dialogue_live`. The default body is
what makes capture replay possible: a menu asks `ui_item_source()` and gets `nullptr`, then falls back to JSON
recorded from the real game ([`menus_items.cpp:78-162`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L78-L162)).

**(a6) Minimal count for the three target menus.**
- HUD: 4 delegate handlers (`PlaySound`, `RegisterHUDComponents`, `GetButtonFromUserEvent`, `myLog`,
  [`menus_hud.cpp:413-441`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L413-L441)) and 12 host methods (`ui_play_sound`, `ui_player_pose`, `ui_cell_north`,
  `ui_compass_markers`, `ui_player_sneaking`, `ui_meter_percent`, `ui_survival`, plus `game_setting`,
  `button_name`, `capture`, `hud_recording`, `host()`).
- Dialogue: 5 delegate handlers (`SkipText`, `TopicClicked`, `FadeDone`, `CloseMenu`, `PlaySound`,
  [`menus_stats.cpp:1591-1616`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1591-L1616)) and 4 host methods (`ui_dialogue_live`, `ui_dialogue_topic`, `ui_dialogue_skip`,
  `ui_dialogue_closed`) plus 5 host→movie sends (`SetSpeakerName`, `ShowDialogueText`,
  `PopulateDialogueList`, `ShowDialogueList`, `NotifyVoiceReady`).
- Inventory: 27 handler sites over the shared `ItemMenu` base plus 4 subclasses ([`menus_items.cpp:241-333`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L241-L333), [`:497-541`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L497-L541), [`:578-589`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L578-L589), [`:627-640`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L627-L640), [`:670-684`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L670-L684)), and one host interface, `ItemSource` (`ui/items.hpp`, not read), with 5
  methods used here: `data`, `request`, `should_close`, `closed`, `soul_gem_list`.

## Feature ledger

Columns: id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | what
returns a default or a no-op. Evidence levels are those defined in the [reading guide](../README.md#how-to-read-these-reports).

### A. Movie import (`src/importer/convert/ui_*.cpp`)

| id | behaviour | vanilla | spud impl | file:line | evidence | deviation | default/no-op |
|---|---|---|---|---|---|---|---|
| uimv-import-swtags | SWF tag walk | GFx reads SWF 7-10 tags | `Converter::run` switches on tag code; handles shape 2/3/4, bits/jpeg, font2/3, edit text, button2, sprite, place 1/2/3, remove 1/2, frame label, do action, do init action, export/import assets, scaling grid, background colour | [`ui_import.cpp:398-556`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L398-L556) | impl | none seen | 10 known-ignorable tags dropped ([`:544`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L544)); anything else warns once and is dropped ([`:550-555`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L550-L555)) |
| uimv-import-coords | Coordinate space | twips | divides by 20 (`k_twips`) to stage pixels, y down | [`ui_import.cpp:300-320`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L300-L320), [`:39-45`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L39-L45) | impl | none seen | — |
| uimv-import-sprite | Nested sprites | timeline clips | recursive `timeline()`; sprite frame count recomputed from tags, not trusted from the tag header | [`ui_import.cpp:437-446`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L437-L446) | impl | recomputes a value the file states | — |
| uimv-import-frame | Frame assembly | ShowFrame ends a frame | pending command/action/init/label lists flushed on ShowFrame; a final non-empty frame is kept without ShowFrame | [`ui_import.cpp:489`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L489), [`:559-566`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L559-L566) | impl | none seen | — |
| uimv-import-factorder | DoAction tag order | DoAction runs after the place tags of its frame | each block records `after = commands.size()` so the player can interleave | [`ui_import.cpp:490-492`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L490-L492) | impl | none seen | — |
| uimv-import-init | DoInitAction | runs when the class is first constructed | recorded per character, run once by `init_library` | [`ui_import.cpp:493-497`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L493-L497); [`player.cpp:935`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L935) | impl | none seen | a movie never referenced runs no class |
| uimv-import-shape | Shape to triangles | GFx tessellates fills and strokes | quadratic Bezier flattened to a tolerance, then libtess2 nonzero winding | [`ui_shapes.cpp:16-60`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_shapes.cpp#L16-L60) | impl | tessellator may differ from GFx on self-intersecting paths; unverified | failed tessellation returns an empty mesh |
| uimv-import-bitmap | Bitmaps | GFx decodes JPEG/PNG/lossless | decoded at import to RGBA8 premultiplied, rows top-down, JPEG tables supported | [`docs/formats/osu.md:44-46`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osu.md#L44-L46); [`ui_images.cpp:1-50`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_images.cpp#L1-L50) | impl | none seen | undecodable bitmap warns and is skipped |
| uimv-import-text | Edit text | DefineEditText fields | copies all layout and flag bits incl. `was_static`, `html`, `use_outlines`, `text_auto_size` | [`ui_import.cpp:296-320`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L296-L320) | impl | none seen | — |
| uimv-import-button | Buttons | Button2 records and conditions | state records, blend, filters, condition/action pairs | [`ui_import.cpp:322-348`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L322-L348) | impl | none seen | — |
| uimv-import-place | Placement commands | PlaceObject2 semantics | copies flags one by one: move, character, matrix, cxform, ratio, name, clip depth, filters, blend, cacheAsBitmap, visible, clip events | [`ui_import.cpp:350-414`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L350-L414) | impl | `blend` is `tag value - 1`, remapping Flash's enum to spud's | an absent flag means "keep the previous value" |
| uimv-import-script | Script translation | AVM1 bytecode | `ScriptTranslator::translate` decodes actions, resolves branch targets to instruction indices | [`ui_script.cpp:58-232`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_script.cpp#L58-L232) | impl | — | see the `uimv-op-*` rows |
| uimv-import-pool | ConstantPool | pool per function scope | folded into the push values of the functions defined while the pool is in effect | [`ui_script.cpp:97-108`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_script.cpp#L97-L108) | impl | none seen | a constant index outside the pool becomes `undefined` and warns |
| uimv-import-branch | Jump/If targets | byte offsets | mapped through `at_byte`; a target that is not an action boundary warns and snaps to the nearest boundary | [`ui_script.cpp:236-245`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_script.cpp#L236-L245) | impl | **snapping changes control flow** for a malformed movie | — |
| uimv-import-geturl | GetURL | browser call | rewritten to push (url, target) then GetURL2 | [`ui_script.cpp:175-190`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_script.cpp#L175-L190) | impl | — | GetURL2 pops both and does nothing ([`vm.cpp:1297`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1297)) |
| uimv-import-unsupported | With, Try, WaitForFrame, WaitForFrame2 | supported by Flash | warned at import and emitted as a no-op instruction | [`ui_script.cpp:220-224`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_script.cpp#L220-L224) | impl | **silent behaviour loss** if a movie used them | the block they guard runs unconditionally |
| uimv-import-envmaps | Strings, translations, controls, font map, GMSTs, globals, sound categories | engine-side tables | four `.osu` kinds (`UIMV`, `UITX`, `UICT`, `UIFM`) plus `gamesettings.osu`, `globals.osu`, `soundcategories.osu` | [`docs/formats/osu.md:14-22`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osu.md#L14-L22), [`:60-94`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osu.md#L60-L94) | doc-only | I did not read `ui_controls.cpp` or `ui_strings.cpp`; claims rest on the format doc | a missing table makes the library lookup return nullptr |
| uimv-import-version | Cache version gate | — | `.osu` version 2; a movie is reconverted when missing or of another version | [`docs/formats/osu.md:9-11`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osu.md#L9-L11); [`ui_import.cpp:718`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_import.cpp#L718) | impl | — | a stale cache is the failure mode; the source SWF is not re-hashed |

### B. ActionScript 2 / AVM1 semantics (`src/ui/player/vm.cpp`)

| id | behaviour | vanilla | spud impl | file:line | evidence | deviation | default/no-op |
|---|---|---|---|---|---|---|---|
| uimv-op-count | Opcode coverage | AVM1 has about 70 defined actions plus SWF 6/7 additions | 88 `case` labels in one switch | [`vm.cpp:1013-1340`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1013-L1340) | impl | see the unsupported list below | unknown op: `error("unsupported action 0x..")`, the block stops |
| uimv-op-arith | Add/Sub/Mul/Div/Modulo/Inc/Dec | ECMA 262 | `to_number` then double arithmetic; `0x50`/`0x51` add ±1 | [`vm.cpp:1031-1034`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1031-L1034), [`:1142`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1142), [`:1215-1216`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1215-L1216) | impl | none seen | NaN propagates as ECMA requires |
| uimv-op-compare | Equals/Less/And/Or/Not/StringEquals/StringLess/Greater/StringGreater | ECMA, with AVM1's string-first `Less` | `abstract_equals`, `abstract_less`, `strict_equals` are separate functions | [`vm.cpp:1035-1040`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1035-L1040), [`:1082`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1082), [`:1207-1209`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1207-L1209), [`:1257-1259`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1257-L1259); [`vm.hpp:139-141`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.hpp#L139-L141) | impl | none seen | an undefined relational result pushes `undefined` |
| uimv-op-bit | BitAnd/Or/Xor/LShift/RShift/URShift | int32, shift count `& 31` | `to_int32` then int32 ops | [`vm.cpp:1250-1255`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1250-L1255) | impl | none seen | — |
| uimv-op-string | StringLength/StringExtract/Chr/Ord and MB variants | 1-based index, MB = byte | `mb`/`mb` pairs share one case | [`vm.cpp:1041-1046`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1041-L1046), [`:1100-1101`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1100-L1101) | impl | MB variants take the first UTF-8 byte, which is not cp1252 for non-ASCII | out-of-range extract pushes an empty string |
| uimv-op-var | GetVariable/SetVariable/DefineLocal/Delete/Delete2 | scope chain lookup | walks `Scope` parents; `Delete2` searches the chain | [`vm.cpp:1051-1052`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1051-L1052), [`:1108-1163`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1108-L1163) | impl | none seen | deleting an absent name returns false |
| uimv-op-prop | GetProperty/SetProperty | `_x`, `_alpha`, ... | delegated to `hooks->get_display`/`set_display` | [`vm.cpp:1062-1080`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1062-L1080); [`player.hpp:351-352`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L351-L352) | impl | — | an unknown property reads `undefined`; a write is ignored |
| uimv-op-target | SetTarget/SetTarget2/GotoFrame/GotoLabel/GotoFrame2 | path-relative timeline control | `hooks->resolve_target`, `goto_frame`, `goto_label`; GotoFrame2 parses `path:frame` | [`vm.cpp:1269-1295`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1269-L1295), [`:1311-1339`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1311-L1339) | impl | — | a bad path leaves the current target |
| uimv-op-call | CallFunction/NewObject/NewMethod/CallMethod/Call | argument count on the stack | `pop_args` clamps the count to the stack size | [`vm.cpp:1130-1152`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1130-L1152), [`:1217-1249`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1217-L1249) | impl | a wrong arg count is silently clamped instead of corrupting the frame | returns undefined |
| uimv-op-fn | DefineFunction/DefineFunction2 | registers, preload, suppress flags | child body translated recursively; `function2` keeps register count and flags | [`vm.cpp:1298-1310`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1298-L1310); [`ui_script.cpp:196-215`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_script.cpp#L196-L215) | impl | — | — |
| uimv-op-with | With | scope override | **not supported** | [`ui_script.cpp:220-224`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_script.cpp#L220-L224) | stub | the body runs in the caller's scope | no-op |
| uimv-op-clone | CloneSprite/RemoveSprite (0x24, 0x25) | duplicate or remove a clip | **no case in the switch** | absent from [`vm.cpp:1013-1340`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1013-L1340) | stub | a runtime error if reached | error logged, block stops |
| uimv-op-drag | StartDrag/EndDrag (0x27, 0x28) | drag a clip | reached only through the `MovieClip.startDrag` natives | [`builtins_display.cpp:1435-1445`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L1435-L1445) | partial | the opcode form does not work | — |
| uimv-op-callframe | Call (0x9E, frame actions by name) | runs a frame's actions as a subroutine | pops the name and does nothing; the comment says the game's movies do not use it | [`vm.cpp:1310`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1310) | stub | **silent no-op** | nothing |
| uimv-op-geturl2 | GetURL2 | navigate | pops both operands, no browser | [`vm.cpp:1297`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1297) | stub | none for an offline engine | no-op |
| uimv-op-quality | ToggleQuality/StopSounds | toggles quality, stops audio | both no-ops | [`vm.cpp:1030`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1030) | stub | — | no-op |
| uimv-op-throw | Throw (0x2A) | throws | **no case**; falls to `default` and errors | absent from [`vm.cpp:1013-1340`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1013-L1340) | stub | only matters for `try`, which is itself unsupported | error |
| uimv-op-instrlimit | Runaway loop guard | Flash hangs or shows a timeout dialog | `k_max_instructions` per block stops the block with an error | [`vm.cpp:1016-1020`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1016-L1020) | impl | a legitimate long loop is cut off; the limit constant was not read | the block returns undefined |
| uimv-conv | Number/string/boolean conversion | ECMA 262 3rd ed. with AVM1 SWF 7+ rules | `to_number`, `to_string`, `to_boolean`, `to_primitive`, `number_to_string(radix)` | [`vm.hpp:89-96`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.hpp#L89-L96) | impl | none seen | — |
| uimv-proto | Prototypes, `super`, `instanceof`, `extends`, `implements` | AS2 class model | prototype-chain walk, `make_super`, `instance_of`, `Extends`, `ImplementsOp` | [`vm.hpp:99-124`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.hpp#L99-L124); [`vm.cpp:1088-1098`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1088-L1098), [`:1260-1268`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1260-L1268) | impl | none seen | — |
| uimv-enumerate | for..in order | implementation-defined | `enumerate` returns property order; `Enumerate`/`Enumerate2` share a case | [`vm.cpp:1187-1197`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1187-L1197) | impl | the order may differ from GFx; unverified | empty list |
| uimv-gettimer | GetTime | milliseconds since the player started | `getTimer`; a fixed value makes scripted runs reproducible | [`vm.cpp:1103-1108`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1103-L1108); [`vm.hpp:66`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.hpp#L66) | impl | none seen | — |
| uimv-random | RandomNumber | 0..n-1 | `mt19937` seeded 0x05ca1e | [`vm.cpp:1099`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1099); [`vm.hpp:69`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.hpp#L69) | impl | **deterministic seed** where Flash seeds from the clock; visible only in content that uses `random` | 0 when n <= 0 |
| uimv-trace | Trace | debug log | `log::debug` | [`vm.cpp:1081`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/vm.cpp#L1081) | impl | — | dropped below the log level |

### C. Display list and timelines (`src/ui/player/player.cpp`, `display.*`)

| id | behaviour | vanilla | spud impl | file:line | evidence | deviation | default/no-op |
|---|---|---|---|---|---|---|---|
| uimv-display-kinds | Object kinds | clip, shape, text, button | `DisplayKind {clip, shape, text, button}` | [`display.hpp:24`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/display.hpp#L24) | impl | — | — |
| uimv-display-depth | Depth space | timeline depths negative, script depths >= 0 | timeline depth offset −16384 | [`display.hpp:23`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/display.hpp#L23) | impl | none seen | — |
| uimv-display-names | Lazy `instanceN` naming | GFx names unnamed objects when first referenced | resolved on demand, `instance{n}` for clips, `shape{n}` for shapes | [`player.hpp:213-219`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L213-L219); [`display.hpp:55-63`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/display.hpp#L55-L63) | impl | shape naming is spud's own, for dumps | an unreferenced object stays unnamed |
| uimv-display-start | Movie start | frame 1 initialised at creation | `start()` creates the root, makes its AS object, runs frame 1 | [`player.cpp:340-346`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L340-L346) | impl | none seen | — |
| uimv-display-advance | Advance(dt) | `Advance(dt, 2)` | frame-rate accumulator, at most 2 frames, excess time dropped | [`player.cpp:348-370`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L348-L370) | impl | dropping time is GFx's own rule | — |
| uimv-display-order | Clip execution order | newest first | reversed `m_exec` copy; clips created during the pass wait | [`player.cpp:379-398`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L379-L398) | impl | none seen | — |
| uimv-display-place | Placement, move, remove | PlaceObject semantics | `apply_place`: a script-taken object ignores later timeline matrices; clip depth, blend, filters, visibility, clip events | [`player.cpp:740-780`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L740-L780) | impl | `script_transformed` stops the timeline controlling the object | a move for a missing depth is ignored |
| uimv-display-mask | Masks | clip depth masks siblings above | `clip_depth` on the mask object, drawn as stencil brackets | [`display.hpp:72`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/display.hpp#L72); [`draw.hpp:16-19`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/draw.hpp#L16-L19) | impl | — | — |
| uimv-display-scale9 | Scale-9 grids | DefineScalingGrid or `scale9Grid` | rect per sprite or a script override; 9-slice vertex remap | [`player.cpp:2558-2560`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2558-L2560), [`:2649`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2649) | impl | unverified against GFx on rotated grids | no grid: plain transform |
| uimv-display-scrollrect | scrollRect | clips content to a rect | own-space rect applied during draw | [`player.cpp:2490-2494`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2490-L2494) | impl | — | — |
| uimv-display-cachebitmap | cacheAsBitmap | rasterise the object | the flag is recorded from tag and script but **no rasterisation path was found** | [`player.cpp:768-770`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L768-L770); [`display.hpp:73`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/display.hpp#L73) | stub | GFx rasterises; spud draws vectors | the flag has no effect |
| uimv-display-topmost | GFx `topmostLevel` | GFx extension: draw above everything | `topmost` objects are drawn last | [`display.hpp:76`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/display.hpp#L76); [`player.hpp:373`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L373) | impl | — | — |
| uimv-display-hittestdisable | GFx `hitTestDisable` | the mouse passes through | the flag is checked in `wants_mouse` | [`display.hpp:77`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/display.hpp#L77); [`player.cpp:1924`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L1924) | impl | — | — |
| uimv-events-clip | Clip events | `onClipEvent`, `on` button handlers | 19 flags as SWF stores them; timeline binding plus a method form | [`player.hpp:96-114`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L96-L114); [`player.cpp:1021-1054`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L1021-L1054) | impl | — | an event with no handler does nothing |
| uimv-events-queue | Action queue priorities | initialize, constructors, the rest | 3 deques; `run_queue` drains high priority first with a 4,000,000-action guard | [`player.cpp:455`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L455), [`:508-527`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L508-L527) | impl | the guard is spud's | the queue does not drain inside a script |
| uimv-events-construct | Construct event | place, then the constructor, then frame 1 | `construct` and `construct_scripted` split timeline from script creation | [`player.cpp:818`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L818), [`:878`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L878) | impl | none seen | — |
| uimv-events-loadmovie | loadMovie/loadClip | asynchronous load | `queue_load` defers to the next frame; `land_loads` runs the callbacks after the swap | [`player.hpp:186-188`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L186-L188); [`player.cpp:399-420`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L399-L420) | impl | — | a missing movie calls `loaded(false)` and the clip keeps its content |
| uimv-events-interval | setInterval/setTimeout | timers | `Interval` list, minimum period 1 ms, run after each advance | [`player.cpp:1173-1183`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L1173-L1183) | impl | the 1 ms minimum is spud's | — |
| uimv-events-broadcast | AsBroadcaster | listener broadcast to Key/Mouse/Stage/Selection | `broadcast` queues the call | [`player.cpp:499-507`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L499-L507) | impl | — | no listeners: nothing |
| uimv-mouse-move | Mouse move | onMouseMove, `Mouse` broadcast, hover update | drag first, then clip events, then the `Mouse` broadcast, then `update_hover` | [`player.cpp:2133-2153`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2133-L2153) | impl | none seen | — |
| uimv-mouse-button | Mouse button | onPress/onRelease/onReleaseOutside, onPressAux | left button drives buttons and focus; other buttons drive `onPressAux`/`onReleaseAux` | [`player.cpp:2156-2198`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2156-L2198) | impl | the comment records a GFx detail: `onItemSelect` acts only when the mouse index is not 0 | — |
| uimv-mouse-hit | Hit testing, topmost entity | topmost mouse target | `pick` + `wants_mouse` + `topmost_mouse_target`; the script `hitTest` native | [`player.cpp:2047`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2047), [`:1924`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L1924); [`builtins_display.cpp:1416`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L1416) | impl | — | no target: no call |
| uimv-key-input | Key routing | clip key events, focus handlers, text editing | `key`: broadcast, focus `onKeyDown`, `Key` broadcast, then field editing for 8/46/37/39/36/35/13 | [`player.cpp:2225-2283`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2225-L2283) | impl | none seen | an unhandled key does nothing |
| uimv-key-textinput | Typed characters | text input events | `text_input` inserts at the caret, fires onChanged and listeners | [`player.cpp:2285-2305`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2285-L2305), [`:2198-2224`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2198-L2224) | impl | — | no focus, or not editable: dropped |
| uimv-focus | Focus and Selection | `Selection.setFocus` | one focus pointer; a text field takes focus on click | [`player.hpp:182-184`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L182-L184); [`player.cpp:2179`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2179) | impl | — | — |
| uimv-gc | Garbage collection | reference-counted heap | `collect_garbage` past `k_collect_after` bytes; natives pin objects | [`player.cpp:450-453`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L450-L453); [`player.hpp:190`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L190) | impl | cycles unverified | — |

### D. Text (`src/ui/player/text.cpp`)

| id | behaviour | vanilla | spud impl | file:line | evidence | deviation | default/no-op |
|---|---|---|---|---|---|---|---|
| uimv-text-layout | Text layout | GFx lays out glyph runs with metrics from the font library | `layout_text` produces placed glyphs, images, lines, per-char pen positions, text width and height | [`text.hpp:44-56`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.hpp#L44-L56); [`text.cpp:745-750`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.cpp#L745-L750) | impl | — | an empty field lays out nothing |
| uimv-text-fonts | Font resolution | `fontconfig.txt` maps a logical name to a font in a font library movie | field's own movie first, then libraries; `$EverywhereFont` through the map | [`player.hpp:228-230`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L228-L230); [`docs/UI.md:44-50`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L44-L50) | impl | — | an unknown font falls back to the name itself ([`player.hpp:56`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L56)) |
| uimv-text-glyphs | Glyph drawing | vector glyph outlines | glyphs stored as triangles in em units; drawn as `DrawItem::glyph` filled with the run colour | [`docs/formats/osu.md:48`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osu.md#L48); [`draw.hpp:14`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/draw.hpp#L14) | impl | — | a code point with no glyph is skipped |
| uimv-text-kerning | Kerning and letter spacing | kerning pairs, `letterSpacing` | kerning table per font; `letterSpacing` truncated to whole pixels, matching measured GFx behaviour | [`text.cpp:235-244`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.cpp#L235-L244); [`docs/formats/osu.md:48`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osu.md#L48) | impl | none seen (a measured deviation that spud reproduces) | — |
| uimv-text-html | HTML text | Flash 8 text-field HTML subset | `<br> <sbr> <p> <li> <img> <font> <b> <i> <u> <a> <textformat>` plus five entities | [`text.cpp:40-56`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.cpp#L40-L56), [`:166-266`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.cpp#L166-L266) | impl | unknown tags are ignored, not errors | unknown tag ignored |
| uimv-text-autosize | Scaleform `textAutoSize` | Scaleform extension, `shrink` or `fit` | up to 12 iterations of uniform scale to fit the field | [`text.cpp:754-773`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.cpp#L754-L773) | impl | derived from measured in-game values | neither value: no scaling |
| uimv-text-valign | Scaleform `verticalAlign` | Scaleform extension | `center`/`bottom`; a non-multiline auto-size field defaults to centred | [`text.cpp:781-799`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.cpp#L781-L799) | impl | the comment records that multiline `fit` is unmeasured | `none` on a multiline field: top |
| uimv-text-img | Inline `<img>` | bitmap or `img://` engine image | resolves to a movie bitmap or an `ImageRegistry` id, with the width/height attributes | [`text.hpp:26-41`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.hpp#L26-L41), [`:68-72`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.hpp#L68-L72) | impl | engine images are treated as 64x64 by the renderer ([`docs/UI.md:52-60`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L52-L60)) | unresolved image is skipped |
| uimv-text-translate | `$KEY` translation | localized strings | `translate_text` through `translate_<language>.txt` | [`player.hpp:228`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L228); [`docs/UI.md:44`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L44) | impl | — | a missing key yields the key |
| uimv-text-style | TextFormat, getTextExtent, getCharBoundaries, getLineMetrics, getLineText, getLineOffset, getLineLength | Flash 8 TextField API | all present; `getFontList` returns an empty array | [`builtins_display.cpp:1870-2138`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L1870-L2138), [`:1714`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L1714) | impl | `getFontList` is a stub | `getFontList` returns `[]` |
| uimv-text-edit | Editable fields | caret, selection, onChanged | caret and selection indices, Backspace/Delete/arrows/Home/End/Enter | [`player.cpp:2225-2283`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2225-L2283) | impl | no IME, no clipboard | non-editable field ignores keys |
| uimv-text-gutter | Field padding | 2 px gutter | `k_text_gutter = 2.0f` | [`text.hpp:58`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.hpp#L58) | impl | measured constant | — |

### E. Drawing and the movie renderer (`src/ui/movie_renderer.cpp`, `draw.hpp`)

| id | behaviour | vanilla | spud impl | file:line | evidence | deviation | default/no-op |
|---|---|---|---|---|---|---|---|
| uimv-render-drawlist | Draw list contract | GFx builds a render tree | 12 item kinds: mesh, glyph, rect, mask_begin/content/end/done, layer_begin/end, triangles, bitmap, image | [`draw.hpp:12-33`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/draw.hpp#L12-L33) | impl | renderer-independent, which is what makes headless running possible | — |
| uimv-render-main | Main pass | one UI layer over the scene | one 4x MSAA layer with stencil, resolved and composited with premultiplied alpha | `movie_renderer.cpp` (pass recorder); [`docs/UI.md:36-40`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L36-L40) | impl | the whole UI is a scene overlay, not a screen-space canvas | — |
| uimv-render-stencil | Masks | nested clip masks | stencil level per `mask_begin`, opening and closing around content | [`movie_renderer.cpp:1113-1131`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1113-L1131) | impl | — | — |
| uimv-render-layers | Filter and blend layers | offscreen passes | each filtered or blended object renders to its own target, is copied into a layer atlas, filtered, composited back | [`docs/UI.md:41-43`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L41-L43) | impl | — | — |
| uimv-render-blend | Blend modes | 14 BlendMode values | add, multiply, screen, lighten, darken, subtract map to pipelines; everything else falls back to normal | [`movie_renderer.cpp:1113-1118`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1113-L1118) | partial | overlay, hardlight, difference, invert, alpha, erase, layer all draw as normal | unsupported mode: normal |
| uimv-render-filter-blur | Blur filter | Gaussian | box blur, size clamped 1..255, per pass | [`movie_renderer.cpp:1534-1548`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1534-L1548) | impl | box vs Gaussian approximation (Ruffle's), so a small visual difference is possible | blur < 1 draws sharp |
| uimv-render-filter-shadow | Drop shadow and glow | Gaussian alpha blur plus composite | alpha-only blur, then a shadow pass with distance, angle, colour, strength, inner/knockout/composite flags | [`movie_renderer.cpp:1550-1577`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1550-L1577) | impl | — | — |
| uimv-render-filter-matrix | Colour matrix filter | 4x5 matrix | rows r,g,b,a with offsets divided by 255 | [`movie_renderer.cpp:1578-1594`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1578-L1594) | impl | — | fewer than 20 values: identity |
| uimv-render-filter-missing | Bevel, gradient glow, gradient bevel, convolution | supported by Flash 8 | **no effect**; one warning per process | [`movie_renderer.cpp:1595-1600`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1595-L1600) | stub | the object draws unfiltered | filter ignored |
| uimv-render-images | Engine images `img://` | engine image loader, 64x64 | `ImageRegistry` shared by all views; a bitmap is drawn from the frame after it uploads | [`docs/UI.md:52-60`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L52-L60); [`player.hpp:66-68`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L66-L68) | impl | — | an unregistered name draws nothing |
| uimv-render-scale | Stage scale modes | noScale, showAll, exactFit, noBorder plus alignment | `stage_to_pixels` matrix, then `Stage.visibleRect`/`safeRect` inset by `fSafeZone*` = 15 | [`menu.cpp:186-236`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L186-L236) | impl | `noScale` alignment added by spud for the console movie | unknown mode: 1:1 |
| uimv-render-resize | Stage resize notification | `Stage.onResize` under noScale | broadcast when the viewport changes | [`menu.cpp:228-232`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L228-L232) | impl | — | no listeners: nothing |
| uimv-render-bitmap | Movie bitmaps | decoded RGBA8 | premultiplied, rows top-down, async upload | [`docs/formats/osu.md:44-46`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osu.md#L44-L46) | impl | — | a bitmap still uploading is skipped for that frame |

### F. Menu stack, host boundary, input (`menu.*`, `ui.*`, `app_menus.cpp`)

| id | behaviour | vanilla | spud impl | file:line | evidence | deviation | default/no-op |
|---|---|---|---|---|---|---|---|
| uimv-menu-desc | Menu properties | each IMenu has a movie, scale mode, depth, flags, background alpha, input context | `MenuDesc` with the same fields; 14 flag bits named as vanilla's | [`menu.hpp:20-47`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.hpp#L20-L47) | impl | flag names map one-to-one to observed values | unknown flag bit is ignored |
| uimv-menu-count | Menu inventory | dozens of IMenu classes | 36 names in `create_menu` | [`menus.cpp:346-352`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus.cpp#L346-L352), `menus_*.cpp` | impl | — | an unknown name logs `no menu called` and does nothing ([`ui.cpp:475`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L475)) |
| uimv-menu-queue | Message queue | show and hide messages serialise | show/hide queue, drained outside script execution, 256-message guard | [`ui.cpp:441-460`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L441-L460) | impl | the guard is spud's | re-entrant messages wait |
| uimv-menu-show | Show | Show message, then the menu's own | `Ui::show` creates, loads, prepares movie buffers, `on_show` | [`ui.cpp:462-490`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L462-L490) | impl | a second show for `MessageBoxMenu` and `Fader Menu` re-runs `on_show` | an unopenable movie logs and returns |
| uimv-menu-hide | Hide | `on_hide` may animate first | `on_hide()` false keeps the menu and waits for `CloseMenu` | [`ui.cpp:498-517`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L498-L517) | impl | — | a menu that never answers stays open |
| uimv-menu-cursor | Cursor menu | the Cursor Menu follows the device | cursor shown per device; `Mouse.show`/`hide` from script | [`ui.cpp:524`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.cpp#L524), [`player.hpp:249`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L249) | impl | — | — |
| uimv-menu-inputctx | Input contexts | `controlmap.txt` context stack | each menu pushes its contexts; a menu may add and drop its own (quantity slider) | [`menu.hpp:104-105`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.hpp#L104-L105); [`menus_items.cpp:284-318`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L284-L318) | impl | — | no context: only raw keys |
| uimv-menu-userevent | User events | message type 7 | `on_user_event` on the top menu; the HUD and inventory filter by event name | [`menu.hpp:88-91`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.hpp#L88-L91); [`menus_items.cpp:186-238`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L186-L238) | impl | — | unhandled event returns false and falls through |
| uimv-menu-key | Scaleform key event | message type 6, byte 2 passes on | `on_key` before the movie; `pass_through` sends it down | [`menu.hpp:98-100`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.hpp#L98-L100), [`:84`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.hpp#L84) | impl | — | false: the movie gets the key |
| uimv-menu-passthrough | Pass-through | HUDMenu answers 2 to everything | `HudMenu::pass_through()` true; `DialogueMenu` true while inert | [`menus_hud.cpp:169`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L169); [`menus_stats.cpp:1656`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1656) | impl | — | — |
| uimv-menu-visible | Movie visibility | a dialogue movie under a pausing menu is hidden | `movie_visible()` | [`menu.hpp:102`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.hpp#L102); [`menus_stats.cpp:1657`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1657) | impl | — | default visible |
| uimv-menu-blur | UI blur count | DefaultObject 170 blur behind menus | `BlurHold` increments on construction, decrements on destruction | [`menus_internal.hpp:59-92`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_internal.hpp#L59-L92) | impl | — | balanced holds mean no blur |
| uimv-menu-hudmode | HUD mode holds | each menu sets a HUD mode | `HudModeHold` sets and clears "InventoryMode", "DialogueMode", "JournalMode", ... | [`menus_internal.hpp:36-56`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_internal.hpp#L36-L56) | impl | — | no HUD open: no-op |
| uimv-host-external | Movie→engine channel | GameDelegate `call` | one virtual, dispatched by name, response id as first arg, exceptions caught | [`player.hpp:85-86`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L85-L86); [`menu.cpp:160-184`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L160-L184) | impl | — | unknown method: logged, returns undefined |
| uimv-host-interface | Engine services | GameDelegate and the engine's menu sink | 52 `ui_*` virtuals, each defaulting to "no live system" | [`ui.hpp:275-417`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.hpp#L275-L417) | impl | **defaults are the design**: a null source means capture replay | default returns false, 0, empty or nullptr |
| uimv-host-app | Host implementations | — | 45 of the 52 overridden across `src/engine/` | [`app_menus.cpp:234`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_menus.cpp#L234), [`:300`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_menus.cpp#L300), [`:317`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_menus.cpp#L317), [`:421`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_menus.cpp#L421), [`:527`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_menus.cpp#L527), [`:665`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_menus.cpp#L665), [`:819`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_menus.cpp#L819), [`:948`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_menus.cpp#L948); `app_world.cpp`, `app_items.cpp` | impl | save, time and settings are spud's own | the 7 unoverridden stay at the default |
| uimv-host-send | Engine→movie calls | `call` on the movie | 93 send sites, 67 distinct names, name prepended to the args | [`menu.cpp:94-112`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L94-L112) | impl | a movie without a `call` callback silently ignores the call | nothing |
| uimv-host-respond | Delegate responses | response id | `Menu::respond` prepends the id to `respond` | [`menu.cpp:114-120`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L114-L120) | impl | — | no pending call: the extra call is harmless |
| uimv-host-init | Loader sequence | InitExtensions, SetPlatform, SetButtonMapping | exact order in `Menu::load` | [`menu.cpp:55-80`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L55-L80) | impl | — | a movie without the path skips the call |
| uimv-capture | Capture replay | — | named JSON captures can set movie variables (`apply_capture`) or answer one variable (`captured`) | [`menu.cpp:122-158`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L122-L158) | impl | **spud-only feature**: a capture can mask a missing engine system | no capture: the fallback path runs |
| uimv-tool-automation | `skytool ui` | — | open/close, key, pad, mouse, type, state, dump, get/set/call, localmap, movie, reload, screenshot_ui | [`docs/UI.md:114-157`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/UI.md#L114-L157); `apps/skytool/cmd_ui.cpp` | impl | — | — |

### G. HUD menu (`src/ui/menus_hud.cpp`)

| id | behaviour | vanilla | spud impl | file:line | evidence | deviation | default/no-op |
|---|---|---|---|---|---|---|---|
| uimv-hud-desc | HUD properties | movie HUDMenu, exactFit, depth 2, flags 0x18902, no input context | `HudMenu` with those values; `pass_through` always | [`menus_hud.cpp:161-169`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L161-L169) | impl | — | — |
| uimv-hud-types | HUD data messages | 35 message types to the HUD | `enum HudType` 1..35 with the same numbering | [`menus_hud.cpp:107-143`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L107-L143) | impl | only a subset is handled by name; the rest are silent | an unhandled type is `default: break` ([`:288-289`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L288-L289)) |
| uimv-hud-register | Component registration | RegisterComponents answers with 9 objects | `RegisterHUDComponents` stores root, hud elements, quest update, enemy meter, stealth, sneak anim, brackets, rollover name, sneak text holder, sneak text | [`menus_hud.cpp:419-435`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L419-L435) | impl | — | without it every later call is skipped |
| uimv-hud-meters | Actor-value meters | health 24, magicka 25, stamina 26, shout 0 | four `Meter` rows; percent sent on change, when forced, and every frame below 100 | [`menus_hud.cpp:851-856`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L851-L856), [`:549-563`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L549-L563) | impl | the shout meter's actor value is encoded as 0 | no movie root: skipped |
| uimv-hud-crosshair | Crosshair | 10 values: text, flags, weight, value, field | `crosshair()` maps `CrosshairTarget` to 10 args | [`menus_hud.cpp:297-308`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L297-L308) | impl | hostile sets an empty text but keeps the flags | absent numbers pass `undefined` |
| uimv-hud-compass | Compass | markers with heading, alpha, frame, scale, then the angle | `CompassTargetDataA` filled 4 numbers per marker; marker frame chosen by quest/player-set/location/discovered type; then `SetCompassAngle(yaw, yaw + north, true)` | [`menus_hud.cpp:673-720`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L673-L720) | impl | `iMapMarkerFadeStartDistance` hard-coded to 10000 with a comment that the cache holds no numeric GMST | no markers: an empty array is still sent |
| uimv-hud-markerfade | Marker fade | alpha and scale fall with squared distance | linear in squared distance, clamped at the fade radius; quest vs ordinary constants | [`menus_hud.cpp:722-745`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L722-L745) | impl | constant choices documented per GMST name | — |
| uimv-hud-notifications | Notifications queue | at most one notification per frame, after 250 ms | due time +0.25 s, `CanShowNotification` gate, objectives array of 20 pairs | [`menus_hud.cpp:593-620`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L593-L620) | impl | the strict-greater due comparison is measured, not derived | a full queue waits |
| uimv-hud-notify-dedupe | Duplicate suppression | identical text within 5 s dropped | `post_notification` compares text and time | [`menus_hud.cpp:354-362`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L354-L362) | impl | — | duplicates dropped |
| uimv-hud-skill | Skill increase | `sSkillIncreased` with the level meter | GMST printf-style format, then a queued notification with the XP fraction | [`menus_hud.cpp:310-338`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L310-L338) | impl | level threshold hard-coded 25x + 75 with a comment citing the ESM values | no player XP: `remember_xp` must be fed |
| uimv-hud-location | Location discovered | `name + sDiscoveredText`, music by marker type | type-to-sound table, queued | [`menus_hud.cpp:340-352`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L340-L352) | impl | — | — |
| uimv-hud-quest | Quest and objective notices | type 14..19 map onto one notification line | objective lines merge onto a queued line of the same quest, else any objective line | [`menus_hud.cpp:364-401`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L364-L401) | impl | — | — |
| uimv-hud-stealth | Stealth meter | fade in/out on a change, level follows the target by 0.3 per frame | `trunc(diff * 0.3)`, jump under 1, HIDDEN at 0 and DETECTED at 100 with `gotoAndStop(trunc(level * 1.01))` | [`menus_hud.cpp:565-577`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L565-L577), [`:621-636`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L621-L636) | impl | detection is absent: level is always 0 | no sneak anim object: nothing |
| uimv-hud-enemy | Enemy health | SetPercent on a new target, SetTargetPercent on change, brackets sized to the name length | `(len + 5) * 3.5` gotoAndStop, alpha 0 when absent | [`menus_hud.cpp:639-671`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L639-L671) | impl | length clamp at 50 characters | no meter object: nothing |
| uimv-hud-reset | Full reset | type 34 | clears message arrays, hides shown messages, resets modes to "All", forces meters, refreshes activate art, sets crosshair and subtitles | [`menus_hud.cpp:496-540`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L496-L540) | impl | the call order is measured from a trace | — |
| uimv-hud-markers | Floating quest markers | 48 clips named target0..47 | `attachMovie("quest target_HUD", "targetN", depth)` then `_alpha = 0` | [`menus_hud.cpp:477-494`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L477-L494) | impl | — | missing holder: nothing |
| uimv-hud-activate | Activate button art | the button bound to "Activate" | `button_name("Activate")`, undefined when unbound; boot calls respond with the event's own name | [`menus_hud.cpp:542-547`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L542-L547), [`:437-440`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L437-L440) | impl | — | unbound: undefined rather than a key name |
| uimv-hud-capture | Recorded HUD timeline | — | a `Timeline` of `{"f": frame, ...}` entries replays what the game handed the HUD | [`menus_hud.cpp:74-105`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L74-L105), [`:164-166`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L164-L166) | impl | **the HUD is a playback unless a host supplies live data**; documented as test data ([`ui.hpp:424`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.hpp#L424)) | no capture and no live source: static meters at 100 |
| uimv-hud-survival | Survival values | default objects 331-334 | type 35 posted when a value or the on/off changes | [`menus_hud.cpp:209-221`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L209-L221), [`:167-208`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L167-L208) | impl | — | no host values: mode off |
| uimv-hud-sound | UI sounds | SoundDescriptor by editor id | `ui_play_sound(name)` from the delegate | [`menus_hud.cpp:414-418`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L414-L418) | impl | — | a missing descriptor is silent |

### H. Dialogue menu ([`src/ui/menus_stats.cpp:1507-1670`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1507-L1670))

| id | behaviour | vanilla | spud impl | file:line | evidence | deviation | default/no-op |
|---|---|---|---|---|---|---|---|
| uimv-dlg-desc | Dialogue properties | movie DialogueMenu, exactFit, depth 3, flags 0x4400, Menu Mode | `DialogueMenu` with those values | [`menus_stats.cpp:1507-1511`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1507-L1511) | impl | — | — |
| uimv-dlg-show | Show | starts the conversation | `on_show` returns early when a live dialogue system drives it, else replays part 0 | [`menus_stats.cpp:1513-1522`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1513-L1522) | impl | with no live system and no recording, logs `note_missing` | nothing to replay: empty menu |
| uimv-dlg-speaker | Speaker name | SetSpeakerName | `send("SetSpeakerName", {text})` | [`menus_stats.cpp:1524`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1524) | impl | — | — |
| uimv-dlg-text | Response text | ShowDialogueText | `send("ShowDialogueText", {text})` | [`menus_stats.cpp:1525-1527`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1525-L1527) | impl | — | — |
| uimv-dlg-topics | Topic list | PopulateDialogueList then ShowDialogueList | flattened text/new/index triples, terminated by a 0, then `(refreshed, false)` | [`menus_stats.cpp:1528-1538`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1528-L1538) | impl | — | an empty row list still populates |
| uimv-dlg-voice | Voice ready | NotifyVoiceReady | `send("NotifyVoiceReady")` | [`menus_stats.cpp:1539`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1539) | impl | — | — |
| uimv-dlg-skiptext | Skip text | delegate `SkipText` | `ui_dialogue_skip()` | [`menus_stats.cpp:1591-1593`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1591-L1593) | impl | — | no live system: nothing |
| uimv-dlg-topicclick | Topic chosen | delegate `TopicClicked(index)` | live: `ui_dialogue_topic`; replay: advance to the recorded part for that index | [`menus_stats.cpp:1594-1605`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1594-L1605) | impl | replay can only follow choices the recorder made | no recording for the index: logged |
| uimv-dlg-hide | Hide with a fade | StartHideMenu, the movie fades, calls CloseMenu then FadeDone | `on_hide` sends `StartHideMenu` and returns false; `FadeDone` closes | [`menus_stats.cpp:1541-1552`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1541-L1552), [`:1606-1616`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1606-L1616) | impl | — | a movie that never calls FadeDone stays open |
| uimv-dlg-pausing | Hide under a pausing menu | a barter screen hides the dialogue movie and passes input through | `pausing_menus()` change sets hidden and inert and switches the HUD mode off and on | [`menus_stats.cpp:1554-1578`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1554-L1578) | impl | measured, per the comment | no pausing menu: unaffected |
| uimv-dlg-handoff | Hand-off to barter or training | the topic opens another menu | `m_handoff` records the menu; when it closes, the next recorded part starts | [`menus_stats.cpp:1579-1590`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1579-L1590), [`:1638-1655`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1638-L1655) | impl | **only replay can know the conversation continues** | no matching part: stays idle |
| uimv-dlg-ownfilter | Own-traffic filter | — | the recorder drops `SetButtonMapping`, `InitExtensions`, `SetPlatform`, `CurrentTime`, `StartHideMenu` | [`menus_stats.cpp:1476-1489`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1476-L1489) | impl | — | — |

### I. Inventory, container, barter, gift (`src/ui/menus_items.cpp`)

| id | behaviour | vanilla | spud impl | file:line | evidence | deviation | default/no-op |
|---|---|---|---|---|---|---|---|
| uimv-inv-desc | Menu properties | InventoryMenu exactFit depth 0 flags 0xa481; Container and Barter 0xa489 depth 0; Gift noBorder depth 3 | one `MenuDesc` per subclass | [`menus_items.cpp:473-476`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L473-L476), [`:545-548`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L545-L548), [`:593-596`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L593-L596), [`:644-647`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L644-L647) | impl | — | — |
| uimv-inv-source | Data source | the engine's item lists | `ItemSource` interface; `CapturedItems` replays JSON per menu | [`menus_items.cpp:78-162`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L78-L162) | impl | **without a host the lists are frozen recordings** | no capture: `available = false`, the menu opens empty ([`:98-105`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L98-L105)) |
| uimv-inv-request | Item actions | equip, drop, transfer, buy, sell | `ItemSource::request` returns false here; the capture logs that the game's inventory system is needed | [`menus_items.cpp:126-137`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L126-L137) | impl | **every mutating action is a no-op under replay** | nothing changes |
| uimv-inv-list | List fill | `entryList` refilled then `InvalidateListData` | clears the movie array and pushes the JSON entries converted to AS values | [`menus_items.cpp:375-387`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L375-L387) | impl | — | no list object: nothing |
| uimv-inv-select | Selection | the list's `selectedIndex` | read back from the movie, validated against the entry count, else −1 | [`menus_items.cpp:389-401`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L389-L401) | impl | — | invalid index: −1, and every per-item action is skipped |
| uimv-inv-categories | Category list | ALL, then 9 kinds, with flags; the other side's flags shifted 10 bits | `add_owner_categories` with `1023 << shift` for ALL and 2,4,8,...,512 for the kinds | [`menus_items.cpp:403-418`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L403-L418) | impl | the 10-bit shift is the game's own encoding | a missing GMST yields an empty label |
| uimv-inv-show | Show sequence | categories, items, player info, saved indices | per subclass; Inventory also sends `RestoreIndices` | [`menus_items.cpp:478-495`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L478-L495), [`:551-576`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L551-L576), [`:599-625`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L599-L625), [`:646-669`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L646-L669) | impl | — | — |
| uimv-inv-playerinfo | Player info | UpdatePlayerInfo | `send("UpdatePlayerInfo", {player_info})` or the barter variant with gold and vendor name | [`menus_items.cpp:420-427`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L420-L427), [`:626-639`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L626-L639) | impl | barter passes 4 values, gift passes none | not available: not sent |
| uimv-inv-itemcard | Item card | RequestItemCardInfo answered from the engine's card | responds with the captured card object for the selected index | [`menus_items.cpp:252-258`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L252-L258) | impl | — | no card: no response |
| uimv-inv-item3d | 3D item preview | Inventory3DManager: show, clear, rotate, zoom | `Item3D` scene slot 1/6, begun on show, model per entry from a capture | [`menus_items.cpp:241-251`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L241-L251), [`:264-278`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L264-L278), [`:556-566`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L556-L566) | impl | the model comes from a captured key `text/count/filterFlag` joined by `\|` | no model: nothing shown |
| uimv-inv-zoom | Zoom toggle | wheel or Item Zoom, with ToggleMenuFade | `ZoomItemModel` and the `Item Zoom` user event; toggling sends `ToggleMenuFade` | [`menus_items.cpp:192-208`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L192-L208), [`:264-278`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L264-L278) | impl | — | at rest at the wrong end: no toggle |
| uimv-inv-mouseequip | Mouse equip | CheckForMouseEquip answers left or right hand | `mouse_user_event(button)` mapped to RightEquip 0 / LeftEquip 1 | [`menus_items.cpp:319-328`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L319-L328) | impl | — | an unmapped button: no response |
| uimv-inv-saveindices | Saved positions | SaveIndices into globals, RestoreIndices on show | one file-static `g_inventory_indices` | [`menus_items.cpp:166-170`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L166-L170), [`:485-494`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L485-L494) | impl | **process-global state**, not per save | no saved indices: 0 |
| uimv-inv-game | Deferred inventory update | after a sound, the list refills | `game()` calls `request`, plays the sound only on success, then reloads and refills | [`menus_items.cpp:428-443`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L428-L443) | impl | under replay the refill never runs | request false: nothing |
| uimv-inv-contexts | Input contexts | Menu Mode, Item Menus, Inventory; the slider takes over | pushed at construction; the slider drops and restores them, adding Cursor on PC | [`menus_items.cpp:473-476`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L473-L476), [`:284-318`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L284-L318) | impl | — | — |
| uimv-inv-barter-gold | Barter gold warning | `sNotEnoughVendorGold` with two numbers | `format_gmst` with `%d`/`%i` | [`menus_items.cpp:48-66`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L48-L66), [`:627-639`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L627-L639) | impl | — | fewer args than formats: the rest are left out |
| uimv-inv-notify | Too-heavy notification | `sItemTooHeavy` with a cancel sound | plays the sound and logs the text; **it does not post a HUD message** | [`menus_items.cpp:444-448`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L444-L448), [`:575-576`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L575-L576) | impl | partial: the text never reaches the HUD | no HUD: nothing |

Row count: 163 (measured: `grep -c "^| uimv-"`). That is a large ledger because the topic
has three separate sub-systems (importer, interpreter, three menus); every row is a distinct behaviour and no
row merges two.

## Support matrix

All counts are measured; the command is given for each.

**AS2 / AVM1 opcodes.** `awk 'NR>=1013 && NR<=1340' src/ui/player/vm.cpp | grep -o 'case 0x[0-9A-Fa-f]*' | sort -u | wc -l`
= **88 distinct opcode values** with a `case`.

| class | count | values |
|---|---|---|
| real implementation | 84 | arithmetic, comparison, string, variable, property, target, call, function, prototype, array/object, bit, enum, timeline, GotoFrame2, Trace |
| accepted but a no-op | 4 | `0x08` ToggleQuality, `0x09` StopSounds, `0x9A` GetURL2, `0x9E` Call |
| handled only at import | 2 | `0x83` GetURL → Push+GetURL2; `0x88` ConstantPool → folded into pushes |
| **no case at all** (falls to `default`, logs `unsupported action`) | **8** | `0x24` CloneSprite, `0x25` RemoveSprite, `0x27` StartDrag, `0x28` EndDrag, `0x2A` Throw, `0x8A` WaitForFrame, `0x8D` WaitForFrame2, `0x94` With |

The four that are also warned at import are `0x8A`, `0x8D`, `0x94` and `0x8F` Try ([`ui_script.cpp:220-224`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_script.cpp#L220-L224));
`Try` is not AVM1 and the importer only warns.

Note the asymmetry: `StartDrag` and `EndDrag` do work through `MovieClip.startDrag`/`stopDrag`
([`builtins_display.cpp:1435-1445`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L1435-L1445)), so the opcode form is the only gap. `CloneSprite`/`RemoveSprite` have no
method equivalent either (`duplicateMovieClip` is a separate native, [`builtins_display.cpp:1284`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L1284)).

**Native builtins.** `rg -c "define_method" src/ui/player/builtins_display.cpp` = 159,
`builtins_core.cpp` = 17 → **176 native methods**. Plus 40 + 29 `vm.define` property sites.
Classes defined: 17 in `builtins_display.cpp` (`rg -o 'make_class\(vm[^,]*, [a-z_]+[^,]*, "[A-Za-z.]+"'` →
BitmapData, BitmapFilter, Button, Color, ColorTransform, Date, Matrix, MovieClip, MovieClipLoader, Point,
Rectangle, Sound, TextField, TextFormat, Transform, plus AsBroadcaster and LoadVars) and the core set in
`builtins_core.cpp` (Object, Function, Array, String, Number, Boolean, Math). Globals: `Key`, `Mouse`,
`Selection`, `Stage`, `System`, plus `setInterval`/`setTimeout`/`clearInterval`/`clearTimeout`/
`updateAfterEvent`/`targetPath`. Packages: `flash.external.ExternalInterface`, `flash.geom`
(`Point`, `Rectangle`, `Matrix`, `ColorTransform`), `flash.filters` (`BitmapFilter`, and
`BitmapFilterType` constants), `flash.display` (`BitmapData`).

**Stubbed builtins** (present, no effect): `Sound.attachSound/start/stop/loadSound/setVolume/setPan/setTransform`
([`builtins_display.cpp:2461-2470`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L2461-L2470)); `getFontList` returns `[]` ([`:1714`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L1714)); `MovieClip.getURL`,
`attachAudio`, `attachBitmap`, `Selection.captureFocus`, `Selection.moveFocus`, `System.setClipboard`,
`Mouse.setCursorType`, `updateAfterEvent`, `BitmapData.dispose`; `Mouse.getButtonsState` returns 0;
`Selection.getControllerMaskByFocusGroup` returns 1; `Key.isToggled`/`isAccessible` return false.

**Display-list features.**

| feature | state | evidence |
|---|---|---|
| nested clips, timelines, labels, frame actions | supported | [`player.cpp:423-450`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L423-L450) |
| masks by clip depth | supported (stencil) | [`draw.hpp:16-19`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/draw.hpp#L16-L19) |
| scale-9 grid | supported | [`player.cpp:2649`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2649) |
| `scrollRect` | supported | [`player.cpp:2490`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2490) |
| `topmostLevel`, `hitTestDisable` (GFx) | supported | [`display.hpp:76-77`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/display.hpp#L76-L77) |
| blend modes | 6 of 13 named modes + normal | [`movie_renderer.cpp:1113-1118`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1113-L1118) |
| `cacheAsBitmap` | flag only, no rasterisation | [`player.cpp:768-770`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L768-L770) |
| filters | 4 of 8 kinds | [`movie_renderer.cpp:1534-1600`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/movie_renderer.cpp#L1534-L1600) |
| buttons, clip events, drag | supported | [`builtins_display.cpp:1700-1712`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/builtins_display.cpp#L1700-L1712); [`player.cpp:1021`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L1021) |
| video, sounds in a movie | absent | `rg -c -i "sound" src/importer/convert/ui_import.cpp` = 0 |

**Text.**

| feature | state |
|---|---|
| glyph runs, kerning, letter spacing | supported (`text.cpp`) |
| word wrap, multiline, alignment, margins, indent, leading | supported ([`docs/formats/osu.md:50`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osu.md#L50)) |
| HTML subset (13 tags, 5 entities) | supported ([`text.cpp:166-266`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.cpp#L166-L266)) |
| Scaleform `textAutoSize`, `verticalAlign` | supported ([`text.cpp:754-799`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.cpp#L754-L799)) |
| inline images, `img://` | supported ([`text.hpp:68-72`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.hpp#L68-L72)) |
| input fields, caret, selection | supported ([`player.cpp:2225-2283`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.cpp#L2225-L2283)) |
| IME, clipboard, right-to-left, complex shaping | **absent**: `rg -i "bidi\|harfbuzz\|shaping\|arabic\|rtl" src/ui/player/text.cpp` returns 0 matches |
| `<textformat>` block tag | supported ([`text.cpp:255-266`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/text.cpp#L255-L266)) |

**Sound.** Movie sound is **not implemented**: the importer has no sound tag case, `StopSounds` is a no-op,
the `Sound` class is a stub. All audible UI sound comes from the engine side through `ui_play_sound` /
`ui_start_sound` / `ui_stop_sound` ([`ui.hpp:294-300`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/ui.hpp#L294-L300)); the movie asks for a descriptor by editor id through the
`PlaySound` delegate, with 13 registration sites across the 9 menu handler tables.

**Menus and the delegate tables.**

| measure | count | command |
|---|---|---|
| menu names `create_menu` can build | 36 | `rg -o 'if \(name == "[^"]+"\)' src/ui/menus*.cpp \| wc -l` |
| `handle("...")` registration sites | 153 | `rg -o 'handle\("[^"]+"' src/ui/*.cpp \| wc -l` |
| distinct delegate names | 110 | same, `\| sort -u \| wc -l` |
| `send("...")` engine→movie sites | 93 | `rg -o 'send\("[^"]+"' src/ui/*.cpp \| wc -l` |
| distinct engine→movie callback names | 67 | same, `\| sort -u \| wc -l` |
| `UiHost` virtuals | 52 | `awk 'NR>=275 && NR<=417' src/ui/ui.hpp \| grep -oE "ui_[a-z_]+\\(" \| sort -u \| wc -l` |
| `UiHost` overridden by `App` | 45 | `rg -o "App::ui_[a-z_]+\\(" src/engine/*.cpp \| sed 's/.*App:://;s/(//' \| sort -u \| wc -l` = 47, minus `ui_command` and `ui_scene_json` |
| HUD data message types | 35 declared, 15 handled by name | [`menus_hud.cpp:107-143`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L107-L143), [`:224-292`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L224-L292) |

**Three target menus, feature by feature.**

| need | HUD | Dialogue | Inventory |
|---|---|---|---|
| delegate handlers | 4 | 5 | 27 |
| host→movie calls | 16 distinct (`ShowMessage`, `ShowSubtitle`, `HideSubtitle`, `SetSubtitlesEnabled`, `SetLoadDoorInfo`, `ShowElements`, `SetLocationName`, `ShowTutorialHintText`, `SetCrosshairEnabled`, `SetCrosshairTarget`, `ValidateCrosshair`, `RefreshActivateButtonArt`, `ShowSurvivalElements`, `FlashShoutMeter`, `SetChargeMeterPercent`, `ShowArrowCount`) plus 6 `vm.call_method` targets (`SetCompassMarkers`, `SetCompassAngle`, `StartMagickaBlinking`, `StartStaminaBlinking`, `gotoAndStop`, plus the 4 meter `Set*Percent`) | 5 | 10 (`SetCategoriesList`, `InvalidateListData`, `UpdatePlayerInfo`, `RestoreIndices`, `ToggleMenuFade`, `AttemptEquip`, `DropItem`, `XButtonPress`, `UpdateItemCardInfo`, `ItemRotating`) |
| movie variables read | 11 handles + 5 frame numbers, plus `QuestNotification.AnimLetter.*` on reset | none | `Menu_mc.bPCControlsReady`, `entryList`, `selectedIndex` |
| live engine data | pose, yaw, north, sneak, 4 meters, compass markers, survival | speaker, text, topics, voice state | item list, cards, models, gold, names, categories |
| state kept in C++ | meters, notification queue, stealth level, enemy, XP | conversation part, hand-off, hidden/inert | saved indices (process global), `m_data` |

## Mudcrab mapping

Mudcrab's decision is written down: Flash `.gfx` layouts are **transpiled** to Bevy 0.19 `bsn!` nodes and
ActionScript 2.0 handlers become **Luau** (`docs/roadmap/03-luau-and-ui.md:3.4`;
`docs/roadmap/README.md` Phase 3). That is not spud's design, so the mapping below separates what transfers as
a technique from what transfers only as a specification.

State measured in Mudcrab: there is no `crates/scripting` yet (`ls crates/` = converter, dummy-content,
engine, launcher, mo2, shared), no `.swf` or `interface` handling in `crates/converter/src` (`rg -i
"swf|scaleform|controlmap"` finds only BSA name-hash tests in `archive/bsa_name.rs`), and no game UI module in
`crates/engine/src` (`settings_ui.rs` is a plain Bevy panel documented as temporary in
`docs/specs/engine/settings-ui.md:3`). So the whole lane is absent, not partial.

| ledger group | Mudcrab today | gap | fits the design how | owner |
|---|---|---|---|---|
| A. Movie import | nothing | total | A `crates/converter/src/ui/` module that reads `Skyrim - Interface.bsa` through the existing archive reader and emits movie geometry, glyphs, bitmaps and **a control-flow graph, not bytecode**. `archive/bsa_name.rs:112-126` already has the `interface\controls\...` hash pairs, so the reader is proven for this archive. | `converter` |
| A. Control map, translations, font map | nothing | total | The same importer emits `interface/controls`, `translate_<lang>`, `fontconfig` tables into SQLite, as `esm-to-sqlite.md` does for records. These are pure data and are needed by any UI design, interpreter or not. | `converter` + `shared` |
| B. AVM1 interpreter | not planned | n/a | **Do not transfer.** A transpiler needs the opcode list as a *source language* definition, not the runtime. The 88-case switch is a checklist of what the movies use; the `ui_script.cpp` decoder (branch resolution, constant-pool folding, DefineFunction2 registers) is the useful part. | — |
| B. Opcode support list | nothing | total | Adopt as a conformance table for the Luau emitter: which actions a movie contains, and therefore which control-flow shapes the emitter must handle. | `converter` (spec) |
| C. Display list | nothing | total | Maps to Bevy UI: clips → entities with `Node`, depth → `ZIndex`, matrix → `Transform` (or `UiTransform` where present), clip-depth masks → clipped nodes, scale-9 → nine-slice border drawing, `cacheAsBitmap` → no equivalent needed. I did not verify which of these Bevy 0.19 API names exist; the mapping is conceptual. | `engine` (new `ui` module) |
| C. Event order | nothing | total | The order rules (newest clip first, 3-priority queue, DoAction split around place commands) are properties of the *movie*, not of the renderer. If Mudcrab transpiles the movie into a static `bsn!` scene, this order collapses into ordinary ECS systems. Record it as a spec, not code. | `engine` (spec) |
| D. Text | Bevy `Text` only (`console.rs:552-560`) | fonts, HTML, Scaleform autosize | Transpile HTML runs into Bevy text spans. The Scaleform extensions (`textAutoSize`, `verticalAlign`) have no Bevy counterpart and need a layout pass. Font metrics must come from the converted font movies; the texture side reuses the existing DDS→KTX2 lane (`docs/specs/converters/dds-to-ktx2.md`). | `converter` + `engine` |
| E. Renderer | Bevy UI renderer | filters, blend modes | Bevy UI has no per-node filters. UI effects that the movies use are mostly drop shadows on text; those become material/text atlas work, or are dropped. This is the largest visual-fidelity risk in the transpile design. | `engine` |
| F. Menu stack and host | `console.rs` and `settings_ui.rs` only | total | The `UiHost` shape transfers well: a `Resource` of menu services with defaulted methods, one plugin per menu family. Mudcrab already uses this pattern (`AppConsoleExt::add_console_command`, `console.rs:1-14`). | `engine` |
| F. Input contexts | Bevy `InputFocus` already used (`settings_ui.rs:15-18`) | control-map contexts | Replace the `controlmap.txt` context stack with Bevy input focus plus an explicit context resource; the table itself is converted data. | `engine` |
| F. Capture replay | none | total | **Worth borrowing as a test technique.** JSON captures of the real game's delegate traffic let a menu be developed before its game system exists. Mudcrab already has a reference harness milestone for paired captures (#372, #373, milestone "Skyrim reference harness"). | `engine` + evidence |
| G. HUD | absent (`ui-hud` in `mudcrab-inventory.md:111`) | total | The HUD is the best first target: 4 delegate handlers, 16 movie calls, and it is a pure sink. A `HudPlugin` owning meters, compass and notifications as components, with a `bsn!` scene per element, proves the pipeline with the smallest surface. | `engine` |
| H. Dialogue | absent; records only (`mudcrab-inventory.md:75`) | total | Needs a dialogue *system* first (topic records, voice state). The movie side is 5 handlers, so the UI can wait for `game` to own topics. `mudcrab-inventory.md:75` records "no conversation system found". | `game` then `engine` |
| I. Inventory | absent (`game-items` in `mudcrab-inventory.md:103`) | total | The 27 handlers are all game-side actions. Spud's split — `ItemSource` for data and actions, `Menu` for presentation — is the right boundary and matches how `console.rs` separates `ConsoleRegistry` from its UI. | `game` then `engine` |
| I. Item 3D preview | glTF path exists | scene slot | A Bevy camera + render-to-texture for the item model, as `render.rs` already does for other targets. | `engine` |

What already decided in Mudcrab that constrains this:
- Bevy 0.19 with `bevy_ui` and `bevy_text` enabled (`Cargo.toml:22-28`), so `Node`, `Text` and `InputFocus`
  exist today.
- Luau through `mlua` with the `luau-jit` feature (`Cargo.toml:32`), but the crate that would host it does not
  exist yet.
- No ADR covers UI. `docs/adr/0001`-`0014` are all asset, LOD, texture, security or cutout policy. A UI ADR is
  missing and is the cheapest way to settle the transpile-vs-interpret question in writing.
- `docs/specs/converters/pipeline.md:468-500` defines the pack layout (`textures/`, `meshes/`, `scripts/`,
  `skyrim_world.db`, `cell_cache.rkyv`). A UI lane would add `interface/` to that list and must go through the
  same manifest, resume and repair rules.

## Port notes

1. **The expensive part is the engine side, not the drawing.** In spud the HUD engine logic is about 500 lines
   of per-frame rules ([`menus_hud.cpp:496-745`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L496-L745)) driving a movie that is already authored. A transpiled Bevy HUD
   has to own those rules itself. Budget accordingly: the movie is free, the behaviour is not.
2. **The movie is a state machine, not a picture.** `SetCategoriesList`, `InvalidateListData`,
   `RestoreIndices` and `PopulateDialogueList` must be called in order, with the exact argument shapes
   ([`menus_items.cpp:478-495`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_items.cpp#L478-L495), [`menus_stats.cpp:1528-1538`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_stats.cpp#L1528-L1538)). A transpiler that only extracts shapes loses this
   entirely; a transpiler that keeps scripts as Luau keeps it. This is the strongest argument for keeping the
   scripts, and the strongest risk in a "pure art" approach.
3. **Movie variables are the interface.** `Menu_mc.bPCControlsReady`, `entryList`, `selectedIndex`,
   `CompassTargetDataA`, `ObjectivesA`, `HUDModes` are read and written by name from C++. A Bevy port needs an
   equivalent named-binding layer per menu, or the transpile must resolve those names at conversion time.
4. **Constants worth knowing**: compass fade radius 10000 ([`menus_hud.cpp:722-741`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L722-L741)), marker alpha 90/40 and
   scale 100-150/90-130 (quest vs ordinary), stealth step 0.3 and `trunc(level * 1.01)`, enemy bracket
   `(len + 5) * 3.5`, notification delay 0.25 s, duplicate window 5 s, skill threshold `25 * level + 75`,
   category flags `1023 << 10`, safe-zone inset 15 px ([`menu.cpp:222-227`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menu.cpp#L222-L227)), text gutter 2 px.
5. **Two decoupling tricks are worth copying.** (a) The `PlayerHost`/`UiHost` pair with all-defaulted methods
   lets a menu exist before its game system. (b) `Menu::send` tolerates a movie with no `call` callback, so a
   movie can be tested without the engine.
6. **Minimal vertical slice** that proves the lane end to end, without any AS2 work: convert one movie
   (`hudmenu.swf`), draw its stage-1 shapes and text in a Bevy UI node at 1280x720, and compare a screenshot
   with `screenshot_ui` output from spud at frame 1. That proves the importer geometry, the coordinate space,
   the glyphs and the compositing order in one check. It needs no HUD logic, no VM and no host.
7. **Ordering constraints when converting**: shapes before glyphs (shared vertex/index buffers), fonts before
   text fields that name them, control map before any input routing, and translations before any `$KEY` text
   is laid out.
8. **Do not copy the code.** Spud is MIT and Mudcrab is GPL-3.0, so the licence permits reading, but the
   audited artefacts here are constants and orderings. Re-derive them from the movies or from measured
   captures, and cite the measured value rather than the file.

## Test ideas

Deterministic, headless, no game launch:

1. **Geometry parity per movie.** Fixture: one converted movie from `Skyrim - Interface.bsa` (e.g.
   `hudmenu.swf`). Check: the triangle count, the vertex bounding box in stage pixels and the fill count equal
   spud's `.osu` for the same source. Tolerance 0 on counts, 1e-3 on coordinates. Catches tessellation
   drift ([`ui_shapes.cpp:16-60`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/ui_shapes.cpp#L16-L60)) without needing a screenshot.
2. **Glyph advance table.** Fixture: `fonts_en.swf`. Check: for 200 code points, advance and kerning equal the
   converted table. Catches em-unit and y-down errors early.
3. **Delegate table conformance.** A pure test that the set of delegate names a transpiled menu handles equals
   the 110-name set, per menu family. Catches a dropped handler at conversion time, not at runtime.
4. **HUD rule unit tests, no movie.** Feed a pose, yaw and three markers to the compass rule and assert the
   4-tuple per marker against values computed from the formula in port note 4. This is the part that is real
   logic and it is fully testable without graphics. Mudcrab's `console.rs` already sets this precedent
   (`console.rs:1-8`: parsing, completion and scrollback are pure and tested).
5. **Dialogue ordering test.** Given a topic list and two refreshes, assert the emitted call sequence is
   `PopulateDialogueList` then `ShowDialogueList(refreshed, false)`, and that the flattened argument array is
   length `3n + 1` with a trailing 0. Catches the off-by-one that would misalign every topic.
6. **Notification queue test.** Post three notifications at t=0, assert exactly one is shown per frame after
   0.25 s, and that a duplicate text within 5 s is dropped. Deterministic with a fixed timestep.
7. **Paired capture check (later).** For each of HUD, dialogue and inventory, record delegate traffic in retail
   Skyrim SE through the #372/#373 reference harness, then replay it into the Mudcrab menu and compare the
   emitted call sequence. This is the only test that can prove vanilla parity; the harness does not exist yet.

Fixtures to extract from vanilla data: `Skyrim - Interface.bsa` movies (`hudmenu.swf`, `dialoguemenu.swf`,
`inventorymenu.swf`, `inventory components/*.swf`, `fonts_en.swf`), `interface/translate_english.txt`,
`interface/controls/pc/controlmap.txt`, `interface/fontconfig.txt`, and the GMST strings the three menus use
(`sHUDStarted`, `sHUDCompleted`, `sHUDFailed`, `sDiscoveredText`, `sSkillIncreased`, `sAllItems`, `sWeapons`,
`sApparel`, `sPotions`, `sScrolls`, `sFood`, `sIngredients`, `sBooks`, `sKeys`, `sMisc`, `sNotEnoughVendorGold`,
`sItemTooHeavy`).

## Open questions

1. Does the pinned Bevy 0.19 export a `bsn!` macro at all? The roadmap names it (`03-luau-and-ui.md:3.4`); I did
   not find a use in `crates/`. Cheapest check: `rg "bsn!"` over the Bevy 0.19 sources in the cargo registry, or the vendored
   tree. This decides whether the transpile target exists today.
2. Will Mudcrab transpile AS2 to Luau, or extract only art and rewrite every menu by hand? The roadmap says the
   former, no ADR decides it, and the two answers differ by a large multiple of effort. Port note 2 is the
   argument for keeping the scripts.
3. What is the real frame rate of the interface movies? Spud reads it from the movie header ([`player.hpp:135-140`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/player/player.hpp#L135-L140));
   the HUD's per-frame rules assume 60 Hz (the notification timing comment at [`menus_hud.cpp:596-599`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/ui/menus_hud.cpp#L596-L599)). A
   converted movie that plays at a different rate changes every animation timing constant.
4. How many of the 110 delegate names does a minimal playable game actually need? My estimate, unverified: 4
   (HUD) + 5 (dialogue) + 27 (inventory) + ~10 (main menu, fader, message box) ≈ 46. Confirming this needs a
   trace of a scripted play session, which does not exist.
5. Does spud's `.osu` capture everything needed to rebuild a movie without the interpreter? Specifically, are
   the scale-9 rects, filter chains and clip-event bindings enough, or does the Luau script still need the
   display list at runtime? Unverified; the cheapest check is to convert one movie and try to rebuild its
   first frame from the `.osu` alone.
6. Which of the four unsupported filters and seven unsupported blend modes do the Skyrim interface movies
   actually use? `rg` over converted `.osu` files would answer it, but no cache exists in this audit
   environment. This changes the renderer cost estimate materially.





