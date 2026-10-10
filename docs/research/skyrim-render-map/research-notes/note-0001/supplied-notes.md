# Skyrim Special Edition: Vanilla Lighting Technical Notes

Scope: vanilla Skyrim SE, interior and exterior lighting data as exposed by the Creation Kit (CK) and the CELL record. Mods are excluded.

## Confidence tags

- **[Sourced]**: stated on a UESP/CK wiki page or in the Bethesda tutorial, read in full from text the user pasted.
- **[Sourced, other game]**: from Fallout 3, Fallout 4, or Source documentation. Probably applies to Skyrim, unconfirmed.
- **[Inference]**: my reasoning from sourced facts. No source states it.
- **[Unverified]**: forum posts or memory. Do not rely on it.

Sources read in full: UESP Skyrim Mod File Format CELL page, Skyrim CK Lighting Template page, CK Cell Lighting page, Bethesda Tutorial "Lights and FX". Earlier search snippets (Fallout 3/4 wikis, a forum thread) are tagged where used.

The runtime renderer is still not documented. Sections 7 and 8 mark what remains open.

---

## 1. Lighting data hierarchy

**Interior cells** take lighting from three places, in order:

1. **Lighting Template** (a standalone WorldData record, shared between cells). [Sourced] Created and edited under World > Lighting Templates in the CK.
2. **Cell lighting block** (XCLL on the CELL record). [Sourced] Holds the cell's own values. Each field can inherit from the template through inherit flags (section 3).
3. **Light and FX references** placed in the cell. [Sourced] See section 5.

**Exterior cells** take sky and weather lighting from the worldspace and its weather records. [Sourced] The Cell Lighting page says "Show Sky" makes an interior behave like an exterior, using the weather of a chosen Region. The weather record format is not covered here.

## 2. Cell record flags and lighting-related fields

[Sourced] CELL DATA flags relevant to lighting and sky:

| Flag | Value | Meaning |
|---|---|---|
| Interior | 0x0001 | Interior cell |
| Has Water | 0x0002 | Water present |
| Show Sky | 0x0080 | Sky and weather enabled, as in an exterior |
| Use Sky Lighting | 0x0100 | Weather lighting affects the cell |

[Sourced] Other lighting-related fields:

- **LTMP** (formID): the lighting template used by the cell.
- **XCLW** (float): non-ocean water height. Special values: 0x7F7FFFFF means no water.
- **XCLC**: exterior grid location. Exteriors only.
- **XCMO, XCIM, XCAS**: music, image space, and acoustic space for the cell. Image space (post-processing) is separate from the light data.

## 3. Cell lighting block (XCLL), 92 bytes

[Sourced] Field list in the order the UESP page gives. The page marks some entries as uncertain.

| # | Field | Notes |
|---|---|---|
| 1 | rgb Ambient | Base ambient color |
| 2 | rgb Directional | Directional light color |
| 3 | rgb Fog Near (color) | Fog color near the player |
| 4 | float Fog Near | Distance where fog begins |
| 5 | float Fog Far | Distance where fog reaches its far value |
| 6 | int Rotation XY | Directional light angle, horizontal |
| 7 | int Rotation Z | Directional light angle, vertical |
| 8 | float Directional Fade | Directional light fade |
| 9 | float Fog Clip Dist | Clipping distance |
| 10 | float Fog Pow | Fog curve |
| 11–16 | rgb Ambient X+, X-, Y+, Y-, Z+, Z- | The six-axis ambient cube |
| 17 | rgb Specular Color | [Sourced as "assumed from LGTM", not shown in CK] |
| 18 | float Fresnel Power | [Sourced as "assumed from LGTM", usually 1, not shown in CK] |
| 19 | rgb Fog Far (color) | Fog color far from the player |
| 20 | float Fog Max | Maximum fog contribution |
| 21 | float Light Fade Distances Start | Omni light fade start |
| 22 | float Light Fade Distances End | Omni light fade end |
| 23 | uint32 Inherit flags | See below |

