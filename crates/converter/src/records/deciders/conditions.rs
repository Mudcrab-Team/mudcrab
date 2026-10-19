//! Shared native condition selectors authored independently from permitted facts.
use super::{Context, FieldSchema, Selection};

/// Selectors whose inputs all belong to the enclosing CTDA payload.
pub const NAMES: &[&str] = &[
    "condition_comparison",
    "condition_parameter_1",
    "condition_parameter_2",
    "condition_reference",
];

/// Every 397 native and five pinned SKSE function indices, with both parameter types.
/// Names in comments identify observed format facts, not supported execution APIs.
pub const FUNCTION_PARAMETERS: &[(u16, &str, &str)] = &[
    (0, "none", "none"),                           // GetWantBlocking
    (1, "object_reference", "none"),               // GetDistance
    (5, "none", "none"),                           // GetLocked
    (6, "axis", "none"),                           // GetPos
    (8, "axis", "none"),                           // GetAngle
    (10, "axis", "none"),                          // GetStartingPos
    (11, "axis", "none"),                          // GetStartingAngle
    (12, "none", "none"),                          // GetSecondsPassed
    (14, "actor_value", "none"),                   // GetActorValue
    (18, "none", "none"),                          // GetCurrentTime
    (24, "none", "none"),                          // GetScale
    (25, "none", "none"),                          // IsMoving
    (26, "none", "none"),                          // IsTurning
    (27, "object_reference", "none"),              // GetLineOfSight
    (32, "object_reference", "none"),              // GetInSameCell
    (35, "none", "none"),                          // GetDisabled
    (36, "integer", "none"),                       // MenuMode
    (39, "none", "none"),                          // GetDisease
    (41, "none", "none"),                          // GetClothingValue
    (42, "actor", "none"),                         // SameFaction
    (43, "actor", "none"),                         // SameRace
    (44, "actor", "none"),                         // SameSex
    (45, "actor", "none"),                         // GetDetected
    (46, "none", "none"),                          // GetDead
    (47, "inventory_object", "none"),              // GetItemCount
    (48, "none", "none"),                          // GetGold
    (49, "none", "none"),                          // GetSleeping
    (50, "none", "none"),                          // GetTalkedToPC
    (53, "object_reference", "variable_name"),     // GetScriptVariable
    (56, "quest", "none"),                         // GetQuestRunning
    (58, "quest", "none"),                         // GetStage
    (59, "quest", "quest_stage"),                  // GetStageDone
    (60, "faction", "actor"),                      // GetFactionRankDifference
    (61, "none", "none"),                          // GetAlarmed
    (62, "none", "none"),                          // IsRaining
    (63, "none", "none"),                          // GetAttacked
    (64, "none", "none"),                          // GetIsCreature
    (65, "none", "none"),                          // GetLockLevel
    (66, "actor", "none"),                         // GetShouldAttack
    (67, "cell", "none"),                          // GetInCell
    (68, "class", "none"),                         // GetIsClass
    (69, "race", "none"),                          // GetIsRace
    (70, "sex", "none"),                           // GetIsSex
    (71, "faction", "none"),                       // GetInFaction
    (72, "base_object", "none"),                   // GetIsID
    (73, "faction", "none"),                       // GetFactionRank
    (74, "global", "none"),                        // GetGlobalValue
    (75, "none", "none"),                          // IsSnowing
    (77, "none", "none"),                          // GetRandomPercent
    (79, "quest", "variable_name"),                // GetQuestVariable
    (80, "none", "none"),                          // GetLevel
    (81, "none", "none"),                          // IsRotating
    (84, "actor_base", "none"),                    // GetDeadCount
    (91, "none", "none"),                          // GetIsAlerted
    (98, "integer", "integer"),                    // GetPlayerControlsDisabled
    (99, "object_reference", "none"),              // GetHeadingAngle
    (101, "none", "none"),                         // IsWeaponMagicOut
    (102, "none", "none"),                         // IsTorchOut
    (103, "none", "none"),                         // IsShieldOut
    (106, "none", "none"),                         // IsFacingUp
    (107, "none", "none"),                         // GetKnockedState
    (108, "none", "none"),                         // GetWeaponAnimType
    (109, "actor_value", "none"),                  // IsWeaponSkillType
    (110, "none", "none"),                         // GetCurrentAIPackage
    (111, "none", "none"),                         // IsWaiting
    (112, "none", "none"),                         // IsIdlePlaying
    (116, "none", "none"),                         // IsIntimidatedbyPlayer
    (117, "region", "none"),                       // IsPlayerInRegion
    (118, "none", "none"),                         // GetActorAggroRadiusViolated
    (122, "actor", "crime_type"),                  // GetCrime
    (123, "none", "none"),                         // IsGreetingPlayer
    (125, "none", "none"),                         // IsGuard
    (127, "none", "none"),                         // HasBeenEaten
    (128, "none", "none"),                         // GetStaminaPercentage
    (129, "class", "none"),                        // GetPCIsClass
    (130, "race", "none"),                         // GetPCIsRace
    (131, "sex", "none"),                          // GetPCIsSex
    (132, "faction", "none"),                      // GetPCInFaction
    (133, "none", "none"),                         // SameFactionAsPC
    (134, "none", "none"),                         // SameRaceAsPC
    (135, "none", "none"),                         // SameSexAsPC
    (136, "object_reference", "none"),             // GetIsReference
    (141, "none", "none"),                         // IsTalking
    (142, "none", "none"),                         // GetWalkSpeed
    (143, "none", "none"),                         // GetCurrentAIProcedure
    (144, "none", "none"),                         // GetTrespassWarningLevel
    (145, "none", "none"),                         // IsTrespassing
    (146, "none", "none"),                         // IsInMyOwnedCell
    (147, "none", "none"),                         // GetWindSpeed
    (148, "none", "none"),                         // GetCurrentWeatherPercent
    (149, "weather", "none"),                      // GetIsCurrentWeather
    (150, "none", "none"),                         // IsContinuingPackagePCNear
    (152, "faction", "none"),                      // GetIsCrimeFaction
    (153, "none", "none"),                         // CanHaveFlames
    (154, "none", "none"),                         // HasFlames
    (157, "none", "none"),                         // GetOpenState
    (159, "none", "none"),                         // GetSitting
    (161, "package", "none"),                      // GetIsCurrentPackage
    (162, "object_reference", "none"),             // IsCurrentFurnitureRef
    (163, "furniture", "none"),                    // IsCurrentFurnitureObj
    (170, "none", "none"),                         // GetDayOfWeek
    (172, "actor", "none"),                        // GetTalkedToPCParam
    (175, "none", "none"),                         // IsPCSleeping
    (176, "none", "none"),                         // IsPCAMurderer
    (180, "object_reference", "keyword"),          // HasSameEditorLocAsRef
    (181, "alias", "keyword"),                     // HasSameEditorLocAsRefAlias
    (182, "inventory_object", "none"),             // GetEquipped
    (185, "none", "none"),                         // IsSwimming
    (190, "none", "none"),                         // GetAmountSoldStolen
    (192, "none", "none"),                         // GetIgnoreCrime
    (193, "faction", "none"),                      // GetPCExpelled
    (195, "faction", "none"),                      // GetPCFactionMurder
    (197, "faction", "none"),                      // GetPCEnemyofFaction
    (199, "faction", "none"),                      // GetPCFactionAttack
    (203, "none", "none"),                         // GetDestroyed
    (214, "magic_effect", "none"),                 // HasMagicEffect
    (215, "none", "none"),                         // GetDefaultOpen
    (219, "none", "none"),                         // GetAnimAction
    (223, "magic_item", "none"),                   // IsSpellTarget
    (224, "none", "none"),                         // GetVATSMode
    (225, "none", "none"),                         // GetPersuasionNumber
    (226, "none", "none"),                         // GetVampireFeed
    (227, "none", "none"),                         // GetCannibal
    (228, "class", "none"),                        // GetIsClassDefault
    (229, "none", "none"),                         // GetClassDefaultMatch
    (230, "cell", "object_reference"),             // GetInCellParam
    (235, "none", "none"),                         // GetVatsTargetHeight
    (237, "none", "none"),                         // GetIsGhost
    (242, "none", "none"),                         // GetUnconscious
    (244, "none", "none"),                         // GetRestrained
    (246, "base_object", "none"),                  // GetIsUsedItem
    (247, "form_type", "none"),                    // GetIsUsedItemType
    (248, "scene", "none"),                        // IsScenePlaying
    (249, "none", "none"),                         // IsInDialogueWithPlayer
    (250, "location", "none"),                     // GetLocationCleared
    (254, "none", "none"),                         // GetIsPlayableRace
    (255, "none", "none"),                         // GetOffersServicesNow
    (258, "actor", "association_type"),            // HasAssociationType
    (259, "actor", "none"),                        // HasFamilyRelationship
    (261, "actor", "none"),                        // HasParentRelationship
    (262, "form_list", "none"),                    // IsWarningAbout
    (263, "none", "none"),                         // IsWeaponOut
    (264, "magic_item", "none"),                   // HasSpell
    (265, "none", "none"),                         // IsTimePassing
    (266, "none", "none"),                         // IsPleasant
    (267, "none", "none"),                         // IsCloudy
    (274, "none", "none"),                         // IsSmallBump
    (277, "actor_value", "none"),                  // GetBaseActorValue
    (278, "owner", "none"),                        // IsOwner
    (280, "cell", "owner"),                        // IsCellOwner
    (282, "none", "none"),                         // IsHorseStolen
    (285, "none", "none"),                         // IsLeftUp
    (286, "none", "none"),                         // IsSneaking
    (287, "none", "none"),                         // IsRunning
    (288, "none", "none"),                         // GetFriendHit
    (289, "integer", "none"),                      // IsInCombat
    (300, "none", "none"),                         // IsInInterior
    (304, "none", "none"),                         // IsWaterObject
    (305, "none", "none"),                         // GetPlayerAction
    (306, "none", "none"),                         // IsActorUsingATorch
    (309, "none", "none"),                         // IsXBox
    (310, "worldspace", "none"),                   // GetInWorldspace
    (312, "misc_stat", "none"),                    // GetPCMiscStat
    (313, "none", "none"),                         // GetPairedAnimation
    (314, "none", "none"),                         // IsActorAVictim
    (315, "none", "none"),                         // GetTotalPersuasionNumber
    (318, "none", "none"),                         // GetIdleDoneOnce
    (320, "none", "none"),                         // GetNoRumors
    (323, "none", "none"),                         // GetCombatState
    (325, "package_data", "none"),                 // GetWithinPackageLocation
    (327, "none", "none"),                         // IsRidingMount
    (329, "none", "none"),                         // IsFleeing
    (332, "none", "none"),                         // IsInDangerousWater
    (338, "none", "none"),                         // GetIgnoreFriendlyHits
    (339, "none", "none"),                         // IsPlayersLastRiddenMount
    (353, "none", "none"),                         // IsActor
    (354, "none", "none"),                         // IsEssential
    (358, "none", "none"),                         // IsPlayerMovingIntoNewSpace
    (359, "location", "none"),                     // GetInCurrentLoc
    (360, "alias", "none"),                        // GetInCurrentLocAlias
    (361, "none", "none"),                         // GetTimeDead
    (362, "keyword", "none"),                      // HasLinkedRef
    (365, "none", "none"),                         // IsChild
    (366, "faction", "none"),                      // GetStolenItemValueNoCrime
    (367, "none", "none"),                         // GetLastPlayerAction
    (368, "integer", "none"),                      // IsPlayerActionActive
    (370, "actor", "none"),                        // IsTalkingActivatorActor
    (372, "form_list", "none"),                    // IsInList
    (373, "faction", "none"),                      // GetStolenItemValue
    (375, "optional_faction", "none"),             // GetCrimeGoldViolent
    (376, "optional_faction", "none"),             // GetCrimeGoldNonviolent
    (378, "shout", "none"),                        // HasShout
    (381, "integer", "none"),                      // GetHasNote
    (390, "none", "none"),                         // GetHitLocation
    (391, "none", "none"),                         // IsPC1stPerson
    (396, "none", "none"),                         // GetCauseofDeath
    (397, "integer", "none"),                      // IsLimbGone
    (398, "form_list", "none"),                    // IsWeaponInList
    (402, "none", "none"),                         // IsBribedbyPlayer
    (403, "object_reference", "none"),             // GetRelationshipRank
    (407, "vats_function", "vats_parameter"),      // GetVATSValue
    (408, "actor", "none"),                        // IsKiller
    (409, "form_list", "none"),                    // IsKillerObject
    (410, "faction", "faction"),                   // GetFactionCombatReaction
    (414, "object_reference", "none"),             // Exists
    (415, "none", "none"),                         // GetGroupMemberCount
    (416, "none", "none"),                         // GetGroupTargetCount
    (426, "voice_type", "none"),                   // GetIsVoiceType
    (427, "none", "none"),                         // GetPlantedExplosive
    (429, "none", "none"),                         // IsScenePackageRunning
    (430, "none", "none"),                         // GetHealthPercentage
    (432, "form_type", "none"),                    // GetIsObjectType
    (434, "none", "none"),                         // GetDialogueEmotion
    (435, "none", "none"),                         // GetDialogueEmotionValue
    (437, "integer", "none"),                      // GetIsCreatureType
    (444, "form_list", "none"),                    // GetInCurrentLocFormList
    (445, "encounter_zone", "none"),               // GetInZone
    (446, "axis", "none"),                         // GetVelocity
    (447, "variable_name", "none"),                // GetGraphVariableFloat
    (448, "perk", "integer"),                      // HasPerk
    (449, "actor", "none"),                        // GetFactionRelation
    (450, "idle", "none"),                         // IsLastIdlePlayed
    (453, "none", "none"),                         // GetPlayerTeammate
    (454, "none", "none"),                         // GetPlayerTeammateCount
    (458, "none", "none"),                         // GetActorCrimePlayerEnemy
    (459, "optional_faction", "none"),             // GetCrimeGold
    (463, "object_reference", "none"),             // IsPlayerGrabbedRef
    (465, "keyword", "none"),                      // GetKeywordItemCount
    (470, "none", "none"),                         // GetDestructionStage
    (473, "alignment", "none"),                    // GetIsAlignment
    (476, "none", "none"),                         // IsProtected
    (477, "actor", "none"),                        // GetThreatRatio
    (479, "equip_type", "none"),                   // GetIsUsedItemEquipType
    (487, "none", "none"),                         // IsCarryable
    (488, "none", "none"),                         // GetConcussed
    (491, "none", "none"),                         // GetMapMarkerVisible
    (493, "knowable", "none"),                     // PlayerKnows
    (494, "actor_value", "none"),                  // GetPermanentActorValue
    (495, "none", "none"),                         // GetKillingBlowLimb
    (497, "none", "none"),                         // CanPayCrimeGold
    (499, "none", "none"),                         // GetDaysInJail
    (500, "none", "none"),                         // EPAlchemyGetMakingPoison
    (501, "keyword", "none"),                      // EPAlchemyEffectHasKeyword
    (503, "none", "none"),                         // GetAllowWorldInteractions
    (508, "none", "none"),                         // GetLastHitCritical
    (513, "actor", "none"),                        // IsCombatTarget
    (515, "object_reference", "none"),             // GetVATSRightAreaFree
    (516, "object_reference", "none"),             // GetVATSLeftAreaFree
    (517, "object_reference", "none"),             // GetVATSBackAreaFree
    (518, "object_reference", "none"),             // GetVATSFrontAreaFree
    (519, "none", "none"),                         // GetLockIsBroken
    (520, "none", "none"),                         // IsPS3
    (521, "none", "none"),                         // IsWin32
    (522, "object_reference", "none"),             // GetVATSRightTargetVisible
    (523, "object_reference", "none"),             // GetVATSLeftTargetVisible
    (524, "object_reference", "none"),             // GetVATSBackTargetVisible
    (525, "object_reference", "none"),             // GetVATSFrontTargetVisible
    (528, "critical_stage", "none"),               // IsInCriticalStage
    (530, "none", "none"),                         // GetXPForNextLevel
    (533, "faction", "none"),                      // GetInfamy
    (534, "faction", "none"),                      // GetInfamyViolent
    (535, "faction", "none"),                      // GetInfamyNonViolent
    (543, "quest", "none"),                        // GetQuestCompleted
    (547, "none", "none"),                         // IsGoreDisabled
    (550, "scene", "integer"),                     // IsSceneActionComplete
    (552, "magic_item", "none"),                   // GetSpellUsageNum
    (554, "none", "none"),                         // GetActorsInHigh
    (555, "none", "none"),                         // HasLoaded3D
    (560, "keyword", "none"),                      // HasKeyword
    (561, "ref_type", "none"),                     // HasRefType
    (562, "keyword", "none"),                      // LocationHasKeyword
    (563, "ref_type", "none"),                     // LocationHasRefType
    (565, "location", "none"),                     // GetIsEditorLocation
    (566, "alias", "none"),                        // GetIsAliasRef
    (567, "alias", "none"),                        // GetIsEditorLocAlias
    (568, "none", "none"),                         // IsSprinting
    (569, "none", "none"),                         // IsBlocking
    (570, "casting_source", "none"),               // HasEquippedSpell
    (571, "casting_source", "none"),               // GetCurrentCastingType
    (572, "casting_source", "none"),               // GetCurrentDeliveryType
    (574, "none", "none"),                         // GetAttackState
    (576, "event", "event_data"),                  // GetEventData
    (577, "object_reference", "object_reference"), // IsCloserToAThanB
    (579, "shout", "none"),                        // GetEquippedShout
    (580, "none", "none"),                         // IsBleedingOut
    (584, "object_reference", "axis"),             // GetRelativeAngle
    (589, "none", "none"),                         // GetMovementDirection
    (590, "none", "none"),                         // IsInScene
    (591, "location", "ref_type"),                 // GetRefTypeDeadCount
    (592, "location", "ref_type"),                 // GetRefTypeAliveCount
    (594, "none", "none"),                         // GetIsFlying
    (595, "magic_item", "casting_source"),         // IsCurrentSpell
    (596, "casting_source", "keyword"),            // SpellHasKeyword
    (597, "casting_source", "none"),               // GetEquippedItemType
    (598, "alias", "none"),                        // GetLocationAliasCleared
    (600, "alias", "ref_type"),                    // GetLocAliasRefTypeDeadCount
    (601, "alias", "ref_type"),                    // GetLocAliasRefTypeAliveCount
    (602, "ward_state", "none"),                   // IsWardState
    (603, "object_reference", "keyword"),          // IsInSameCurrentLocAsRef
    (604, "alias", "keyword"),                     // IsInSameCurrentLocAsRefAlias
    (605, "alias", "location"),                    // LocAliasIsLocation
    (606, "location", "keyword"),                  // GetKeywordDataForLocation
    (608, "alias", "keyword"),                     // GetKeywordDataForAlias
    (610, "alias", "keyword"),                     // LocAliasHasKeyword
    (611, "package_data", "none"),                 // IsNullPackageData
    (612, "integer", "none"),                      // GetNumericPackageData
    (613, "furniture_animation", "none"),          // IsFurnitureAnimType
    (614, "furniture_entry", "none"),              // IsFurnitureEntryType
    (615, "none", "none"),                         // GetHighestRelationshipRank
    (616, "none", "none"),                         // GetLowestRelationshipRank
    (617, "association_type", "none"),             // HasAssociationTypeAny
    (618, "none", "none"),                         // HasFamilyRelationshipAny
    (619, "axis", "none"),                         // GetPathingTargetOffset
    (620, "axis", "none"),                         // GetPathingTargetAngleOffset
    (621, "none", "none"),                         // GetPathingTargetSpeed
    (622, "axis", "none"),                         // GetPathingTargetSpeedAngle
    (623, "none", "none"),                         // GetMovementSpeed
    (624, "object_reference", "none"),             // GetInContainer
    (625, "location", "none"),                     // IsLocationLoaded
    (626, "alias", "none"),                        // IsLocAliasLoaded
    (627, "none", "none"),                         // IsDualCasting
    (629, "quest", "variable_name"),               // GetVMQuestVariable
    (630, "object_reference", "variable_name"),    // GetVMScriptVariable
    (631, "none", "none"),                         // IsEnteringInteractionQuick
    (632, "none", "none"),                         // IsCasting
    (633, "none", "none"),                         // GetFlyingState
    (635, "none", "none"),                         // IsInFavorState
    (636, "none", "none"),                         // HasTwoHandedWeaponEquipped
    (637, "none", "none"),                         // IsExitingInstant
    (638, "none", "none"),                         // IsInFriendStateWithPlayer
    (639, "object_reference", "float"),            // GetWithinDistance
    (640, "actor_value", "none"),                  // GetActorValuePercent
    (641, "none", "none"),                         // IsUnique
    (642, "none", "none"),                         // GetLastBumpDirection
    (644, "furniture_animation", "none"),          // IsInFurnitureState
    (645, "none", "none"),                         // GetIsInjured
    (646, "none", "none"),                         // GetIsCrashLandRequest
    (647, "none", "none"),                         // GetIsHastyLandRequest
    (650, "object_reference", "keyword"),          // IsLinkedTo
    (651, "keyword", "none"),                      // GetKeywordDataForCurrentLocation
    (652, "object_reference", "none"),             // GetInSharedCrimeFaction
    (654, "none", "none"),                         // GetBribeSuccess
    (655, "none", "none"),                         // GetIntimidateSuccess
    (656, "none", "none"),                         // GetArrestedState
    (657, "none", "none"),                         // GetArrestingActor
    (659, "none", "none"),                         // EPTemperingItemIsEnchanted
    (660, "keyword", "none"),                      // EPTemperingItemHasKeyword
    (664, "casting_source", "none"),               // GetReplacedItemType
    (672, "none", "none"),                         // IsAttacking
    (673, "none", "none"),                         // IsPowerAttacking
    (674, "none", "none"),                         // IsLastHostileActor
    (675, "variable_name", "none"),                // GetGraphVariableInt
    (676, "none", "none"),                         // GetCurrentShoutVariation
    (678, "actor", "none"),                        // ShouldAttackKill
    (680, "none", "none"),                         // GetActivatorHeight
    (681, "actor_value", "none"),                  // EPMagic_IsAdvanceSkill
    (682, "keyword", "none"),                      // WornHasKeyword
    (683, "none", "none"),                         // GetPathingCurrentSpeed
    (684, "axis", "none"),                         // GetPathingCurrentSpeedAngle
    (691, "keyword", "none"),                      // EPModSkillUsage_AdvanceObjectHasKeyword
    (692, "player_action", "none"),                // EPModSkillUsage_IsAdvanceAction
    (693, "keyword", "none"),                      // EPMagic_SpellHasKeyword
    (694, "none", "none"),                         // GetNoBleedoutRecovery
    (696, "actor_value", "none"),                  // EPMagic_SpellHasSkill
    (697, "keyword", "none"),                      // IsAttackType
    (698, "none", "none"),                         // IsAllowedToFly
    (699, "keyword", "none"),                      // HasMagicEffectKeyword
    (700, "none", "none"),                         // IsCommandedActor
    (701, "none", "none"),                         // IsStaggered
    (702, "none", "none"),                         // IsRecoiling
    (703, "none", "none"),                         // IsExitingInteractionQuick
    (704, "none", "none"),                         // IsPathing
    (705, "actor", "none"),                        // GetShouldHelp
    (706, "casting_source", "none"),               // HasBoundWeaponEquipped
    (707, "keyword", "none"),                      // GetCombatTargetHasKeyword
    (709, "none", "none"),                         // GetCombatGroupMemberCount
    (710, "none", "none"),                         // IsIgnoringCombat
    (711, "none", "none"),                         // GetLightLevel
    (713, "perk", "none"),                         // SpellHasCastingPerk
    (714, "none", "none"),                         // IsBeingRidden
    (715, "none", "none"),                         // IsUndead
    (716, "none", "none"),                         // GetRealHoursPassed
    (718, "none", "none"),                         // IsUnlockedDoor
    (719, "actor", "none"),                        // IsHostileToActor
    (720, "object_reference", "none"),             // GetTargetHeight
    (721, "none", "none"),                         // IsPoison
    (722, "keyword", "none"),                      // WornApparelHasKeywordCount
    (723, "none", "none"),                         // GetItemHealthPercent
    (724, "none", "none"),                         // EffectWasDualCast
    (725, "none", "none"),                         // GetKnockedStateEnum
    (726, "none", "none"),                         // DoesNotExist
    (730, "none", "none"),                         // IsOnFlyingMount
    (731, "none", "none"),                         // CanFlyHere
    (732, "none", "none"),                         // IsFlyingMountPatrolQueud
    (733, "none", "none"),                         // IsFlyingMountFastTravelling
    (734, "none", "none"),                         // IsOverEncumbered
    (735, "none", "none"),                         // GetActorWarmth
    (1024, "none", "none"),                        // GetSKSEVersion
    (1025, "none", "none"),                        // GetSKSEVersionMinor
    (1026, "none", "none"),                        // GetSKSEVersionBeta
    (1027, "none", "none"),                        // GetSKSERelease
    (1028, "none", "none"),                        // ClearInvalidRegistrations
];

