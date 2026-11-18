# Texture audit

The texture audit finds every surface of a converted pack that cannot reach the renderer with its texture. The renderer never fails a draw for this: a material with no base-colour image draws flat white, a primitive with no UVs samples one texel, and a terrain layer with no diffuse draws Bevy's stand-in image. The audit and a baseline make those cases visible and keep their count from growing.

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

Retail behaviour is not counted: an entry that targets NullTextureSet hides the shape, and an empty TXST slot stays empty (RF-TEX-004, RF-TEX-005).

## Running it

```sh
python3 scripts/workspace.py cargo run --locked --profile quick -p converter --bin texture-audit -- \
  --pack <pack> --data <Skyrim Data> --scratch <empty folder> --json report.json --csv report.csv \
  --baseline crates/converter/tests/fixtures/texture-audit/full-pack-baseline.json
```

The pack and the game data are only read. Source NIFs are extracted to `--scratch`. Each finding has the mesh, the shape, the slot, the authored path, where it came from, whether the source holds it, and the cells that place the mesh.

The same check runs as tests. `crates/converter/tests/texture_audit.rs` audits a small synthetic pack in CI. The ignored test `full_pack_stays_within_its_baseline` audits a real pack (`MUDCRAB_TEXTURE_AUDIT_PACK`, `MUDCRAB_GAME_DATA`). A cause above its baseline fails the test. When a cause is fixed, lower its value in the baseline file; never raise one.

## In the engine

`texture_fallback` logs each fallback once at `warn` level with its kind, model and slot, and keeps a count. It covers lighting materials with no base-colour image, textured primitives with no UVs, and terrain layers with no diffuse image.