[Inference] The 92-byte size fits a layout of 11 color entries at 4 bytes each (44 bytes) plus 12 four-byte scalars (48 bytes). That matches entries 1–3, 11–17, and 19 as colors, and the rest as scalars. The per-field byte offsets are still unverified.

[Sourced] Note: the UESP page says one NavMeshGenCell duplicate stores this field at 64 bytes.

**Inherit flags** (uint32), each bit inherits one group from the Lighting Template:

| Bit | Group |
|---|---|
| 0x0001 | Ambient Color |
| 0x0002 | Directional Color |
| 0x0004 | Fog Color |
| 0x0008 | Fog Near |
| 0x0010 | Fog Far |
| 0x0020 | Directional Rotation |
| 0x0040 | Directional Fade |
| 0x0080 | Clip Distance |
| 0x0100 | Fog Power |
| 0x0200 | Fog Max |
| 0x0400 | Light Fade Distances |

[Sourced] The ambient cube (X+ through Z-) does not appear in this flag list. The page does not say whether it inherits from the template. The CK's Directional Ambient tab is the only documented way to set it.

## 4. Lighting Template fields

[Sourced] From the CK Lighting Template page:

- **Ambient**: [Sourced] Only feeds the "Set From Ambient" button, which fills the six directional ambient colors. It has no direct effect on the lighting. The page also says this value sets the "light level" the game uses for sneak detection.
- **Fog Near / Far (color)**: fog color.
- **Fog Near**: distance where fog starts.
- **Fog Far**: distance where fog maxes out. If Fog Max is below 1, full fog is reached sooner.
- **Fog Pow**: fog curve, between 0 and 1. Lower values make fog appear sooner.
- **Fog Max**: maximum amount fog color can cover the scene. 1 means 100%. The page recommends about 0.7.
- **Clip Dist**: distance at which geometry is clipped.
- **Directional (color)**: [Sourced] "used to simulate bounced light" and affects the whole cell.
- **Rotation**: angle of the directional light.
- **Fade**: [Sourced] described as "the brightness of the light". Note this is a different description from the cell field name "Directional Fade".
- **Directional Ambient X+/-, Y+/-, Z+/-**: six colors, set by hand or with Set From Ambient.
- **Light Fade Distances Start / End**: [Sourced] Omni lights start fading at Start and are fully faded at End. **No effect on shadow casters.**
- **Specular**: [Sourced] "does nothing. It was never fully implemented."
- **Fresnel Power**: [Sourced] "does nothing. It was never fully implemented."

[Sourced] Additional notes from the page:
- Setting RGB to 0, 0, 0 can turn lighting off in some spots, even with Night Eye. Avoid pure black templates.

## 5. Cell Lighting tab: behavior notes

[Sourced] From the CK Cell Lighting page:

- **Template dropdown**: selects a template. **Inherit** checkboxes override individual template fields and enable editing. Choosing template NONE lets you set every value by hand.
- **Fog Max**: [Sourced] the Cell Lighting tab says "Not used." The Lighting Template page describes Max as a real control. This is a conflict between the two pages. Test in the CK before relying on either.
- **Clip Distance**: interior clipping plane.
- **Show Sky** and **Region**: sky and weather from the chosen Region.
- **Use Sky Lighting**: whether weather lighting affects the cell.
- **Light Fade Distances**: [Sourced] If Start is 0, the game uses the INI value `fLightLODStartFade` from the Display section. If End is 0, it uses `fLightLODStartFade` plus `fLightLODRange`. Both INI key names come from the page.

## 6. Local and FX lights (from the Bethesda tutorial)

The tutorial describes four light types in the CK, all under WorldObjects > Light:

| Type | Behavior | Shadows |
|---|---|---|
| Omni | Sphere of light from the pivot | None. Cheapest to render. Light passes through objects. |
| Shadow-casting omni | Same as omni, with shadows | Yes. Purple axis marker. |
| Spotlight | Single-direction cone | Always shadow-casting. "Used frugally." |
| Hemispherical | Behaves like an omni, but casts shadows from one side of the pivot | Yes, one side. Red half-sphere marker. |