/// Look up both native slots without interpreting an unregistered extension function.
pub fn function_parameters(index: u16) -> Option<(&'static str, &'static str)> {
    FUNCTION_PARAMETERS
        .binary_search_by_key(&index, |entry| entry.0)
        .ok()
        .map(|position| {
            let (_, first, second) = FUNCTION_PARAMETERS[position];
            (first, second)
        })
}

/// Pick a named category; missing authored alternatives are bounded field errors.
fn alternative(field: &FieldSchema, name: &str) -> Result<Selection, String> {
    field
        .fields
        .iter()
        .position(|candidate| candidate.name == name)
        .map(Selection::Alternative)
        .ok_or_else(|| format!("condition slot lacks {name} alternative"))
}

/// Decode a checked native word using explicit little-endian bytes.
fn word(parent: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        parent[offset..offset + 4]
            .try_into()
            .expect("checked CTDA extent"),
    )
}

/// Interpret the VATS parameter only for native selectors with a known typed slot.
fn vats_parameter(selector: u32) -> &'static str {
    match selector {
        0 => "vats_weapon",
        1 => "vats_weapon_list",
        2 => "vats_target",
        3 => "vats_target_list",
        5 => "vats_target_part",
        6 => "vats_action",
        9 => "vats_critical_effect",
        10 => "vats_critical_effect_list",
        15 => "vats_weapon_type",
        18 => "vats_projectile_type",
        19 => "vats_delivery_type",
        20 => "vats_casting_type",
        _ => "unknown",
    }
}

