# Interactive settings and controls

Open **Settings / Controls** in the upper-right corner, or press **Escape**. The plain Bevy UI is temporary; rendering preferences and their consumers are independent of its appearance.

The menu pauses virtual time, releases the cursor, and clears pending movement input; an airborne player keeps their velocity and jump state and resumes the arc after Resume. Escape or Resume closes it. During gameplay, F3 releases the cursor without opening the menu; left click captures it again. Cursor capture returns on the following frame so a closing click or key cannot reach gameplay. A pause that existed before opening the menu stays paused. The developer console and menu cannot open over each other.

Buttons support mouse clicks and Tab / Shift+Tab navigation with Enter / Space activation. Left / Right adjust the focused setting, or switch tabs when a tab has focus. Only the current page participates in keyboard navigation; focused buttons scroll into view. Scroll with the wheel, Page Up / Page Down, or Home / End; a scrollbar shows position when a page overflows.

Settings are grouped under headings (for example Point lights, Sun shadows, Water, Per-frame budgets). Toggles read as On / Off switches. A row whose value differs from the defaults shows an accent bar at its start, and a setting whose controlling option is off (for example the light budget while point lights are off) is dimmed but stays adjustable. The footer shows the status line, with Save as the primary action.

## Graphics

- Point lights, enabled-light budget, point-light shadows and shadow-map resolution. Enabling lights also creates them in already loaded cells; disabling them hides those lights. Authored invalid, negative, and initially off lights retain their existing exclusion rules.
- Sun lighting, illuminance and RGB color; ambient brightness and RGB color.
- Sun shadows, shadow-map resolution, cascade count and distance multiplier. The underlying shadow range follows the full-detail unload grid.
- Sky dome, sky-brightness multiplier, exterior distance fog and fog-density multiplier. Interior cells retain their existing sky/fog rules.
- Water reflections and reflection multiplier; waves, flow normals, wave scale, speed and strength. The reflection multiplier layers over each water's converted Fresnel and reflectivity values.
- CPU frustum culling, GPU indirect drawing, GPU occlusion culling and depth prepass. Enabling occlusion enables its depth-prepass and indirect-drawing dependencies; disabling either dependency disables occlusion.
- MSAA, exposure, tone mapping, vertical FOV, VSync, window mode and windowed resolution. Walking sprint adds its existing FOV bonus to the chosen base FOV.
- Color grading: Off, Saturation only, Full (legacy), Approved sRGB and Snapshot linear, with live saturation, brightness, contrast, pivot, tint amount and tint RGB. Defaults preserve the approved Sleeping Giant sRGB appearance and Cool Gray sky/fog. Stage-dependent rows dim while inactive. Reflections stay ungraded. Console commands synchronize the menu. See the integration contract below for domains, persistence, launch precedence and capture isolation.
- Status HUD and diagnostic asset fallbacks for subsequent loads. Existing failed instances keep their current fallback state. Collision outlines remain a session-only toggle available in the Graphics tab and the console.

The panel lists room culling as unavailable until #188 supplies an implementation. HDR scene composition stays enabled. Renderer backend and adapter information are displayed; backend/device capabilities require renderer initialization. Authored material colors, texture tiling, and per-water factors remain asset data.

## Streaming

The UI exposes the full-detail and unload radii; all four supported terrain LOD distance settings; cell commit count/time budgets; cell unload, model spawn, and render-asset upload budgets; and IO-thread count. LOD distance changes refresh queries at the current camera position. The unload radius must cover the full-detail radius.

IO threads are created at startup. Changing that value requires Save and a restart; other settings apply during the session, with diagnostic fallbacks taking effect on subsequent loads. Numerical UI limits bound interactive edits. A zero budget is unlimited only where the label says so.

## Saved preferences and automation

Save writes a validated JSON file through a temporary file and rename. Reset defaults applies defaults to the session; Save persists that reset. Read or write failures appear in the menu. Invalid saved files are ignored without changing launch configuration.

| Platform | File |
| --- | --- |
| macOS | `~/Library/Application Support/Mudcrab/settings.json` |
| Windows | `%APPDATA%/Mudcrab/settings.json` |
| Linux | `$XDG_CONFIG_HOME/Mudcrab/settings.json`, or `~/.config/Mudcrab/settings.json` |

