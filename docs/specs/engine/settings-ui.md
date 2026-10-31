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
- Color grading: Off, Saturation, Full (legacy), Approved sRGB, Snapshot linear and Record sRGB, with live saturation, brightness, contrast, pivot, tint amount and tint RGB. Defaults preserve the approved Sleeping Giant sRGB appearance and Cool Gray sky/fog. Stage-dependent rows dim while inactive. Reflections stay ungraded. Console commands synchronize the menu. See the integration contract below for domains, persistence, launch precedence and capture isolation.
- Status HUD and diagnostic asset fallbacks for subsequent loads. Existing failed instances keep their current fallback state. Collision outlines remain a session-only toggle available in the Graphics tab and the console.

The panel lists room culling as unavailable until #188 supplies an implementation. HDR scene composition stays enabled. Renderer backend and adapter information are displayed; backend/device capabilities require renderer initialization. Authored material colors, texture tiling, and per-water factors remain asset data.

## Streaming

The UI exposes the full-detail and unload radii; all four supported terrain LOD distance settings; cell commit count/time budgets; cell unload, model spawn, and render-asset upload budgets; and IO-thread count. LOD distance changes refresh queries at the current camera position. The unload radius must cover the full-detail radius.

IO threads and the visual profile ([visual-profiles.md](visual-profiles.md)) are fixed at startup. Changing either requires Save and a restart; other settings apply during the session, with diagnostic fallbacks taking effect on subsequent loads. Numerical UI limits bound interactive edits. A zero budget is unlimited only where the label says so.

## Saved preferences and automation

Save writes a validated JSON file through a temporary file and rename. Reset defaults applies defaults to the session; Save persists that reset. Read or write failures appear in the menu. Invalid saved files are ignored without changing launch configuration.

A field that an explicit launch option sets is not written from the running value. Save keeps the value the file held before that run, or the menu default when no valid file existed. The running settings keep the launch value, and the file stays valid for a later run without the option. An explicit option still wins on the next launch that passes it. Other fields save as shown in the menu.

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

`python3 scripts/workspace.py cargo run --profile quick -p engine --example settings_menu` opens the actual menu over an asset-free scene using the real sky, fog, water, and renderer systems. It does not load or save personal preferences.

For a bounded capture of all three tabs:

```sh
MUDCRAB_UI_PREVIEW_DIR=/tmp/mudcrab-ui python3 scripts/workspace.py cargo run --profile quick -p engine --example settings_menu
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
| Record sRGB | Loaded IMGS CNAM/TNAM in sRGB post pass | sRGB post pass |

Fresh interactive preferences and Reset defaults select Approved sRGB and Cool
Gray: saturation 1.66, brightness 1.38, contrast 1.50, tint amount 0.56 toward RGB
(0.88, 0.81, 0.72), pivot 0.18. Cool Gray retains #257's quarter-density blue-white
fog. The menu's sky/fog preset does not discard custom grading values.

All six modes, preset, tuning and pivot persist in the existing validated JSON.
The override mask records only fields explicitly changed by the user. Unedited
rows display the resolved source values, including authored values outside the
menu's edit range; loading those values does not turn them into overrides.
Console SceneGrade and ScenePreset select the canonical mode/preset grade and
synchronize the menu and tuning. Rejected commands leave source and settings
unchanged. Reset clears the override mask and restores the approved sRGB / Cool
Gray defaults.

Startup precedence is field-specific. Valid INI values and explicit CLI options
overlay saved preferences for their corresponding fields. `--color-grade`
owns the active mode and clears saved numeric overrides for that launch, so the
selected source supplies canonical values. `--visual-preset` owns only the
preset and keeps saved mode and tuning. Explicit `--grade-preset`, `--tone-map`
and `--tone-map-params` sources protect the active mode and numeric overrides
from stale saved values. An image-space selector pins authored source data but
does not select Record sRGB by itself. A previously saved Record sRGB mode uses
that source only when selection is explicit; without a selection it reports
unavailability and uses the approved default. The grade source and tuning
resolve before the first visible camera frame.

Save preserves the stored grade mode and override values for fields owned by
launch sources, while saving unrelated menu changes. It does not write the
current CLI/source-derived grade into preferences. Legacy Vivid is available
with `--visual-preset vivid --color-grade full`.

The Bevy tone-map menu selection flows through the shared resolver. Explicit
`--tone-map retail-curve` owns camera tonemapping (`None`) and keeps its loaded
HNAM White and curve parameters across unrelated menu edits; the Bevy tone-map
choice remains available on the Tony path. Exposure still applies to scene and
reflection cameras, while reflections remain ungraded.

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

At commit `52266688`, 1,054 engine-library CPU tests passed with 1 ignored,
strict engine/shared Clippy passed for all targets and features with `-D warnings`,
and workspace formatting passed. Native UI, GPU behavior, visual parity, gameplay
and performance were not exercised. CI on the published head remains a merge gate.
