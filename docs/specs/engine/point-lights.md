# Mudcrab Point Lights from `LIGH` References

How the runtime turns a streamed Skyrim light reference into a Bevy point light, how bright it is, and
how many of them are enabled at once. The code is `crates/engine/src/lights.rs` (the conversion, the
scale and the budget), `crates/engine/src/world/database.rs` (the row it is built from) and
`crates/engine/src/streaming.rs` (where the light is spawned).

Lights are **opt-in**: nothing in this document happens unless the engine is run with `--lights`.

---

## 1. What is read

The cell query joins `lights` to the reference's canonical winning base record. It reads radius,
RGB, flags, FNAM fade/status and DATA falloff. `references.radius_override` retains the signed raw
XRDS offset; `radius_status` distinguishes absent (0), finite (1), and malformed/non-finite (2).
Despite the historical column name, XRDS is **not an absolute replacement radius**:

```text
authored_radius = LIGH.DATA.radius + REFR.XRDS (absent offset = 0)
backend_range = min(authored_radius, 8192), if authored_radius > 0
```

Negative offsets are valid. Zero offset preserves the base radius. A nonpositive sum emits no
light. An invalid present offset is rejected instead of silently using the base radius. The range
limit applies to both base-only and offset radii; it does not overwrite the authored result.

The optional `reference_light_data` table projects XLIG size and its four floats: FOV offset, fade
offset, end-distance cap and shadow-depth bias. Valid shapes are 16/20 bytes. Missing XLIG has no
row; invalid sizes retain a row with NULL fields, and non-finite floats become NULL individually.
The canonical archive retains original bytes. These are raw inputs, **not effective multipliers**.

All optional columns/tables are probed once per connection. Older packages still load; missing
FNAM uses unit fade with a `legacy_fade` disposition. Missing reference projections cannot establish
that the source lacked XLIG. Reconvert into a separate pack to obtain them; never repair a registered
pack in place. No schema-version or asset-producer bump: these additive projections follow #278.

## 2. How a row becomes a light

`point_light(row, radius_override)` returns `Some(PointLight)` for a record the engine may light the
world with, and `None` otherwise:

- `range` is the effective radius in Creation units. This engine renders one Creation unit as one
  Bevy world unit, so no conversion is applied.