[Sourced] Shadow-casting lights: "Shadows generate additional polygons and only four can be visible at any given time." This is the tutorial's wording and is not an engine spec. It also matches the forum claim from the earlier search, which is now partly sourced. The exact rule (per object or per screen) is not stated.

[Sourced] Other tutorial points:
- Shadow rendering must be enabled in the CK preferences (Shaders tab) to preview shadows.
- Shadow-caster striping is fixed by adjusting the light's Depth Bias (Alt+B). Too much bias makes shadows appear to float.
- A seam on shadow-casting omni lights is fixed by rotating the light.
- Non-shadow lights tint FX objects (for example, mist) near them.
- FX objects near shadow-casting lights inherit the cell's directional and ambient light, but not the shadow light.
- **External emittance**: [Sourced] An FX object can be told to take the color of a chosen light (an "FX" light, such as FXLightRegionSunlightWhite), overriding other light. The tutorial warns this blocks other light on the object, including spells and torches, so use it only around shadow casters.
- Glow Fills use external emittance to mimic haze around a light.

## 7. Templates, image spaces, and room-level lighting

[Sourced] From the tutorial:
- Changing a cell's template changes fog and contrast for the whole cell. Example templates named: MineTemplateClose, AzurasStarTemplate, BleakFallsBarrowClose, BleakFallsBarrowMedium, BleakFallsBarrowFar. Close templates suit small rooms. Far templates suit large rooms.
- Templates can be set per room with the Set lighting template batch action on room markers.
- Image space (post-processing, such as depth of field or desaturation) is set on the cell's Common Data tab. The tutorial's common choice is DefaultImageSpaceDungeon. Rooms can have their own image space under the room reference's Bound Data tab.

## 8. Open questions

Still not documented in any source I read:

- **Runtime blend** of the six-axis ambient cube (weights, normalization, per-vertex or per-pixel). [Unverified]
- **Specular and cubemap path**. Specular and Fresnel are documented as unimplemented. Where static reflection cubemaps come from is not covered. [Unverified]
- **Whether the renderer is forward or deferred**. [Unverified] A forum thread calls Skyrim DX9 with some DX11 features. It says nothing about shading.
- **Exact shadow-caster limit rule** (per object, per screen, or per frame) and shadow map configuration. [Unverified] The INI keys `fShadowDistance` and `iShadowMapResolution` from my earlier notes are from memory and not checked.
- **Weather record format** and the exact way exterior lighting is composed from weather and sky data.
- **Whether the ambient cube inherits** from the template. The flag list does not include it. [Unverified]
- **Fog Max conflict** between the Lighting Template and Cell Lighting pages.
- **Byte offsets and types** for each XCLL field, beyond the ordered list above.

The tutorial is dated 2015 and the wiki pages were edited in 2025 and 2026. Keep that in mind when treating the CK behavior as current.

## Sources

- UESP Skyrim Mod File Format, CELL: https://content2.uesp.net/wiki/Skyrim_Mod:Mod_File_Format/CELL (read from user-pasted text)
- Skyrim CK wiki, Lighting Template: https://skyrimck.uesp.net/wiki/Lighting_Template (read from user-pasted text)
- CK wiki, Cell Lighting: https://ck.uesp.net/wiki/Cell_Lighting (read from user-pasted text)
- Bethesda Tutorial, Lights and FX: https://ck.uesp.net/wiki/Bethesda_Tutorial_Lights_and_FX (read from user-pasted text)
- Fallout 4 CK wiki, Light: https://falloutck.uesp.net/wiki/Light (search snippet only; Fallout 4)
- Fallout 3 GECK wiki, Lighting Template: https://geck.uesp.net/wiki/Lighting_Template (search snippet only; Fallout 3)
- PCGH forum thread (weak, mod-related): https://extreme.pcgameshardware.de/threads/enb-0-123beta-fuer-skyrim-deferred-rendering-und-reflections.247707/
