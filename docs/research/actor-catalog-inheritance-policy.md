# Actor catalog inheritance policy

This note records the evidence boundaries behind the converter's actor catalog. It describes a catalog projection; it does not claim to reproduce undocumented engine copy logic.

## Use Traits

The resolver reads the Traits flag as ACBS `u16` offset 18, bit 0 (`0x0001`). It follows NPC `TPLT` links only while that bit is set, with a visited set and no fixed depth limit. It uses the final NPC's race, sex, skin override, and FaceGen identity. A missing, zero, deleted, wrong-type, cyclic, or `LVLN` target leaves one unresolved row for the original NPC, with no parts or head.

The Creation Kit describes Use Traits as using the ActorBase's Traits tab, and that tab identifies race, sex, and skin. xEdit's TES5 definition exposes the template bit and links, but does not provide a field-to-flag `SetDontShow` mapping. CommonLib declares `CopyFromTemplateForms`, but its implementation is not public. Therefore, per-field inheritance here follows CK documentation, not decompiled copy logic.

## Skin fallback

The catalog selects the appearance-root NPC's `WNAM` when it resolves to a live `ARMO`; otherwise it uses the effective race's `WNAM`. An unusable explicit override emits a diagnostic. This fallback is conservative catalog policy inferred from the Oldrim reverse-engineering note that `GetSkin` checks the actor base and race. It is not proof of Skyrim engine fallback behavior.

## FaceGen identity

For a resolved Traits chain, FaceGen paths use the appearance root's stable identity: defining plugin and plugin-local FormID. Ordinary non-templated overrides keep the existing defining-owner path rule; they do not switch to the winning plugin's folder. If Traits resolution fails, the catalog emits no fabricated FaceGen path.

This projection is supported by Mutagen's non-template FaceGen asset-link condition and CommonLib's `GetRootFaceNPC` name. Neither source proves which NPC Skyrim copies into or uses to build a FaceGen asset path. Treat templated FaceGen ownership as conservative catalog policy, not engine copy-body proof.

## QNAM handoff for #391

QNAM inheritance through Traits remains unproved. Future tone support must suppress a templated NPC's own placeholder QNAM whenever Traits is enabled, preserve a valid authored `(0, 0, 0)` for a non-template NPC, and inherit QNAM from the resolved root only after stronger proof. This change adds no tone columns and does not port #391. Keep any future effective-Traits reuse simple; do not add speculative API fields.

## Armor race selection

An ARMA matches the effective race when its primary `RNAM` or a resolvable, nonzero additional-race `MODL` names that race. Compatible entries retain authored ARMO/MODL order; the existing `DefaultRace` tier follows them. This does not model same-slot `DNAM` priority or RACE `armorParentRace`.

## Pinned evidence

- xEdit TES5 definitions, commit [`9fb016884bec138ea6c7b872cec831537d464c3e`](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas): Traits flag at lines 1305-1323; template flags, `TPLT`, `WNAM`, and `QNAM` at lines 8346-8455; ARMA `RNAM` and additional-race `MODL` at lines 4096 and 4128; RACE `WNAM` and armor-race `RNAM` at lines 9307 and 9498. `wbDefinitionsCommon.pas` lines 8429-8448 defines shared flags. TES5 has no `SetDontShow(wbActorTemplate...)` call sites.
- Mutagen Bethesda, commit [`d051530e3c254b897a6acabaada8e1ecc92e0321`](https://github.com/Mutagen-Modding/Mutagen/tree/d051530e3c254b897a6acabaada8e1ecc92e0321): `Npc.cs` lines 50-60 gates FaceGen asset links on non-template Traits; `NpcConfiguration.cs` lines 42 and 48 names Traits and ModelAnimation flags.
- CommonLibSSE `powerof3@7b7d0fc9`: `TESActorBaseData.h` lines 46-63, 74, and 136 documents base flags, template flags, and `TPLT`; `TESNPC.h` lines 229, 276, and 293 declares `CopyFromTemplateForms`, `faceNPC`, and QNAM body tint; `TESNPC.cpp` lines 163-184 has `GetRace` and `GetRootFaceNPC`; `BGSSkinForm.h` line 23 identifies skin as WNAM; `TESObjectARMA.cpp` lines 13-46 implements primary/additional-race compatibility; `TESObjectARMO.cpp` lines 12-34 covers armor-addon selection; `TESRace.h` line 344 declares `armorParentRace`. Public source does not include the template-copy implementation.
- CommonLibSSE-NG `b93280e8`, `TESActorBaseData.h` lines 128-129: template forms.
- SKSE64 `ianpatt@25b72352`, `GameObjects.h` line 772 and `GameObjects.cpp` lines 55-99: `nextTemplate`, `GetRootTemplate`, and root-derived weight in `GetNodeName`.
- Creation Kit wiki snapshots: [Template Data](https://ck.uesp.net/w/index.php?oldid=16591), [Traits Tab](https://ck.uesp.net/w/index.php?oldid=16827), [Character Gen Parts Tab](https://ck.uesp.net/w/index.php?oldid=23631), [LeveledCharacter](https://ck.uesp.net/w/index.php?oldid=12728), [ArmorAddon](https://ck.uesp.net/w/index.php?oldid=6760), and [Armor](https://ck.uesp.net/w/index.php?oldid=6750).
- UESP NPC format snapshot, [`oldid=3650076`](https://en.uesp.net/w/index.php?title=Skyrim_Mod:Mod_File_Format/NPC_&oldid=3650076): template flags at wikitext lines 47-60 and QNAM at line 252.
- Oldrim reverse-engineering, `DavidJCobb/skyrim-classic-re` commit `d217bef5`, `Forms/BaseForms/TESNPC.h` line 93: `GetSkin` note says it checks actor base and race. `_DOCS/ActorWeightData/Subroutine0046E4E0.txt` lines 32-67 documents DNAM slot priority. These are inference inputs, not Skyrim SE engine proof.
- RaceMenu `expired6978/SKSE64Plugins` commit `9ebcb733`, `skee64/TintMaskInterface.cpp` lines 49-52 and 908-922: runtime base color read and a template walk for hair color.
