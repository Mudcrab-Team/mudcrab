# Texture audit

The texture audit finds every surface of a converted pack that cannot reach the renderer with its texture. The renderer never fails a draw for this: a material with no base-colour image draws flat white, a primitive with no UVs skips its base-colour sample (Bevy 0.19) and draws the base colour, and a terrain layer with no diffuse draws Bevy's stand-in image. The audit and a baseline make those cases visible and keep their count from growing.

## What it checks

For every GLB in the pack it reads the source NIF's authored texture paths, then asks three questions: does the game data (archives and loose files) hold the `.dds`, does the pack hold the converted `.ktx2`, and does the GLB reference it. GLBs with no source NIF (composed or generated models) are checked from their own contents. It also checks `TXST` and `WATR` paths, terrain layers, and the published `base_texture_swaps`. Path handling uses the converter's own functions (`canonical_asset_path`, `resolve_asset_uri`, `runtime_texture_source`).

| Cause | Meaning |
| --- | --- |
| `source_texture_absent` | The authored path names no file in the game data. |
| `converted_texture_missing` | The game data has the `.dds`; the pack has no `.ktx2`. |
| `slot_unreferenced` | The pack has the `.ktx2`; the GLB does not reference it. |
| `unparseable_path` | The path cannot be turned into a texture key. |
| `primitive_without_uvs` | A textured primitive has no `TEXCOORD_0`. |
| `texture_set_unresolved` | A TXST or WATR path names no converted texture. |
| `land_texture_unresolved` | A terrain layer has no diffuse image. |
| `override_target_unresolved` | A MODS entry targets a texture set no plugin defines. |
| `binding_ambiguous` | The texture exists, but no single GLB material that a primitive uses can be tied to the NIF shape, so the audit cannot say the slot is bound. |

Retail behaviour is not counted: an entry that targets NullTextureSet hides the shape, and an empty TXST slot stays empty (RF-TEX-004, RF-TEX-005).

## How a slot resolves

A slot resolves only when the GLB binds it and the exact file the engine requests exists in the pack. The audit follows the primitive that draws the shape: it takes the one material that a primitive uses and that carries the shape's NIF shape block, and reads the image that material binds for the slot. Diffuse is the base-colour texture, normal is `normalTexture`, glow is `emissiveTexture`, the `_s` slot is the specular colour texture, and every other slot is an entry of `OPEN_SKYRIM_material.textureSlots` found by its NIF slot index. A material that no primitive uses proves nothing. When no used material, or several, carry the shape block, the audit does not guess: the slot is reported as `binding_ambiguous` and is never cleared by some other image of the GLB. An image that is listed but unbound does not count either. The audit checks the exact URI target, not just the base name: a GLB that binds `stone.opensky-srgb.ktx2` fails when only `stone.ktx2` exists. It accepts the replacement the converter publishes in `texturePathRepairs` (the volcanic grass normals) only through that unique material. Record paths (`TXST`, `WATR`) go through `shared::texture_aliases::converted_record_texture_path`, the function the engine uses, so a path the engine refuses (for example a `.` segment) is reported, not accepted. An image URI the resolver refuses is reported as `unparseable_path`. Path handling shares the converter's own functions, so a bug inside `canonical_asset_path` would not show here.

Game data is selected the way the converter selects it: an archive owned by an installed plugin that is not active is left out, loose `Meshes`/`Textures` folders match without regard to case, and a selected archive that cannot be listed is recorded.

## Running it

```sh
python3 scripts/workspace.py cargo run --locked --profile quick -p converter --bin texture-audit -- \
  --pack <pack> --data <Skyrim Data> --scratch <empty folder> --json report.json --csv report.csv \
  --baseline crates/converter/tests/fixtures/texture-audit/full-pack-baseline.json --strict
```

The pack and the game data are only read. The pack is held under its shared asset lock from the first read to the last, and `--scratch`, `--json`, `--csv` and `--write-baseline` must lie outside the pack and the game data. Source NIFs are extracted to `--scratch`. Each finding has the mesh, the shape, the slot, the authored path, where it came from, whether the source holds it, and the cells that place the mesh.

An audit is incomplete when a mesh or archive could not be read, no NIF slot or texture set was read, or a record could not be decoded. Findings are missing then, so an incomplete audit never passes a baseline. `--mesh-filter` audits part of the pack and refuses `--baseline` and `--write-baseline`.

The baseline is a ceiling that only goes down. A cause above its value fails. With `--strict`, and in the full-pack test, a cause below its value also fails and names the value to lower it to, so a later regression cannot hide under the old ceiling. `--write-baseline` refuses to write over an existing baseline when a count would rise.

The same checks run as tests. `crates/converter/tests/texture_audit.rs` audits a small synthetic pack and a small world database in CI, one case per cause. The ignored test `full_pack_stays_within_its_baseline` audits a real pack (`MUDCRAB_TEXTURE_AUDIT_PACK`, `MUDCRAB_GAME_DATA`). When a cause is fixed, lower its value in the baseline file; never raise one.

## In the engine

`texture_fallback` logs each fallback candidate once at `warn` level with its kind, model and slot, and keeps a count. It covers lighting materials with no base-colour image, textured primitives with no UVs, and terrain layers with no diffuse image. They are candidates: the hook sees the model's own material, and a texture swap that a placed reference applies later can supply an image or hide the shape.
