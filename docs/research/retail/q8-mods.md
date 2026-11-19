# Q8 Alternate textures (MODS) applied to a loaded model (#508)

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace (Ghidra 12.2 decompiler). Record counts (marked "maintainer data") come from a read-only query of the game records done for #508; they are not executable facts.

## Data structure
TESModelTextureSwap (vtable `0x1417fb3c0`): alternate-texture array at `+0x28`, count at `+0x30`. Each entry is 0x18 bytes: TXST pointer `+0x00`, 3D Index (int32) `+0x08`, 3D Name (BSFixedString) `+0x10` (copy constructor `0x1401ed240`).

## Where it is applied
TESObjectREFR 3D load (`0x1402e9ee0`): after the model loads, if the base form's model component is a TESModelTextureSwap, it calls `0x1401ed800(swap, root node)`.

## Answers
**(3) Multi-entry lists** (`0x1401ed800`): the engine builds a map keyed by **3D Index**. Entries with index -1 are skipped. If two entries have the same index, the later entry replaces the earlier one. Then one depth-first traversal applies every entry; it stops early when the map is empty. All entries of a list apply, each to its own shape. high.

**(2) 3D Name and 3D Index** (`0x1401ee0e0`): matching uses **only the 3D Index**. The traversal visits the root's children in array order, recursing into nodes (pre-order, depth first). A running counter starts at 0 and increases by one for every geometry leaf (NiObject "AsGeometry" vtable slot +0x38 non-null) in that order, whether or not it receives a swap. A geometry whose counter equals an entry's 3D Index receives that entry. The **3D Name (including the ":N" suffix) is not read** by the apply path. Its ":N" suffix is therefore editor data with no runtime effect on this path; the suffix/index mismatches (1,504 of 2,476 entries, maintainer data) do not matter at runtime. high (apply path), medium ("not used anywhere": other readers of `+0x10` were not searched exhaustively).

**(1) TXST slots** (`0x1401ee0e0` -> `0x14032d720`):
- Only shapes whose shader property is **BSLightingShaderProperty** (NiRTTI `0x1433d49d8`) or **BSGrassShaderProperty** (`0x1433d4a10`) are swapped. Other shader types (effect, water, sky ...) are skipped but still counted.
- The engine builds a **new BSShaderTextureSet with 9 slots** from the TXST: for each slot 0..8 whose TXST path is non-empty, slot = "Data\Textures\" + path. **Empty TXST slots stay empty in the new set. They do not keep the model's texture.** The new set replaces the material's whole texture set (material `+0x78`); a shared material is cloned first (`0x1414e8000`). Then the material reloads its textures and the property is re-initialised (`0x141519980`, `0x141519430`, `0x1415195d0`). high.
- Special case: if the entry's TXST is the engine's **NullTextureSet** (singleton created at `0x14032dc60`, editor ID "NullTextureSet", form ID set to 0x28 at creation; form ID reading is medium), the shape is **hidden** (NiAVObject flags `+0xF4` bit 0, app-culled) instead of retextured. Lead: some MODS entries whose TXST does not resolve may reference this form; check their FormIDs for 0x28.

## Consequences for #508
- Drop the "exactly one entry", "index 0", exact-name and diffuse-only gates: none is a retail rule.
- Implement: per placed ref, count geometry leaves depth-first in child order; apply entry k to geometry #k; build the texture set from the TXST alone (empty slot = no texture, so the shader default for that slot applies, e.g. no normal map); last entry wins on duplicate indices; NullTextureSet hides the shape; skip non-lighting/grass shader shapes but keep counting them.
- The converted NIF/GLB must preserve the retail geometry order (depth-first child order) for the index to match. This is a converter requirement, not verified here.

## Other engines
- spud / OpenSky: not checked for MODS in this pass.
