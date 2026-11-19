# Q9 Leveled lists (LVLI, LVLN, LVSP): resolution, timing, random source (#661, clothing part 2)

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace (Ghidra 12.2 + decompiler). GMST values: Skyrim.esm wins over executable defaults.

## Data layout (TESLeveledList, vtable `0x1417faaa8`)
Entries `+0x08` (0x18 bytes each: form `+0x00`, count `+0x08` (int16), level `+0x0A` (uint16), extra data `+0x10`), sorted by level ascending on insert (`0x1401e8a60`). Chance none byte `+0x10` (LVLD), flags `+0x11` (LVLF), entry count `+0x12`, chance-none global `+0x20` (LVLG).
- Chance none (vtable slot +0x20, `0x1401e8880`): if LVLG is set, the global's float value truncated to int; else LVLD. high.
- Flags: 0x01 "calculate from all levels <= PC level", 0x02 "calculate for each item in count" (slot +0x28, `0x1401e88f0`), 0x04 "use all", 0x08 "special loot". high.
- Level difference limit (slot +0x30): LVLI iLevItemLevelDifferenceMax, LVLN iLevCharLevelDifferenceMax (`0x140272110`, `0x140271820`); both 0 in the executable (no ESM override found).

## One pick (`0x1401e8d20`)
```
1. c = chanceNone; if c > 0: if (rand() % 100) < c: return nothing
2. L = requested level                                    (special-loot lists: see below)
   if "clamp to player" argument: L = min(L, player level)
3. allLevels = (flags & 0x01 and mode == 0) or mode == 1
   minLevel  = allLevels ? (diff != 0 ? L - diff : no lower bound) : L
4. Walk entries in level order up to the last entry with level <= L:
   - not allLevels: the candidates are all entries at the highest level <= L
   - allLevels: all entries with level <= L (and >= the first level group reaching minLevel)
5. pick = candidates[ rand() % candidateCount ]           (uniform among candidates)
6. if pick is itself a leveled list (LVLN 0x2C, LVLI 0x35, LVSP 0x52) and recursion is on:
   resolve it with the same L; count = entry count * nested count
```
Mode 2 (an argument used by some callers) lets the first level group above L stand in when nothing is <= L; medium.
Special loot (flag 0x08): L = (u ^ fSpecialLootWeighting) * (max - min) + min with u uniform in [0,1), min = fSpecialLootMinPCLevelMult*PC + fSpecialLootMinZoneLevelMult*L (+ bases), max = fSpecialLootMaxPCLevelMult*PC + fSpecialLootMaxZoneLevelMult*L (+ bases). Skyrim.esm: Weighting 4.0, MinPC 0.6, MinZone 0.4, MaxZone 1.0 (exe MaxPC 1.0, bases 0). high (form), medium (which base pairs with which term).

## Whole list (`0x1401e90f0`, args: level, count, out list, mode, recurse)
- "calculate for each item" (0x02): `count` independent picks (each with its own chance-none roll).
- else "use all" (0x04): ONE chance-none roll for the list; if it passes, every entry is added with its count (nested lists resolve recursively, each with its own chance none).
- else: one pick; the result gets `count` (times the entry count).
- With allLevels, the level passed down is min(level, highest entry level).

## Random source
One global Mersenne Twister MT19937 (`0x14219e810`; constants 0x6C078965 in the seeding routine `0x140ce4460`, 0x9908B0DF in `0x140ce44e0`). If not yet seeded, it seeds itself from `_time64()` on first use. All picks use `rand() % N`. **Results are not reproducible between runs.** A stable per-FormID seed (as in clothing part 2) is a Mudcrab design choice, not retail behaviour. high.

## Levels used
- **Reference calc level** (`0x140300b90`): the encounter zone's level if the reference has a zone (zone `+0x44` holds the level once computed), else the player's level. Optional jitter: a global counter steps by +1 and wraps from iCalcLevelAdjustUp to -iCalcLevelAdjustDown; **Skyrim.esm sets both to 0, so there is no jitter.** Result >= 1.
- **Encounter zone level** (`0x14028f9e0`, ECZN DATA min `+0x31`, flags `+0x32`, max `+0x33`): if PC < min: min (or PC if flag 0x02 "match PC below minimum"); else min + (PC - min) * fLevelScalingMult (Skyrim.esm 1.0, so PC level); capped at max when max != 0. The zone stores the result (`+0x44`), so later calls reuse it. high (formula), medium (when the stored value is cleared, e.g. zone reset).
- **Leveled actors** (`0x1401e9530`): level = reference calc level (with jitter argument) x the reference's level modifier (extra data 0x1E: XLCM; GMSTs fLeveledActorMult Easy 0.33 / Medium 0.67 / Hard 1.0 / VeryHard 1.25 in Skyrim.esm; none = 1.0), rounded down, min 1. The extra also supplies the mode argument. No clamp to player level. medium-high (mapping of the extra to the GMSTs is inferred).
- **Containers** (`0x14022d340`): each LVLI entry resolves with the owning actor's own level if the container is an actor, else the reference calc level (with jitter). Count = |entry count|.
- **Outfits** (`0x14023fa80`): each LVLI in the outfit resolves with the actor's level, count 1.
- **Death items** (`0x140672b90`): the actor's death LVLI with the actor's level, count 1.

## When it rolls
- **Leveled actors (ACHR whose base is LVLN)**: `0x1401dcba0` resolves the list and, if it yields an NPC, makes a generated base for the reference. It is called from the reference init path (`0x1402dec90`) only when the reference is not already resolved: `0x1403016b0` returns true when the base is already an NPC with the leveled-creature extra data, and a second check (`0x140618150`) can also skip it. Other callers: `0x140363e30` (actor creation), `0x1403c26c0`, `0x140a47180` (PlaceAtMe). So a placed leveled actor rolls once when first created; the result stays with the reference (and is carried by the save through the reference's changed base). After a cell reset the actor is created again and rolls again (inferred from the "already resolved" gate; the reset path itself was not traced). medium-high.
- **Container inventory**: on inventory initialisation (`0x14022d340`, via `0x1402cf9d0`/`0x1402d2cb0`), i.e. first load and inventory reset; not on every load.

## Consequences
- #661: placed refs with an LVLN base must run this pick (per reference, actor level from the zone/player and XLCM) to get an NPC; the pick is not stable across runs in retail.
- Clothing part 2: outfit LVLI entries resolve with the actor's level using the rules above (highest level group <= level unless flag 0x01; chance none per pick). The stable seed is acceptable as a Mudcrab choice but must be labelled as such.

## Other engines
- spud / OpenSky: not checked for leveled lists in this pass.