- `color` is the record's colour, read as sRGB like every other colour of the game.
- `intensity` comes from unit-fade backend conversion times finite, nonnegative FNAM (§4).
- `shadow_maps_enabled` is **false**: one cube map per light is unaffordable at 64 lights. Skyrim
  does have shadow-casting lights (the record's shadow flags); honouring them is left for later.
- the area `radius` stays 0, so there is no oversized specular highlight.

`streaming::spawn_cell` places the light as a **child of its reference**, which is what makes it sit
where the reference is, follow a render-origin rebase with the rest of the cell, and be despawned
when the cell unloads. A reference whose base record is a `LIGH` often has no model at all, so the
reference also gets a `Visibility` component: without it Bevy warns (B0004) and the light child can
never become visible.

## 3. Flags that place no light

`LIGH` `DATA` carries a flag word. Two bits make the engine place nothing (UESP, "Skyrim Mod:Mod File
Format/LIGH"):

| Bit | Name | Why it is skipped |
| :--- | :--- | :--- |
| `0x0000_0004` | negative | the light removes light; this engine cannot subtract a clustered light |
| `0x0000_0020` | off by default | the record is dark until a script turns it on, which the runtime does not model |

Neither bit is set by any `LIGH` record in the shipped plugins, so skipping them cannot take a light
out of the game's own world; they exist for mods and for scripts.

## 4. Evaluation and bounded Bevy compatibility transform

`evaluate_light` retains the raw `LightRow`, signed offset, authored sum, backend range, effective
base fade and dispositions as a `LightEvaluation` component on the reference. Immediate and deferred
spawning consume the same evaluated `PointLight`. `LightInputCoverage` counts dispositions once per
spawn, including rejected/zero-light results; reloads count again. Debug logs report cumulative counts.
Source identity remains the containing reference's form/base IDs, linked to the converter's existing
records/formid map. This does not add missing field-level contributor history to old packages.

Finite nonnegative FNAM scales luminous power linearly, including valid zero. Missing, malformed,
non-finite and negative FNAM use unit fade with an explicit legacy/invalid disposition; `fade_status`
keeps absent, valid and invalid distinct. This is a **compatibility adapter**, supported by Light
Placer's direct base-fade -> NiLight fade assignment, not instruction-backed retail evaluation of all
reference/controller inputs. In particular, XLIG's final fade equation is unresolved. Neither
`FNAM * raw_XLIG_fade` nor an inferred `1.35 + raw_XLIG_fade` is implemented.

Transform identity: `bevy-point-legacy-normalization-v1`:

```text
legacy_radiance = 12000 / (PI * 2.8) * 10  (frozen b5259d33 input normalization)
reference_distance = min(backend_range / 2, 256)
unit_fade_lumens = 4 * PI^2 * legacy_radiance * reference_distance^2 / (225/256)
intensity = min(unit_fade_lumens * effective_base_fade, f32::MAX)
```

The frozen coefficient preserves this checkout's baseline scale; it is not a new dimming control or
physical unit calibration. It no longer reads `AMBIENT_ILLUMINANCE`, `SUN_ILLUMINANCE` or environment
state. **The inherited unmeasured normalization remains a legacy fallback.** This slice cannot claim
to resolve the observed house overbrightness, close #264/#364, or establish a Skyrim rendered ratio.
The earlier ambient change multiplied configured inputs by 8.526157665637252; that is not a visual ratio.

Bevy divides lumens by `4*PI` and its diffuse BRDF supplies `1/PI`. Its attenuation remains
`window(d)/d²`, with `window(d) = max(1-(d/range)^4,0)^2`. The 256-unit anchor and 8192-unit range
cap are inherited backend safeguards, not native semantics. The formula matches its stated target
at half range only at ranges <=512; it does not match Skyrim's whole attenuation curve. Brightness
above `f32::MAX` is explicitly backend-limited; raw fade/radius remain available for diagnosis.

DATA falloff is retained and counted unsupported rather than applied as an invented inverse-square
exponent. xEdit hides it on ordinary non-shadow point lights; Community Shaders' regular attenuation
uses `1 - saturate(d/r)^2` without this field. Neither source establishes every retail shadow mode.
XLIG is retained and counted unsupported even when its fields are zero. Malformed XLIG is counted
separately. No global ambient policy, interior environment resolver, raw-colour Skyrim profile,
shadow, flicker, pulse or camera/exposure change is included.

## 5. Evidence and remaining gates

Pinned upstream sources (format/community implementation evidence, not a retail measurement):

- xEdit [`cce1c42403b279957e1e78bfe38100b29f39f72c`, wbDefinitionsCommon.pas,
  `wbREFRRadiusToStr`, lines 5404-5453](https://github.com/TES5Edit/TES5Edit/blob/cce1c42403b279957e1e78bfe38100b29f39f72c/Core/wbDefinitionsCommon.pas#L5404):
  adds signed XRDS to base radius for display/native values; editing subtracts base radius.
- Same revision, [`wbDefinitionsTES5.pas`, REFR/XLIG](https://github.com/TES5Edit/TES5Edit/blob/cce1c42403b279957e1e78bfe38100b29f39f72c/Core/wbDefinitionsTES5.pas):
  four float layout, optional tail, and `Fade 1.35+/-` label. The label does not prove an equation.
- [CommonLibSSE-NG `b93280e832f263dbef44e44cbe2936622a02f91a`, TESObjectLIGH.h](https://github.com/CharmedBaryon/CommonLibSSE-NG/blob/b93280e832f263dbef44e44cbe2936622a02f91a/include/RE/T/TESObjectLIGH.h):
  separate base fade and DATA falloff; its ExtraLightData.h describes reference floats, not evaluation.
- [Light Placer `6b7c05894de17cb3e7f0b5169e82833c9244e1ed`, LightData.cpp](https://github.com/powerof3/LightPlacer/blob/6b7c05894de17cb3e7f0b5169e82833c9244e1ed/src/LightData.cpp):
  `GetFade` selects base fade; `UpdateLight` assigns scaled fade to NiLight. This mod bypasses the
  native placed-reference construction, so it does not establish XLIG precedence or composition.
- [Community Shaders `e5ae2d0082469f92ac08dda89d230dc2d9328ba0`, Attenuation.hlsli](https://github.com/community-shaders/skyrim-community-shaders/blob/e5ae2d0082469f92ac08dda89d230dc2d9328ba0/features/Light%20Limit%20Fix/Shaders/LightLimitFix/Attenuation.hlsli):
  ordinary versus inverse-square response. `LightLimitFix.cpp/CreatePointLight` calls the native
  constructor before reading runtime data; it does not recover the native XLIG equation.

Required next evidence: version/hash-scoped native `CreatePointLight` dataflow and controlled radius,
FNAM, XLIG and falloff probes; independent attenuation/unit measurements; fixed-camera interior
comparison after the separate environment resolver lands. Tests prove source sensitivity, storage,
explicit fallback/limit behavior and the actual CPU consumer, not native rendering, performance or
Skyrim parity. The frozen normalization must be replaced only with that evidence.

## 6. The budget: 64 enabled lights

`LightsPlugin` runs one system, `budget_lights`, in `PostUpdate` after transforms are propagated. It
enables the `ENABLED_LIGHT_BUDGET = 64` lights nearest the camera and sets every other `SkyrimLight`
to `Visibility::Hidden`. Hidden is the switch that matters: Bevy extracts a light to the render world
only while it is visible. A `PointLight` without the `SkyrimLight` marker is never ranked, so the
budget cannot touch a light that a fixture or a future engine feature spawns.

The choice is cached in a resource and re-made when:

- the camera has moved more than `BUDGET_RECHOOSE_DISTANCE = 256.0` units, or
- the **set** of spawned lights has changed - a light that is not in the set the last choice was made
  from, or a count that no longer matches, which covers a cell streaming in or out.

The set is what is remembered, not the count: a cell that unloads as another streams in with the same
number of lights is invisible to a count, and a room walked into and stopped in would stay dark until
the player moved another 256 units. Re-choosing is a sort over every spawned light, so it is not done
every frame.

## 7. The `--lights` flag

`EngineConfig::lights` (`crates/engine/src/config.rs`) is false by default and set by `--lights`.
`LightsPlugin` is registered for every run - it owns the budget, not the spawning - but
`streaming::spawn_cell` places a light only while the flag is on. A default run therefore renders
exactly as it did before this feature, and the acceptance, profiling and benchmark baselines that
`scripts/phase2-*.ps1` take do not move.

## 8. Scope limits

Native attenuation/units, XLIG evaluation, shadow modes, cone/near clip, flicker/pulse and full raw
colour/exposure handling remain open under #133/#264/#364. The legacy normalization is visible and
bounded, not qualified as a native solution. Interior environment selection is separately owned.
