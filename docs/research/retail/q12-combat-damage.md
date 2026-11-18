# Q12 Combat damage: weapon damage, sneak and critical multipliers, armour, block, difficulty (C3, C2)

Build: SkyrimSE.exe 1.7.104.0, SHA-256 846efccf…1402f. Static trace (Ghidra 12.2 + decompiler, capstone for the float sequence of the weapon-damage function). GMST values: Skyrim.esm / Update.esm where they override the executable (marked ESM), else the executable default (exe).

## Order of operations for one melee hit (HitData init `0x1407efca0`)
1. **Weapon damage** (`0x140229570` -> `0x14041e9b0`):
   `D = ((weaponBase * fDamageWeaponMult(1.0 exe) + temper) * skillMult * attackDamageMult + flatAV) * k`
   - weaponBase: the weapon's base damage (WEAP DATA). temper: from the item's tempering health (`0x140421a40`: linear map between two item-health thresholds to [1, fSmithingWeaponMax 10]; the thresholds are setting pointers not named in this pass; medium).
   - skillMult (`0x14041e900`): player lerp(fDamagePCSkillMin 1.0, fDamagePCSkillMax **1.5 ESM**, skill/100); NPC lerp(fDamageSkillMin 1.0, fDamageSkillMax 3.0 exe, skill/100). Skill = the weapon's skill AV.
   - attackDamageMult: the attack data's damage multiplier (ATKD; power attacks carry their own multiplier there). fPowerAttackDefaultBonus has no reader in this build.
   - flatAV: the attacker's UnarmedDamage (AV 0x22) for hand-to-hand, MeleeDamage (AV 0x23) otherwise (medium: AV index from the call, name from CommonLib).
   - k = 1 for normal hits (another branch uses the frame delta, for per-second damage; not traced).
2. Multi-hit and VATS factors (fVATSAutomaticMeleeDamageMult 2.0 exe, only in VATS-type attacks); x attacker field `+0x98` / 100 (a percent; meaning unverified).
3. Perk entry points: 0x1C (bash attacks), 0x00 (weapon damage), then `0x1407f17c0`:
4. **Perk 0x23 (attacker), 0x24 (target)** on the damage.
5. **Sneak attack** (player attacker only; target not detecting the player, `0x1406a2520` level <= 0): mult = table by weapon animation type (`0x14028c410`, table `0x14207b2a0`): hand-to-hand fCombatSneakHandMult **2.0 ESM**, 1H sword / dagger / axe / mace **3.0 ESM**, 2H sword / axe **2.0 ESM**, bow 2.0 exe, staff 1.0 exe, crossbow 2.0 exe; then perk entry 0x12; damage x mult. (fDamageSneakAttackMult and fCombatSneakAttackBonusMult have no reader in this build.)
6. **Critical hit**: chance c (`0x14041ff60`) from the attacker's CriticalChance AV (0x21), the weapon critical-chance mult (CRDT) and fWeaponConditionCriticalChanceMult 0.1; perk entries 0x01 and 0x11; crit if rand()%1000 < 10c. Crit damage = weapon CRDT damage (unarmed: fCombatUnarmedCritDamageMult **1.0 ESM** x damage), perk entry 0x02; **added** to the damage. medium (chance composition), high (add).
7. Kill-move target: x fCombatKillMoveDamageMult 10.0 exe. Then x attacker AV 0x9A (an attack-damage multiplier AV; name unverified).
8. **Armour and resistances** (`0x1407f09d0`):
   - player target in bleedout state: x fCombatPlayerBleedoutHealthDamageMult (**0.0 ESM**).
   - `r = armourRating x fArmorScalingFactor(0.12 ESM) / 100 + extra`, extra = number of worn armour pieces x fArmorBaseFactor (**0.03 ESM**) (`0x140739470`, cached); perk entry 0x25; **cap r at fMaxArmorRating (80 ESM) / 100**; damage x (1 - r).
   - Armour rating (AV 0x27, `0x140739470`): sum over worn armour of the piece rating x skill mult for its type (Heavy AV 0x0B / Light AV 0x0C): player lerp(fArmorRatingPCBase 1.0, fArmorRatingPCMax **1.4 ESM**, skill/100); NPC lerp(fArmorRatingBase 1.0 ESM, fArmorRatingMax 2.5 ESM, skill/100) (`0x14041f450`).
   - Weapon-specific resistance: if the weapon names a resist AV, damage x (1 - min(AV,100)/100).
   - **Block** (if blocking and facing the attack): `b = shield: (skillMult(Block) + AV 0x42) x shieldRating x fShieldScalingFactor(0.2 ESM)/100 + fShieldBaseFactor(0.45 ESM)`; `weapon/unarmed: skillMult(Block) x weaponBlockValue x fBlockWeaponScaling(0.2 ESM)/100 + fBlockWeaponBase(0.3 ESM)`; x fBlockPowerAttackMult (0.66 ESM) against power attacks (`0x14041f4d0`; skillMult uses fArmorRatingBase/Max). Perk entry 0x27; cap at fPlayerMaxResistance 85 exe / 100; damage x (1 - b).
9. **Difficulty** (`0x140421cc0`, only for Health damage, AV 0x18): damage dealt BY the player x fDiffMultHPByPC[d] (exe: VE 2.0, E 1.5, N 1.0, H 0.75, VH 0.5, L 0.25); damage TO the player x fDiffMultHPToPC[d] (exe: VE 0.5, E 0.75, N 1.0, H 1.5, VH 2.0, L 2.5; **Update.esm Legendary 3.0**).

## Arrows (`0x1407e5930` -> `0x140420210`)
Projectile damage = ammo damage (AMMO `+0xCC`) x (fDamageGunWeapCondBase **0.66 ESM** + fDamageGunWeapCondMult **0.34 ESM** x draw power) x skillMult (Archery). The bow's own damage is combined in the hit data (not traced separately).

## Confidence
- Armour formula, cap, skill mults, sneak table, difficulty tables: high.
- Weapon damage composition: high for the float sequence; medium for the names of the flat AV and the temper thresholds.
- Critical chance composition: medium.
- Not traced: perk entry-point semantics (only their numbers), stagger, damage to weapon/armour condition, poisons and enchantments on hit (`0x1407f1040`, `0x1407f1cc0`).

## Other engines
- spud / OpenSky: not checked for combat in this pass.
