# Q10 NPC templates: ACBS template flags and how a template chain resolves (#661)

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace (Ghidra 12.2 + decompiler).

## Data
TESActorBaseData (TESNPC `+0x30`): ACBS flags `+0x08`, template flags (ACBS "template data flags") `+0x1A` (uint16), template form (TPLT) `+0x30`. Bit 15 of the template flags is a runtime "template applied" mark (`0x1401dca70`, set by `0x1401dca80`). `0x1401dcaf0(baseData, bit)` tests one template flag.

## Flag bits and what each copies (`0x1403c1e30`, TESNPC's TESActorBaseData vtable slot +0x20, "apply template")
Bit names follow xEdit; the copied fields are from the trace.
| bit | xEdit name | copied from the template |
|---|---|---|
| 0 | Use Traits | race, voice type, skin/model data, height and weight, head parts and face data, ACBS Female (0x1) and Opposite Gender Anims (0x80000), and further appearance fields |
| 1 | Use Stats | class, level (or PC level mult: ACBS 0x80), calc min/max level, speed mult, magicka/stamina offsets, skills block, health offset, ACBS 0x40000 |
| 2 | Use Factions | faction list, crime faction |
| 3 | Use Spell List | spell list, perks |
| 4 | Use AI Data | AI data, combat style, one more AI field |
| 5 | Use AI Packages | package list |
| 6 | Use Model/Animation | not tested in this function |
| 7 | Use Base Data | full name, short name, ACBS flags in mask 0x10A84A |
| 8 | Use Inventory | container (inventory), default outfit, sleep outfit |
| 9 | Use Script | tested elsewhere (`0x1409bc600`, script binding); not here |
| 10 | Use Def Pack List | default package list and its override lists |
| 11 | Use Attack Data | attack data, only when the template's attack data differs from its race's default |
| 12 | Use Keywords | keywords |
Fields whose bit is clear stay as authored on the NPC itself. high (bit-to-field mapping as traced; field names partly inferred from layout).

## Which template is used
1. If the caller passes an NPC (the leveled pick for a placed reference), that NPC is the template.
2. Else, if TPLT is an NPC, that NPC. Before copying, if that NPC has its own template and is not yet applied (bit 15 clear), it is applied first, recursively. So a chain NPC -> NPC -> NPC resolves depth-first and every level contributes only through the flags at its own level.
3. Else (no TPLT, or TPLT is a leveled list with no pick yet): a built-in placeholder NPC (`0x1431e0458`, created once with default race/class forms) supplies the flagged groups.
- If the NPC is Unique (ACBS 0x20) and its chain ends in a leveled list, the Unique flag is cleared.
- Afterwards the NPC stores the first non-NPC form in its chain (the leveled list, if any) and the AND of all template flags along the chain (`0x1401dd260`, `0x1401dcb30`, `0x1401dcb50`), and sets bit 15.

## Placed reference whose base has a leveled template (`0x1401dcba0`; see Q9 RF-LVL-008)
For an ACHR whose base NPC has TPLT = LVLN, at reference creation:
1. Resolve the LVLN with the reference's level (Q9) -> picked form. If it is not an NPC, the reference is marked (vtable +0x118) and nothing is generated.
2. Create a new NPC of the base's type, copy the placed base into it (vtable +0x178), and call "apply template" on it with the picked NPC (step 1 of the list above).
3. Store the original base and the pick on the reference (`0x1403016e0`, the leveled-creature extra data), and set the new NPC as the reference's base (vtable +0x420). Then inventory, outfit and 3D are initialised for it.
So the final NPC = the placed base's own fields, overwritten group by group (per the base's template flags) by the picked NPC, which itself was resolved through its own template chain.

## Consequence for #661
A reference cannot take its body (traits) from the base NPC alone when the base's template is a leveled list: until a reference resolves it, the base carries placeholder traits. Resolving the LVLN per reference (Q9) and applying the flagged groups (this note) is needed for those 8,191 references.

## Other engines
- spud / OpenSky: not checked for templates in this pass.