Preferences load only for interactive world play. Explicit CLI options and valid, supported INI keys override saved values for those settings. Benchmark, headless, fixture, automated flight, acceptance screenshot, and shots runs keep their existing configuration and do not install the menu or load preferences. Their launch options remain useful for automation.

Startup-only inputs such as asset paths, worldspace/start cell, backend selection, fixture selection, and capture/profiling outputs are not runtime rendering preferences. The menu adds no engine launch flags.

## Controls

The Controls tab lists menu/cursor keys, console entry and completion, mouse look, movement, NOCLIP/WALK switching, flying/jumping, slow walking/fast flight, sprint, collision outlines, debug tankards, and console help. The HUD reports movement, sprint, speed, collision status, and blocked-walk reasons without repeating their bindings.

## Native preview

`cargo run --profile quick -p engine --example settings_menu` opens the actual menu over an asset-free scene using the real sky, fog, water, and renderer systems. It does not load or save personal preferences.

For a bounded capture of all three tabs:

```sh
MUDCRAB_UI_PREVIEW_DIR=/tmp/mudcrab-ui cargo run --profile quick -p engine --example settings_menu
```

The preview saves `graphics.png`, `streaming.png`, and `controls.png`, then exits after the final screenshot completes. This verifies the native UI/render path; it is not a gameplay or performance acceptance run.

## Approved grading integration

PR #262 incorporates the complete #257 head (including review fix 5db0b05), and
also requires the settings foundation in #229. Keep #257 as the grading/capture
owner; merge both foundations before integrating the menu. No grading code needs
to be copied back into #257.

| Mode | Saturation | Brightness, contrast and tint |
| --- | --- | --- |
| Off | Identity | Disabled |
| Saturation only | Bevy linear post-tonemap | Disabled |
| Full (legacy) | Bevy linear post-tonemap | sRGB post pass |
| Approved sRGB | sRGB post pass | sRGB post pass |
| Snapshot linear | Linear post pass | Linear post pass |

Fresh interactive preferences and Reset defaults select Approved sRGB and Cool
Gray: saturation 1.66, brightness 1.38, contrast 1.50, tint amount 0.56 toward RGB
(0.88, 0.81, 0.72), pivot 0.18. Cool Gray retains #257's quarter-density blue-white
fog. The menu's sky/fog preset does not discard custom grading values.

All five modes, preset, tuning and pivot persist in the existing validated JSON.
Previously saved explicit values remain player preferences. Console SceneGrade
and ScenePreset select the canonical mode/preset grade and synchronize the menu
and tuning; rejected commands leave them unchanged. Explicit launch mode selects
its canonical grade even when preferences exist, and explicit preset wins over
the saved preset. Legacy Vivid is available with --visual-preset vivid
--color-grade full.

Interactive tuning exists only in interactive world runs (and the asset-free
preview). Hidden-window, shots, acceptance, benchmark and fixture runs do not load
preferences or create tuning. They retain #257's mode/preset resolution and receipt
contracts, including the original 1.15 brightness for snapshot-linear. The
cinematic shader, capture script, comparison fixtures and review corrections are
unchanged from #257. Reflections remain HDR and ungraded; tuning affects scene
views only.

Default sun and ambient colors now derive from WeatherLighting instead of the
settings foundation's earlier white/blue approximations. The fog-density slider
scales atmospheric extinction and inscattering while retaining the preset,
including its quarter-density Cool Gray baseline.

Integration checks in the Linux editing environment: 10 comparison tests and
2 capture tests pass; Python compileall and Rust syntax parsing pass. Added Rust
regressions cover all-mode console synchronization, reset to approved appearance,
custom tuning and reflection exclusions, saved settings round trips, explicit
launch precedence and automated-run isolation. cargo fmt --all -- --check passes. cargo test -p engine --lib settings was
attempted but stopped in wayland-sys because pkg-config/Wayland development
dependencies are missing; installing system packages is unavailable in this
container. Rust tests, clippy and engine builds remain unverified.
Native Metal, grading-section UI visibility, converted-scene appearance,
gameplay and performance have not been verified on this integration.
