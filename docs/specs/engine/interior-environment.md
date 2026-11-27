# Authored interior environment (#264 / #364 / #480)

`EnvironmentCatalog` resolves CELL, XCLL, LTMP, LGTM, XCIM and room-marker inputs into one
`SceneEnvironment` (`crates/engine/src/world/environment/interior.rs`). `ActiveSpace.interior`
selects the cell. The selection and the apply step run in PostUpdate, after `DoorCrossingSet`
and before transform propagation, so the lighting changes in the same frame as `ActiveSpace`.

Sources: registry rows RF-LIGHT-CPU-001 to RF-LIGHT-CPU-016, RF-LIGHT-GPU-001 to
RF-LIGHT-GPU-010, RF-IMGS-001 and RF-WATER-011, and the INTLIGHT verify report (static trace of
retail build 1.7.104.0). Both are private maintainer research notes and are not in this
repository; ask in #264 for a row. This spec states each fact it relies on.

## The rule

1. **Sky mode** (RF-LIGHT-CPU-010). No cell or an exterior: 3. An interior (DATA bit `0x1`)
   without `0x80`: 1. `0x80` without `0x100`: 2. `0x80` and `0x100`: 3. `0x100` without `0x80`
   is mode 1. In the shipped data only `0xF2CAB LabyrinthianMaze` is an interior in mode 3
   (RF-LIGHT-CPU-015). A database without the cell flags gives mode 1 (bit `0x1` only).
2. **Mode 3** keeps the exterior path: the #588 clock sun, the weather colours and the sun
   shadows. Nothing below applies.
3. **Modes 1 and 2: resolve the lighting.** Per XCLL field, a set inherit bit takes the LGTM value
   when the template exists, else the cell value (RF-LIGHT-CPU-014). Bits: `0x1` ambient and
   DALC, `0x2` directional, `0x4` fog colours, `0x8` fog near, `0x10` fog far, `0x20` rotation,
   `0x40` directional fade, `0x80` clip, `0x100` fog power, `0x200` fog max, `0x400` light fade.
   The template DALC is its separate subrecord, not DATA. A cell without XCLL reads no bit, and
   every field applies as 0, as every retail getter returns 0. A database without the
   `cell_lighting` table (an older pack) has no authored lighting at all, so nothing below applies
   and the built-in lights stay; this differs from an authored cell without XCLL. `xcll_size` is used only as "the
   cell has XCLL"; all 786 shipped interiors have 92 bytes.
4. **Room templates** (RF-LIGHT-CPU-013). When the camera is inside a room marker box
   (`room_bounds`, first match in `ref_id` order) whose LNAM template exists, that template
   supplies the colour source: directional, DALC and fog colours. The rotation and the fog
   distances stay the cell's. Retail follows the current room through the portal system and
   blends. Mudcrab uses the camera position and switches at once. The record-to-extra-0x77 link
   is a medium-confidence name lead.