/// Resolve one condition member using its complete own native payload.
/// Unknown functions and unused parameters preserve bytes; only verified links remap.
pub fn select(
    name: &str,
    field: &FieldSchema,
    bytes: &[u8],
    context: &Context<'_>,
) -> Result<Selection, String> {
    if !NAMES.contains(&name) {
        return Err(format!("unknown condition decider {name}"));
    }
    let parent = context
        .parent_payload
        .ok_or("condition slot lacks its CTDA payload")?;
    if parent.len() != 32 || bytes.len() != 4 {
        return Err("condition requires a 32-byte CTDA and a 4-byte member".into());
    }
    let selected = match name {
        "condition_comparison" => {
            if parent[0] & 4 != 0 {
                "global"
            } else {
                "float"
            }
        }
        "condition_reference" => match word(parent, 20) {
            2 => "reference",
            _ => "unused",
        },
        "condition_parameter_1" | "condition_parameter_2" => {
            let index = u16::from_le_bytes([parent[8], parent[9]]);
            let (first, second) = function_parameters(index).unwrap_or(("unknown", "unknown"));
            let mut kind = if name == "condition_parameter_1" {
                first
            } else {
                second
            };
            if matches!(kind, "object_reference" | "actor" | "package") {
                if parent[0] & 2 != 0 {
                    kind = "alias";
                } else if parent[0] & 8 != 0 {
                    kind = "package_data";
                }
            }
            if kind == "vats_parameter" {
                kind = vats_parameter(word(parent, 12));
            }
            kind
        }
        _ => unreachable!("explicit registry checked"),
    };
    alternative(field, selected)
}