5. **Key light**: the same `DirectionalLight` entity as the exterior sun (retail uses one light
   node, RF-LIGHT-CPU-011).
   - Colour `Color::srgb_u8(directional)`, the same conversion as the exterior sun.
   - Illuminance `PI * AMBIENT_ILLUMINANCE * SunlightScale(XCIM)` (RF-LIGHT-GPU-006: retail
     `DirLightColor = colour x fade 1.0 x SunlightScale`). Alvor's house: 6428.6 lux. An interior
     on the weather image space, or with a missing image space, uses `SUNLIGHT_SCALE`; this
     fallback is a Mudcrab decision.
   - Direction (RF-LIGHT-CPU-012): angles converted as unsigned integers, Creation travel
     `(cos a cos b, -sin a cos b, sin b)`, engine travel `(cos a cos b, sin b, sin a cos b)`.
   - No exposure change (RF-LIGHT-GPU-004, RF-IMGS-003). The directional fade is never applied
     (RF-LIGHT-CPU-008).
   - Shadows OFF. **Mudcrab interim decision.** Retail is UNRESOLVED (RF-LIGHT-GPU-010: the light
     is wired as the shadow light with `fInteriorShadowDistance` 3000, and one interior gate is
     not decoded). The exterior cascades would let the roof and floor shadow the
     room. Their range is the exterior `fShadowDistance`; the interior distance is open (#857).
   - `crate::sun::update_sun` skips the sun while `ExteriorSunState` holds it.
6. **Ambient**: `GlobalAmbientLight.color` is the mean of the six DALC colours, with the same
   averaging as `WeatherLighting::mean_ambient`. Brightness stays `AMBIENT_ILLUMINANCE`. XCLL
   ambient is not a Lighting-shader input (RF-LIGHT-GPU-002) and is reported only. Under `skyrim`
   the near meshes read the whole DALC cube by normal instead, from the raw record bytes
   (`visual-profiles.md`, #1010).
7. **Fog**: Bevy linear fog from the resolved near and far and the near colour. Fog max is a cap:
   Bevy multiplies its ramp by the colour alpha, so the engine sets alpha = max and ends the ramp
   at `near + max * (far - near)`, which gives `min(ramp, max)`. Max 0 draws no fog.
   Retail scene fog is `min(saturate((d - near)/(far - near))^power, max)` with a near-to-far
   colour lerp (RF-WATER-011). Bevy has no power term and one colour, so power and the far
   colour are reported only. `far <= near` draws no fog. The near/far clamps and the clip
   distance are not applied.
   A short XCLL (72 to 91 bytes) has no fog max field. Retail behaviour for a short block is
   unverified (RF-LIGHT-CPU-014 covers a cell without XCLL only). Mudcrab keeps the authored fog
   distances and applies no cap. No shipped interior has a short XCLL, but six take fog
   distances from a short (72-byte) lighting template that has no max, and now get that fog. An
   authored max of 0 still draws no fog.
8. **Exit**: leaving modes 1 and 2 restores the saved rotation, colour, illuminance, shadows and
   ambient, and the clock sun takes over the next frame. A write by another system while indoors
   (`apply_render_settings` on a preference change), including one in the exit frame itself,
   replaces the saved value, so the exit returns the current setting.
9. **Order**: `crate::sky`'s `apply_fog` runs after `EnvironmentSet`, so a door or room crossing
   changes the fog in the same frame as the light.

Point lights are on by default (`--no-lights` turns them off) and use retail's falloff and unit
([point-lights.md](point-lights.md)). A stored light fade of 0/0 does not mean "no fade": a
zero begin or end is replaced by the INI values, so 0/0 reads as 3500/4000 and the distance
dimmer applies (NEEDS 20; see section 4 of [point-lights.md](point-lights.md)).

## Reported only

`SceneEnvironment` (also in the live console `environment` field) reports the resolved fields
with their source per field, the missing fields, the AmbientCube (RF-LIGHT-CPU-002,
RF-LIGHT-GPU-003), the light fade squares, the image space selection (RF-IMGS-001), Show Sky,
Use Sky Lighting, the sky mode, and the applied values (`cell_render`).

## Open items

- Interior directional shadows (RF-LIGHT-GPU-010).
- The Show Sky dome indoors, worldspace templates (`WRLD` `0x20`, Blackreach), the reference
  image space override (RF-IMGS-001 extra `0x77`), and fog power and far colour.
- **UNVERIFIED: the world sign of the key light.** RF-LIGHT-CPU-012 settles the local rotation
  product (high confidence) and that the light travels along column 0 of the world rotation. The
  world sign (medium-high) also assumes an identity parent rotation, as RF-SUN-002 does for the
  exterior sun. No rendered interior has been compared with retail. It matters most for
  rotation Z = 90 (383 of the 786 shipped interiors after template inheritance): the traced
  light travels straight up, so floors get no key light and ceilings get all of it. If the sign
  is wrong, those cells are lit from the wrong side. The look checks are in the PR: Alvor's
  house, one Z = 90 cell and the Bleak Falls Barrow room switch.
- No headless interior start exists: shots skip interiors and `coc` refuses them (#727).
