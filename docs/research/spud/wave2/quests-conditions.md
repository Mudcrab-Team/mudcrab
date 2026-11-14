# Wave 2: quests, conditions, story manager, scenes, dialogue (topic `quests-conditions`)

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Wave 2: quests, conditions, story manager, scenes, dialogue (topic `quests-conditions`)

Every line cite below was read. Where a row cites a function's start line, the individual `case`
inside it is a few lines later in the same function.

## Scope read

In-scope core files, measured with `wc -l`:

| file | lines | read |
|---|---|---|
| `src/game/quests.cpp` | 1969 | 321-1740 (parse, lifecycle, stages, objectives, aliases, fills, reservations, natives, save header) |
| `src/game/quests.hpp` | 441 | all |
| `src/game/conditions.cpp` | 882 | all |
| `src/game/conditions.hpp` | 203 | all |
| `src/game/quest_text.cpp` | 416 | all |
| `src/game/story_manager.cpp` | 390 | all |
| `src/game/story_manager.hpp` | 70 | all |
| `src/game/scenes.cpp` | 665 | 23-100, 240-600, 635-665 |
| `src/game/scenes.hpp` | 129 | all |
| `src/game/dialogue.cpp` | 983 | 45-560, 947-983 |
| `src/game/dialogue.hpp` | 206 | all |
| `src/game/conversations.hpp` | 22 | all |
| `src/engine/app_dialogue.cpp` | 277 | all |
| `src/engine/journal_source.cpp` | 126 | all |
| `src/engine/journal_source.hpp` | 30 | all |

Measured total: 6,376 lines in the 12 core files plus 433 engine lines = 6,809 lines.

Not read, with reason:

- [`quests.cpp:1734-1969`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1734-L1969): the binary save/load body and `alias_has_keyword`/`alias_packages`
  ([quests.cpp:1926](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1926), [:1943](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1943)). Bulk field-by-field serialisation; no rules.
- [`dialogue.cpp:1-45`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L1-L45), [`:380-436`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L380-L436), [`:560-947`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L560-L947): voice path tail, the conversation line pump
  (`speak_current`, `update`, `line_finished`, `end_conversation`, `explain`). Out of the
  scope of this report (stage/objective/alias/condition/event paths); dialogue line timing is the
  `engine-app` topic.
- [`scenes.cpp:1-23`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L1-L23), [`:100-240`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L100-L240), [`:600-635`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L600-L635), [`:665`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L665): `start`/`force_start`/`stop`/`quest_started` were read
  in 187-240; the skipped ranges are helpers and the stop fragment tail already summarised by
  `set_playing` ([scenes.cpp:587](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L587)).
- `docs/`: spud's docs mention quests only in one line ([`docs/ROADMAP.md:12`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/ROADMAP.md#L12)); the numbered
  "design"/"section" comments in the code refer to a document that is not in the repo, so
  section numbers below are spud's own references, not verifiable text.

## Architecture

Three layers, all in `src/game`, with a thin engine adapter.

1. `Conditions` ([`conditions.hpp:123`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.hpp#L123)) is a pure function from (records, `WorldState`,
   `ConditionFacts`) to a boolean. It reads CTDA lists decoded by the forms database and answers
   condition functions from the record graph; anything live (actor values, globals, quest
   variables, animation graph variables) comes through the `ConditionFacts` interface
   ([conditions.hpp:56](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.hpp#L56)), implemented by `game::Game` ([game.hpp:92](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/game.hpp#L92)) and `engine::Papyrus`
   ([papyrus.hpp:23](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/papyrus.hpp#L23)).
2. `QuestSystem` ([`quests.hpp:255`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L255)) owns the QUST records, the running/starting/stopping lists,
   stage items, objectives, alias fill and quest scripts. `QuestWorld` ([`quests.hpp:180`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L180)) is the
   virtual seam over the world (`loaded_refs`, `ref_state`, `create_ref`, `location_cleared`,
   `alias_applied`, `notify`); without live callbacks every answer comes from records.
3. `StoryManager` ([`story_manager.hpp:15`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.hpp#L15)) owns the SMEN/SMBN/SMQN tree and an event queue.
   `Scenes` ([`scenes.hpp:38`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.hpp#L38)) and `Dialogue` ([`dialogue.hpp:124`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.hpp#L124)) hang off `QuestSystem` as
   members ([`quests.hpp:355-357`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L355-L357)) and register as native providers ([`quests.cpp:1528`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1528)).

Data flow per frame ([`quests.cpp:791`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L791)):

```
engine app  -- queue(StoryEvent) -->  StoryManager.m_pending
                                        |  process_events()  (priority QSTR/AFAV first)
                                        v
                     root SMEN conds -> event node -> SMBN/SMQN walk
                                        |  QUST.story_conditions + fill_aliases(event)
                                        v
                                QuestSystem.m_candidates -> m_starting
update_quests(): starting -> init_quest (reset scripts, send quest event, run StartUp stages)
                 -> running (sorted by priority) -> begin_stop (ShutDown stages) -> finish_stop
                     |                                   |
                     +--> Scenes::quest_started/update      +--> release_aliases, unregister scripts
```

Threading/ownership: single-threaded and synchronous. The only concurrency is
`QuestWorld::index_refs` warming indexes on an `std::future` ([quests.hpp:268](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L268), [quests.cpp:72-183](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L72-L183))
and the engine's voice-length reads ([`app_dialogue.cpp:58`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_dialogue.cpp#L58)). Comment at [quests.cpp:1378](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1378) says
fills finish in one call because nothing yet makes a fill slow enough to slice.

Lifecycle state machine ([`quests.hpp:151-156`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L151-L156)): two bits, `running` (0x1) plus `wait` (0x80) plus a
`promoting` flag, give `is_starting/is_running/is_stopping/is_stopped`. The engine drives it with
`update(float, game_days, menu_paused)`; scenes stop advancing while a menu pauses
([quests.cpp:788](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L788)).

## Feature ledger

### Quest record decoding and state

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| game-qst-parse | QUST record decode | sections: quest data, stages (INDX/QSDT), objectives (QOBJ/QSTA), aliases (ALST/ALLS) | one pass over decoded fields with a `Section` enum; CTDA routed to the current section's list | [quests.cpp:321-492](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L321-L492) | impl | none seen; sub-record order assumed, unknown sigs dropped | unknown subrecords silently ignored (`default: break`) |
| game-qst-flags | runtime flag masking at load | only the persistent record flags survive; run state starts empty | forces `start_game` from the record's running bit, clears running/completed/failed/shown/wait/active/keep_instance; an event-scoped quest (`ENAM != 0`) loses both running and start-game | [quests.cpp:478-491](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L478-L491) | impl | start-game for event quests is forced off, which vanilla also does at runtime, not at load | — |
| game-qst-stageflags | stage flags | INDX flags: start-up, shut-down, keep-instance, done | `stage_done` cleared at load; start-up/shut-down/keep-instance kept | [quests.cpp:377-384](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L377-L384), [quests.hpp:35](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L35) | impl | none seen | — |
| game-qst-qsdt | stage item decode | QSDT flags = complete/fail quest | `item_complete_quest=0x1`, `item_fail_quest=0x2`; CNAM log; NAM0 next quest | [quests.cpp:390-407](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L390-L407), [quests.hpp:36-37](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L36-L37) | impl | none seen | — |
| game-qst-objparse | objective decode | QOBJ index, NNAM text, QSTA target with ALST alias and lock flag, FNAM OR flag | `ored` from FNAM bit 0 for both objective and target sections | [quests.cpp:409-434](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L409-L434) | impl | FNAM meaning disambiguated only by current section | — |
| game-qst-objstate | objective states | 0 dormant, 1 displayed, 2 done, 3 displayed+done, 4 failed, 5 displayed+failed | explicit enum in `Objective::state` | [quests.hpp:104-111](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L104-L111) | impl | none seen | — |
| game-qst-aliasparse | alias decode | 20+ AL* subrecords | all of ALID/ALFI/ALFR/ALUA/ALFA/ALRT/KNAM/ALEQ/ALEA/ALCO/ALCA/ALCL/ALNA/ALFE/ALFD/ALFL/ALSP/ALFC/ALPC/ALDN/CNTO/KWDA | [quests.cpp:426-471](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L426-L471), [quests.hpp:114-140](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L114-L140) | impl | `alias_filled_by_other` is re-derived at parse from ALFI rather than read from FNAM | FNAM filled-by-other bit masked out |
| game-qst-filltype | fill-method inference | one fill type per alias from which subrecord is present | numeric `fill` code: 0 find matching (+conditions), 1 specific (ALFR/ALFL), 2 find-in-location (ALFA), 3 from event (ALFE), 4 create object (ALCO/ALEQ location), 5 external alias (ALEQ), 6 unique actor (ALUA), 7 near alias (ALNA) | [quests.cpp:423-444](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L423-L444), [:470](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L470), [:1182-1317](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1182-L1317) | impl | an alias with only conditions becomes "find matching" (vanilla's Find Matching is exactly that, so this is a shared default) | `fill=-1` with no conditions never fills; non-optional such alias fails the quest |
| game-qst-findmatching | Find Matching fill | scan loaded refs, then persistent refs unless "in loaded area" | two passes over `loaded_refs()` then `persistent_refs()`; player excluded; `usable()` filter; conditions evaluated per candidate | [quests.cpp:1187-1243](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1187-L1243) | impl | persistent pass is a data pass, not a streaming pass; ordering is record order then uniform pick | 0 when nothing passes |
| game-qst-candidates | candidate narrowing | engine-side optimisation, not semantics | a group that only ANDs on HasRefType / GetIsReference / GetInCell / GetIsID / GetInFaction supplies the candidate set; conditions still decide | [quests.cpp:1102-1170](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1102-L1170) | impl | **invented optimisation**; changes which refs can be picked when a group mixes narrowing with unmodelled OR | falls back to the full scan when no group narrows |
| game-qst-failsforall | pre-scan short circuit | not a vanilla concept | evaluates a subject-free group once; if it fails for the player, the alias fails without scanning | [conditions.cpp:848-864](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L848-L864), [quests.cpp:1189-1191](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1189-L1191) | impl | invented fast path; correct only for subject-free groups (guarded) | returns 0 ref |
| game-qst-onlyactors | actor-only narrowing | not a vanilla concept | `needs_actor` on a standalone AND item sends the scan to `loaded_actors()` | [quests.cpp:1171-1180](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1171-L1180) | impl | invented; safe only because those functions return 0 for non-actors | — |
| game-qst-closest | closest flag | ALFR "closest" picks by distance | sorts the filtered list by `distance_to_player`, then honour-reuse rule | [quests.cpp:1238-1247](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1238-L1247), [quests.hpp:29](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L29) | impl | distance from the live world only; default 1e30 for every ref makes the sort stable/arbitrary | unsorted when no live distance |
| game-qst-reuse | reuse rule | a ref already in another alias of the quest is skipped unless reuse allowed | `pick()` removes violations and re-picks uniformly | [quests.cpp:1086-1100](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1086-L1100) | impl | uniform pick, vanilla uses its own random stream | 0 when every candidate violates reuse |
| game-qst-specific | specific reference fill | ALFR/ALFL, one ref | `ref_state` existence/deleted check, reservation and condition gate | [quests.cpp:1244-1250](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1244-L1250) | impl | deleted check only; disabled/destroyed not tested here (unlike `usable`) | 0 |
| game-qst-filoc | find matching in location | ALFA + ALRT | `location_refs(location, ref_type)` then `usable()` and uniform pick | [quests.cpp:1251-1266](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1251-L1266) | impl | — | 0 when the other alias has no location |
| game-qst-fromevent | from event | ALFE/ALFD, member of the triggering event | event type must match `a.event`, member value used as the ref | [quests.cpp:1267-1276](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1267-L1276) | impl | only one member, no alias-vs-event location variants | 0 |
| game-qst-createobj | create object alias | ALCO at ALCA, on fill, at level ALCL | at fill it returns the base form ("stub until promotion"); at promotion it calls `world.create_ref(base, anchor, inside, level)` | [quests.cpp:1277-1279](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1277-L1279), [:636-650](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L636-L650) | impl | the alias holds a base form id until promotion; world may refuse | alias dropped from `filled_refs` if `create_ref` returns 0 |
| game-qst-external | external alias | ALEQ/ALEA from another quest | reads the other quest's filled map directly, then conditions | [quests.cpp:1280-1287](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1280-L1287) | impl | no cycle or ordering check; depends on fill order | 0 |
| game-qst-unique | unique actor | ALUA resolves the placed ref of an NPC | `ref_of_unique_npc` then dead/disabled/reserved checks | [quests.cpp:1288-1299](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1288-L1299) | impl | two different usability paths (this one ignores destroyed/exists) | 0 |
| game-qst-near | near alias | ALNA, linked refs of the other alias | `linked_children(other)`, usable + conditions, uniform pick | [quests.cpp:1300-1316](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1300-L1316) | impl | no radius test; keyword-less links only | 0 |
| game-qst-locfill | location alias fill | any LCTN matching, prefer uncleared | iterates all LCTN, deleted/reserved/cleared filters, fresh list preferred over cleared | [quests.cpp:1318-1345](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1318-L1345) | impl | "cleared" needs a live callback, else everything is fresh | 0 |
| game-qst-locfromalias | location from alias | parent location with keyword | `location_parent_with_keyword` walking PNAM | [quests.cpp:1346-1358](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1346-L1358) | impl | — | 0 |
| game-qst-locfromevent | location from event | FNAM/ALFE member | same member read as ref fill | [quests.cpp:1352-1358](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1352-L1358) | impl | — | 0 |
| game-qst-locexternal | location external alias | ALEQ/ALEA when ALLS | reads other quest's `filled_locations` | [quests.cpp:1359-1367](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1359-L1367) | impl | — | 0 |
| game-qst-optional | optional alias failure | optional alias simply stays empty | non-optional failure clears everything and records `fill_failure` with each condition's index and a `?` when the function is unavailable | [quests.cpp:1378-1433](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1378-L1433) | impl | the `?` marker is spud's own diagnostic | quest fails (start returns false / QSTR waits return false) |
| game-qst-alfi | force-into alias | ALFI: two aliases share one ref | at fill the ref is written into both alias ids; at promotion `filled_by_other` aliases get their effects applied | [quests.cpp:1400-1405](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1400-L1405), [:654-668](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L654-L668) | impl | — | non-optional filled-by-other alias with no source fails the fill ([quests.cpp:1419-1424](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1419-L1424)) |
| game-qst-reserve | alias reservation counting | reserved refs cannot fill other quests | `m_reservations` map, +1 on fill/promotion, -1 on clear/stop | [quests.cpp:1434-1455](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1434-L1455), [:1456-1469](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1456-L1469) | impl | reservation is a count, not a per-quest claim | `reserved()` false before `m_ready` |
| game-qst-start | quest start | start is asynchronous, fills, then initialises | `start(q, immediate, pending)`: run-once gate, immediate fill + `add_starting`, else queue a `QSTR` StoryEvent | [quests.cpp:565-597](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L565-L597) | impl | QSTR goes through the story manager even for a plain script `Start()` | false on run-once or fill failure |
| game-qst-newgame | new game | all start-game quests start behind the loading screen | binds scripts for every quest and alias, fills each `start_game_enabled` quest, moves it to `m_starting`; a failed fill leaves the running bit with the quest in no list | [quests.cpp:538-564](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L538-L564) | impl | `first` quest parameter is an invented ordering hint | failed start-game quest is running-but-unlisted |
| game-qst-promote | promotion (fill -> start) | reserved refs claimed, alias effects applied, instance text taken | `finish_promotion`: reservations, alias_applied for non-location, then created objects, then filled-by-other, then instance text | [quests.cpp:618-660](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L618-L660) | impl | order is a reconstruction; no per-alias "init" script call here | — |
| game-qst-init | quest init | reset scripts unless run-once, send quest event, run StartUp stages | VM reset per quest and alias object, `send_quest_event`, sets wait bit, runs start-up stages | [quests.cpp:661-685](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L661-L685) | impl | quest event is replayed on every init, vanilla sends only the event that started it | — |
| game-qst-update | frame lifecycle | starting -> running, running -> stopping, stopping -> stopped | `update_quests` erases from the running list when the bit drops, promotes waiters when outstanding items empty, sorts by priority | [quests.cpp:735-790](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L735-L790) | impl | priority sort on change only | — |
| game-qst-setstage | SetStage | sets current stage and runs its items | requires running bit and an existing stage; `current_stage` is a high-water mark | [quests.cpp:800-810](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L800-L810) | impl | **current stage never decreases** on reset of a later stage | false if not running or unknown stage |
| game-qst-repeat | repeated stages | stage runs again only with allow-repeated-stages | `stage_done` blocks re-entry unless `quest_repeat_stages` | [quests.cpp:811-817](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L811-L817) | impl | none seen | false |
| game-qst-itemcondition | stage item conditions | item runs only if its CTDA list passes | evaluated with subject = player (0x14), target 0, under `Conditions::QuestScope` of the quest | [quests.cpp:834-846](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L834-L846) | impl | always the player as subject; vanilla uses the quest's "run on" subject for stage fragments (player in practice) | item is marked done and skipped |
| game-qst-itemflags | item complete/fail | flag ends or fails the quest | `set_completed` / `set_failed` called before the log and the fragment | [quests.cpp:842-849](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L842-L849) | impl | fail also sets completed (vanilla behaviour) | — |
| game-qst-itemlog | item log | appends the stage log to the journal | writes the instance's single last-entry slot, calls `QuestWorld::log_entry` | [quests.cpp:850-853](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L850-L853), [quest_text.cpp:128-133](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L128-L133) | impl | only the **last** log entry is kept per instance | empty log writes nothing |
| game-qst-nextquest | next quest on completion | NAM0 starts the linked quest | starts it non-immediately when the item completes the quest and the target is not running | [quests.cpp:854-860](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L854-L860) | impl | start failure is ignored silently | no-op |
| game-qst-fragment | stage fragment dispatch | VMAD quest-stage fragment by stage and item index | binary search over `oss::Scripts::fragments` by form id, match kind/index/entry, call the method, then `item_done` | [quests.cpp:861-878](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L861-L878) | impl | fragment must be found by exact entry index; no fallback to a stage-only function | falls through to immediate `item_done` |
| game-qst-itemdone | item completion | latents waiting on the stage resume | removes (stage,item) from `outstanding`, decrements waiter counters, returns true to the top latent when a waiter hits 0 | [quests.cpp:879-896](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L879-L896) | impl | — | — |
| game-qst-stop | stop | clears running, runs ShutDown stages, stops when their items finish | `stop()` clears the bit; `begin_stop` runs shut-down stages and pushes to `m_stopping`; `finish_stop` waits on `outstanding` | [quests.cpp:686-717](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L686-L717), [:735-760](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L735-L760) | impl | stopping is not interruptible once begun | — |
| game-qst-reset | Quest.Reset | returns the quest to its record state | `reset_progress` clears done bits, stage, objective states, flags; then start-game re-add or stop path | [quests.cpp:598-608](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L598-L608), [:718-734](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L718-L734) | impl | does not clear `filled_refs` before re-adding | early return when state already matches |
| game-qst-complete | CompleteQuest / FailQuest | sets flag, fires notice, deactivates | `set_completed`/`set_failed`, `notify(quest_completed/quest_failed)`, clears `quest_active`; skipped for type 6 (misc) | [quests.cpp:927-951](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L927-L951) | impl | the "only one active quest" rule is `if no quest active at all` ([quests.cpp:964-968](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L964-L968)), a simplification | no-op on the record for misc quests |

### Objectives and journal text

| id | behaviour | vanilla rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| game-qst-objdisplayed | SetObjectiveDisplayed | shows, notifies, sets quest active and shown | first display also sends `quest_started` with the quest name and sets `quest_shown`; sets `quest_active` if no quest is active | [quests.cpp:952-993](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L952-L993), [:999-1011](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L999-L1011) | impl | `quest_active` is granted only when no other quest is active, so later quests never become the active one | ignored when the quest is completed or the index is unknown |
| game-qst-objcomplete | SetObjectiveCompleted | state 1 -> 3, else 2 | state machine plus `ored` propagation to sibling objectives in the same display state | [quests.cpp:1012-1035](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1012-L1035), [:981-991](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L981-L991) | impl | OR propagation compares display state, an invented rule | no-op when state >= 2 |
| game-qst-objfailed | SetObjectiveFailed | state 1 -> 5, else 4 | same path, blocked when the quest is completed | [quests.cpp:1024-1035](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1024-L1035) | impl | — | no-op |
| game-qst-allobj | CompleteAllObjectives / FailAllObjectives | mark every objective | iterates objectives and maps state by whether it was shown | [quests.cpp:1036-1042](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1036-L1042) | impl | — | — |
| game-qst-objnotice | objective notice text | objective text with tags resolved, "or" prefixed | `objective_notice` prepends `sOr` for an ored objective | [quests.cpp:994-998](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L994-L998) | impl | — | `notify` drops empty text ([quests.hpp:252-253](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L252-L253)) |
| game-qst-playerobj | player objective table | journal keeps per quest+instance objectives | `m_player_objectives`, updated on every non-zero state, capped at 3000 by dropping the oldest quest | [quest_text.cpp:401-416](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L401-L416) | impl | drop rule is invented | — |
| game-qst-targets | HUD quest targets | references of displayed objectives | `quest_targets()` walks running quests, state==1 objectives, target conditions per filled alias | [quests.cpp:1043-1066](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1043-L1066) | impl | target alias is a *ref* alias only; location targets unsupported | skipped when the alias is empty or conditions fail |
| game-qst-instance | instance text record | journal freezes alias names and globals per instance | `create_instance_text` when any alias stores text, any QTGL exists or any stage has a log; increments `instance` | [quest_text.cpp:81-101](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L81-L101) | impl | created at promotion only; later changes only touch the current instance | no instance created otherwise |
| game-qst-storedform | stored alias form | the base object shown in text | `stored_form`: player -> 7, else REFR NAME, walking TPLT base-data chain, LVLN -> 0 | [quest_text.cpp:50-79](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L50-L79) | impl | leveled-list refs store nothing | 0 |
| game-qst-aliastext | stored text update | ForceRefTo updates the frozen name | `update_alias_text` on `force_ref`/`force_location` | [quest_text.cpp:113-121](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L113-L121), [quests.cpp:1486-1500](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1486-L1500) | impl | — | — |
| game-qst-texttag-alias | `<Alias=Name>` | alias's stored form name or its display-name message | `alias_tag`; ALDN message with `<BaseName>` substituted, then tags resolved recursively | [quest_text.cpp:182-201](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L182-L201) | impl | only `Alias`, `Global`, `Relationship` are known tags | unknown tag: `<` is kept and scanning resumes after it ([quest_text.cpp:381-386](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L381-L386)) |
| game-qst-texttag-global | `<Global=Name>` and modifiers | global value formatted per type and modifier | `global_tag`: plain value (`%.2f` for float GLOB, `%.0f` otherwise), Timer, Hour, Hour12, Minutes, Meridiem, TimeSpan, Day, Month, Year, MonthWord, Weekday | [quest_text.cpp:238-327](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L238-L327) | impl | TimeSpan bounds come from GMST settings, defaulting to 0 -> an empty string if no settings | unknown modifier returns "" |
| game-qst-texttag-rel | `<Relationship=A=B>` | association title by parent/child and sex | RELA scan, MPRT/FPRT/MCHT/FCHT titles | [quest_text.cpp:202-237](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L202-L237) | impl | first matching RELA in database order | nullopt -> `sInvalidTagString` |
| game-qst-textcap | `.Cap` modifier | capitalise the first character | detected anywhere in the modifier, removed before dispatch | [quest_text.cpp:352-356](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L352-L356) | impl | — | — |
| game-qst-textmiss | unknown or failed tag | shows the invalid-tag string | `sInvalidTagString` for a known tag with no value | [quest_text.cpp:387](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L387) | impl | — | empty GMST -> empty text |
| game-qst-save | quest save/load | co-save per quest | binary writer with version 1, then per-quest fields | [quests.cpp:1734-1740](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1734-L1740) (read up to the first quest header) | partial | layout not read in detail | — |

### Natives (quest side)

| id | behaviour | vanilla rule | spud implementation | file:line | evidence | deviation | default |
|---|---|---|---|---|---|---|---|
| game-qst-native-flags | Quest state queries | IsRunning/IsStarting/IsStopping/IsStopped/IsActive/IsCompleted/GetCurrentStageID/IsStageDone | bound over the bit tests; unknown quest -> false or 0 | [quests.cpp:1553-1581](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1553-L1581) | impl | — | 0/false |
| game-qst-native-stage | Quest.Start / SetCurrentStageID | both latent; return false when nothing to wait for | `Start` refuses event-scoped quests with an error; `SetCurrentStageID` returns false when the stage has no items, else parks the stack | [quests.cpp:1642-1690](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1642-L1690) | impl | stage latent returns false when the stage is empty even though the stage did run | false |
| game-qst-native-alias | Alias natives | GetAlias, GetReference, ForceRefTo, Clear, LocationAlias variants | `GetAlias` returns a `ReferenceAlias`/`LocationAlias` object; `Clear` errors on non-optional aliases | [quests.cpp:1626-1732](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1626-L1732) | impl | no `GetActorRef`/`GetActorReference`/`GetLocationAlias` names beyond these | null object |
| game-qst-native-obj | objective queries | IsObjectiveDisplayed/Completed/Failed | state tests; unknown index logs a VM error | [quests.cpp:1608-1625](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1608-L1625) | impl | — | false |

### Story manager and events

| id | behaviour | vanilla rule | spud implementation | file:line | evidence | deviation | default |
|---|---|---|---|---|---|---|---|
| game-qst-event-struct | event payload | 6 members, each a 2-char code + value | `StoryEvent{id, type, Member[6], count}` | [quests.hpp:50-72](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.hpp#L50-L72) | impl | fixed 6 members | extra members impossible |
| game-qst-smtree | story tree load | SMEN events -> SMBN branches -> SMQN quest nodes | all three types loaded into one map; children ordered by SNAM "previous sibling" | [story_manager.cpp:52-139](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L52-L139) | impl | duplicate NNAM quests dropped silently | orphan nodes unreachable |
| game-qst-smrange | reset range | RNAM: fractional days | `RNAM / 576.0` game days | [story_manager.cpp:73](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L73) | impl | constant 576 = 24*24; matches "scaled by 1/24 twice" comment | — |
| game-qst-smqueue | event queue | events queue and flush each frame | `priority_event` = QSTR/AFAV go to `m_active`, others `m_deferred`; one deferred event promoted per pass | [story_manager.cpp:113-169](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L113-L169) | impl | **priority set is invented** (two event types) | — |
| game-qst-smroot | root node gate | root SMEN (0x5B) conditions apply to every event | evaluated once per event with subject=target=0x14 | [story_manager.cpp:193-206](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L193-L206) | impl | — | no root node = pass |
| game-qst-smbranch | branch walk | conditions, ordered or random child order | AND conditions, then first child returning "started" unless random flag 0x1 | [story_manager.cpp:193-227](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L193-L227) | impl | stops at the first starting child; a per-pass stamp stops re-entry | — |
| game-qst-smqnode | quest node | max concurrent, MNAM count, Do All, shares event | XNAM concurrent check, random or ordered quests, `k_num_quests` 0x40000 limit, `k_do_all` 0x10000 retry with conditions skipped | [story_manager.cpp:259-331](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L259-L331) | impl | Do All retry re-runs the whole node with `skip_conditions` | — |
| game-qst-smcanrun | quest eligibility | run-once, reset timer, already running | `can_run`: in any list, do-all already run, reset days, run-once | [story_manager.cpp:228-244](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L228-L244) | impl | `entry.last_run = 1e-6` when game days is 0 | — |
| game-qst-smvisit | quest start by event | QUST story conditions, then alias fill with the event | `story_conditions` at 0x14, then `fill_aliases(*q, &e)`; failure aborts | [story_manager.cpp:245-258](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L245-L258) | impl | dialogue "shared" info conditions are not part of this check | — |
| game-qst-smqstr | QSTR handling | start request | special-cased: fill now; on failure `fill_failed` wakes waiters with false | [story_manager.cpp:171-180](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L171-L180) | impl | — | — |
| game-qst-smadia | ADIA | random conversation event | after the walk, `Conversations::event_processed(event_id, any_started)` | [story_manager.cpp:182-184](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L182-L184) | impl | — | ignored with no conversations object |
| game-qst-smwait | SendStoryEventAndWait | latent returns whether anything started | parked stacks keyed by event id, all resumed with `any_started` | [story_manager.cpp:186-193](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L186-L193), [:356-390](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L356-L390) | impl | one boolean for every waiter | false |
| game-qst-smevent-send | OnStory* dispatch | quest scripts get typed OnStory<Event> | table of 30 event codes -> handler names; argument types from the member code letter (V int, L Location, K Keyword, F Form, else ObjectReference) | [story_manager.cpp:16-32](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L16-L32), [:354-390](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L354-L390) | impl | only 30 of Skyrim's event types are named | unknown event sends nothing |
| game-qst-smevent-natives | Keyword.SendStoryEvent | keyword sends SCPT event | builds SCPT with K1/L1/R1/R2/V1/V2; refuses before quests are ready | [story_manager.cpp:356-390](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L356-L390) | impl | argument order fixed to the native's 5 params | error + false |

### Aliases, scenes, dialogue

| id | behaviour | vanilla rule | spud implementation | file:line | evidence | deviation | default |
|---|---|---|---|---|---|---|---|
| game-qst-sceneparse | SCEN decode | phases with start/completion conditions, actors, actions | field-name driven (`f.def->name`), so it depends on the schema names | [scenes.cpp:23-99](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L23-L99) | impl | a scene with no quest or with unknown aliases is dropped | dropped |
| game-qst-scenestart | Begin on Quest Start | scene begins when its quest starts | `quest_started` starts flagged scenes if the quest runs | [scenes.cpp:212-227](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L212-L227) | impl | — | — |
| game-qst-scenephase | phase machine | start conditions -> fragment -> actions -> completion conditions or all actions done | `update_scene`: start conditions, phase fragment kind `scene_phase` entry 1/2, per-action phase range, all_done or completion conditions | [scenes.cpp:502-566](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L502-L566) | impl | completion conditions replace all_done entirely when present | — |
| game-qst-sceneaction | action lifecycle | dialogue, package, timer | type 0 say/is_saying with looping delay; type 1 package start/done/clear; type 2 timer with two-step fragment | [scenes.cpp:354-470](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L354-L470) | impl | actions whose alias is the current conversation speaker are treated as done ([scenes.cpp:466](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L466)) | unknown type runs and ends at once |
| game-qst-scenelook | head tracking | speaker looks at head-track actor or player | `look_at(ref, target)` and release on end | [scenes.cpp:369-380](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L369-L380), [:445-452](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L445-L452) | impl | only via the engine callback; no graph | no-op when the callback is unset |
| game-qst-scenerandom | random scene (ADIA) | two actors, walk-away timer, cooldown | first two filled aliases, `can_talk`, `initiator`, GMST cooldowns 5..10 s, `fAIWalkAwayTimerForConversation` 2 s | [scenes.cpp:260-308](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L260-L308) | impl | pair is "first two filled aliases", not the scene's actor list | scene ends when `can_talk` is false |
| game-qst-scenerepeat | Repeat While True | restart the scene while conditions hold | re-evaluates scene conditions with subject 0 and restarts at phase 0 | [scenes.cpp:512-520](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L512-L520) | impl | `repeat_while_true` is checked only after the last phase | — |
| game-qst-sceneend | scene end | end fragment, stop quest if flagged | `set_playing(false)`, `scene_stop_quest_on_end` stops the quest, `scene_stop_on_end` fragment | [scenes.cpp:587-634](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L587-L634) (read 587 header and stop path start) | impl | partly read; end-fragment ordering not verified | — |
| game-qst-scenenative | Scene natives | Start/ForceStart/Stop/IsPlaying/IsActionComplete | bound with error messages when the quest is not running | [scenes.cpp:635-664](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L635-L664) | impl | — | — |
| game-qst-dialogue-load | DIAL/DLBR/INFO decode | topic, branch, info chain by PNAM | topics by branch, SNAM subtype overrides the DATA number, INFO ordered by PNAM children, INFO order by plugin+file order | [dialogue.cpp:45-132](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L45-L132) | impl | subtype table is a fixed string list (`k_subtype_codes`) | unknown subtype drops the topic row |
| game-qst-dialogue-conds | INFO gating | quest dialogue conditions then INFO conditions | `info_passes`: reset-hours memory, say-once, `q.dialogue_conditions`, then INFO CTDA at (speaker, player) | [dialogue.cpp:228-254](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L228-L254) | impl | the QUST top-level CTDA list (before NEXT) is used as the quest's dialogue conditions | fails |
| game-qst-dialogue-pick | topic line pick | first passing INFO, or a random one | stops at the first non-random passing INFO; random pool honours `info_random_end` and "used this cycle" | [dialogue.cpp:255-298](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L255-L298) | impl | no "last said" memory beyond reset hours | null |
| game-qst-dialogue-subtype | greeting / hello pick | misc hello subtype, blocking branches | `pick_greeting` walks blocking branches of running quests, then subtype 79 hello | [dialogue.cpp:299-376](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L299-L376) | impl | — | null |
| game-qst-dialogue-branch | conversation list | blocking then top-level, priority order | branches collected per running quest; blocking first, else top-level; rows sorted by priority descending | [dialogue.cpp:436-503](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L436-L503) | impl | the "nine rows" cap is mentioned in the comment but not implemented | empty list closes the menu |
| game-qst-dialogue-tags | menu prompt tags | `<BribeCost>`, `<CrimeGold...>` then quest text tags | `replace_tags` for four prompt tags, then `QuestSystem::quest_text` | [dialogue.cpp:483-499](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L483-L499) | impl | negative value leaves the tag text in place | — |
| game-qst-dialogue-native | dialogue natives | Topic.Add, TopicInfo.GetOwningQuest, ObjectReference.Say/IsInDialogueWithPlayer | bound; `Say` refuses a None topic info | [dialogue.cpp:947-982](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L947-L982) | impl | — | ignored |
| game-app-dialogue | engine dialogue adapter | dialogue menu, voice, subtitles | `EngineDialogue`: menu open/close/text/topics, `.ogg` length from the last granule page off-thread, voice play/follow/stop, prompt tags from crime | [app_dialogue.cpp:46-186](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_dialogue.cpp#L46-L186), [:275-277](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_dialogue.cpp#L275-L277) | impl | voice path/emotion go to `App::actor_speak` | `.ogg` missing -> 0 s, line advances on a timer |
| game-app-journal | journal source | quests tab, objectives tab, misc tab, stats | `GameJournal`: newest instance first, one row per quest+instance, or-joined objectives, `active`/`completed` from flags, misc quests split out | [journal_source.cpp:23-126](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/journal_source.cpp#L23-L126) | impl | misc objectives filtered by type 6 and state 1 | empty lists |

### Condition records and evaluation

| id | behaviour | vanilla rule | spud implementation | file:line | evidence | deviation | default |
|---|---|---|---|---|---|---|---|
| game-cnd-decode | CTDA decode | 32-byte CTDA: flags+operator, function index, run-on, comparison value or global, params 1-3, reference | reads the forms reader's named members (`function`, `flags`, `operator`, `run_on`, `value`, `global`, `param1`, `param2`, `param3`, `reference`) | [conditions.cpp:90-108](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L90-L108) | impl | depends entirely on those member names; there is no raw-byte fallback | missing members read as 0 |
| game-cnd-flags | flag bits | 1 OR, 2 use aliases, 4 use global, 8 use package data, 16 swap subject/target | same five bits, enum `ConditionFlag` | [conditions.hpp:22-27](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.hpp#L22-L27) | impl | `condition_package_data` is decoded but never used | — |
| game-cnd-text | CIS1/CIS2 string | the string after a CTDA belongs to it | both sigs assign `text` to the previous item; interned in a static set | [conditions.cpp:110-113](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L110-L113), [:115-125](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L115-L125) | impl | interning never frees; the set is process-global | no text -> empty pointer |
| game-cnd-list | record condition list | a record's CTDA fields in order | `condition_list` collects every CTDA of a form | [conditions.cpp:115-125](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L115-L125) | impl | records with several lists (PERK, SPEL effects) must read them field by field instead | empty list = true |
| game-cnd-compare | comparison operators | 0 ==, 1 !=, 2 >, 3 >=, 4 <, 5 <= | `switch (item.flags >> 5)` cases 0..5 | [conditions.cpp:823-831](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L823-L831) | impl | operators 6 and 7 return false, matching the comment that they are unknown | false |
| game-cnd-group | OR/AND grouping | an OR flag joins the item to the next into a group; groups AND | accumulate `group` while the OR flag is set, test on the last item of each group | [conditions.cpp:866-881](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L866-L881) | impl | a trailing OR leaves a final group that must pass alone | empty list returns true |
| game-cnd-group-cheap | group short circuit for aliases | not a vanilla concept | `fails_for_all` re-tests subject-free groups once with subject=player | [conditions.cpp:848-864](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L848-L864) | impl | invented; only groups where every item is subject-free are considered | false (i.e. "no group fails for all") |
| game-cnd-runon | run-on targets | subject, target, reference, combat target, linked ref, alias, package, event | `RunOn` enum, mapped in `evaluate`; `target` swaps s/t; `reference` uses the CTDA's reference; `linked_reference` uses `linked_ref(subject, 0)`; `event` uses the current event's member | [conditions.hpp:28](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.hpp#L28), [conditions.cpp:754-780](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L754-L780) | impl | `combat_target`, `alias` and `package` are decoded and then **forced to subject 0** | subject 0 -> entry returns false |
| game-cnd-swap | swap subject/target | swaps only when both exist | `if (flags & condition_swap && s && t) std::swap` | [conditions.cpp:783-785](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L783-L785) | impl | — | — |
| game-cnd-aliasparam | alias parameter | param1 names an alias of the running quest | `condition_aliases` remaps param1 through the fill maps; missing alias -> 0 | [conditions.cpp:786-793](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L786-L793) | impl | only ref aliases; a location alias used as param1 becomes 0 | 0 |
| game-cnd-retarget | implicit target parameter | param1 of reference type defaults to the target | `takes_target(function)` list of 12 functions (13 in the original note; recounted at [`conditions.cpp:64-68`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L64-L68)) plus 584/678/522-525 | [conditions.cpp:64-70](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L64-L70), [:791-793](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L791-L793) | impl | hard-coded list; a new function that takes a reference must be added twice | param1 stays 0 |
| game-cnd-nosubject | subject-less functions | functions that do not need a subject still run | `runs_without_subject` list of 14 functions; everything else returns false when subject is 0 | [conditions.cpp:50-58](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L50-L58), [:794-796](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L794-L796) | impl | — | false |
| game-cnd-globalcmp | comparison value from a global | the CTDA can name a GLOB instead of a float | `condition_global` reads the GLOB record value, then the live override from facts | [conditions.cpp:815-822](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L815-L822) | impl | a missing GLOB compares against 0 | 0 |
| game-cnd-facts | live overrides | the running game knows actor values, quest variables, graph variables, globals | `ConditionFacts::condition_value` is asked first; a true return wins over the records | [conditions.cpp:355-360](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L355-L360) | impl | one interface for every system, keyed by function index | not handled -> records -> `default: 0.0` |
| game-cnd-availability | availability reporting | every function exists; unsupported ones return 0 | `available(function)` = named table, constant-zero table or IsWin32 | [conditions.cpp:127-131](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L127-L131) | impl | **reported state and actual answer disagree** for HasPerk/HasSpell (answered, reported unavailable) | false |
| game-cnd-dist | GetDistance | distance in units; very large when not co-located | requires both refs; different worldspace (or cell indoors) -> `FLT_MAX`; player position from `WorldState`, others from the placed record | [conditions.cpp:362-387](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L362-L387) | impl | only the player has a live position; every other ref uses its placed record position | 0 when param1 is 0 or a position is unknown |
| game-cnd-perk | HasPerk | checks permanent and added perks | base NPC_'s PRKR list only | [conditions.cpp:388-400](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L388-L400) | impl | runtime-added perks are invisible; not in the available table | 0 |
| game-cnd-spell | HasSpell | checks spells on the NPC and its race | NPC_ SPLO plus RACE SPLO | [conditions.cpp:401-412](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L401-L412) | impl | no added spells; not in the available table | 0 |
| game-cnd-av | GetActorValue / GetBaseActorValue / GetPermanentActorValue | skill/attribute values with modifiers | DNAM skill_values for 6..23, health/magicka/stamina for 24..26 | [conditions.cpp:268-282](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L268-L282), [:413-414](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L413-L414) | impl | base values only, no damage, no modifiers, no other actor values | 0 for every other index |
| game-cnd-avpct | GetActorValuePercent / GetHealthPercentage | current / permanent | returns 1 whenever the subject is an NPC | [conditions.cpp:415-416](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L415-L416) | impl | **defaulted**: nothing damages actors, so health percent is always 1 | 0 for non-actors |
| game-cnd-time | GetCurrentTime | game hour | `WorldState::game_hour` (default 12.0) | [conditions.cpp:417](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L417), [conditions.hpp:110](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.hpp#L110) | impl | — | 12.0 |
| game-cnd-samecell | GetInSameCell | same cell | `cell_of(subject) == cell_of(param1)` from record parents | [conditions.cpp:419-420](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L419-L420) | impl | player cell from world state | 0 |
| game-cnd-faction | SameFaction / SameFactionAsPC / GetInFaction / GetPCInFaction / GetFactionRank | membership by rank list | scans the subject's faction list; `SameFaction` passes when the other's rank for any of those factions is not -1 | [conditions.cpp:255-267](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L255-L267), [:421-434](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L421-L434), [:484-485](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L484-L485), [:488-489](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L488-L489) | impl | **SameFaction is far too permissive** (comment admits "-2 reads as match"); GetFactionRank returns -1 for non-NPCs | -1 / 0 |
| game-cnd-race | SameRace / SameRaceAsPC / GetIsRace / GetPCIsRace | race equality | `race_of` through TPLT traits; player race from world state | [conditions.cpp:187-195](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L187-L195), [:435-438](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L435-L438), [:480-483](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L480-L483) | impl | player base 7 special-cased | 0 |
| game-cnd-sex | SameSex / SameSexAsPC / GetIsSex / GetPCIsSex | ACBS female bit | bit 0 of ACBS flags, player from world state | [conditions.cpp:439-453](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L439-L453) | impl | — | 0 |
| game-cnd-rankdiff | GetFactionRankDifference | difference of two ranks | `a - b` when neither is -1 | [conditions.cpp:454-461](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L454-L461) | impl | "-2 enters the subtraction" per the comment | 0 |
| game-cnd-weather | IsRaining / IsSnowing | weather flag plus precipitation fade | WTHR DATA flag with fade-in/out bytes scaled by 0.0039176475 and compared to the transition | [conditions.cpp:462-472](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L462-L472) | impl | magic constants; outgoing-weather term adds a 0.001 bias | 0 |
| game-cnd-incell | GetInCell / GetInCellParam | cell membership | param1 is the cell; `GetInCellParam` uses param2 as the reference | [conditions.cpp:473-474](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L473-L474) | impl | falls back to the subject when param2 is 0 | 0 |
| game-cnd-class | GetIsClass / GetPCIsClass | NPC_ CNAM class | `npc_trait(..., traits)` CNAM | [conditions.cpp:475-479](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L475-L479) | impl | — | 0 |
| game-cnd-isid | GetIsID | base form comparison | `base_of(subject) == param1`; player base 7 | [conditions.cpp:145-158](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L145-L158), [:486-487](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L486-L487) | impl | a bound object with no REFR is its own base | 0 |
| game-cnd-global | GetGlobalValue | global's value | GLOB record, live override wins; non-GLOB returns -1 | [conditions.cpp:490-499](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L490-L499) | impl | -1 sentinel for a wrong form type (unverified against vanilla) | -1 |
| game-cnd-level | GetLevel | level, optional PC-level multiplier with clamps | ACBS level; flag 0x80 multiplies by the player's level / 1000 and clamps to min/max | [conditions.cpp:500-512](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L500-L512) | impl | reads the player's *record* level, not the calculated one | 0 |
| game-cnd-isref | GetIsReference | reference identity | `subject == param1` | [conditions.cpp:513-514](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L513-L514) | impl | — | 0 |
| game-cnd-wind | GetWindSpeed / GetCurrentWeatherPercent / GetIsCurrentWeather | weather state | world state fields, default 0/1.0/0 | [conditions.cpp:515-520](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L515-L520) | impl | — | 0 / 1.0 |
| game-cnd-playable | GetIsPlayableRace | RACE DATA flag bit 0 | race flags & 1 | [conditions.cpp:521-524](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L521-L524) | impl | — | 0 |
| game-cnd-rela | HasAssociationType / HasFamilyRelationship / HasParentRelationship / GetRelationshipRank | RELA between two bases | a lazily built map keyed by the unordered base pair, first RELA wins; rank mapped as `4 - rank` | [conditions.cpp:310-334](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L310-L334), [:525-549](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L525-L549) | impl | unordered pair key; multiple RELAs collapse | 0 |
| game-cnd-cloud | IsPleasant / IsCloudy | weather flags blended by transition | weighted sum of current and outgoing flags | [conditions.cpp:550-554](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L550-L554) | impl | returns a fraction, not a boolean | 0 |
| game-cnd-owner | IsOwner | XOWN of the cell or reference | XOWN on the subject form, compared to param1 or the player's NPC base | [conditions.cpp:555-559](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L555-L559) | impl | cell ownership not walked | 0 |
| game-cnd-interior | IsInInterior | cell DATA interior flag | world state for the player, cell DATA bit 1 otherwise | [conditions.cpp:560-567](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L560-L567) | impl | — | 0 |
| game-cnd-worldspace | GetInWorldspace | worldspace or FLST of them | cell parent WRLD; param1 may be a FLST | [conditions.cpp:568-580](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L568-L580) | impl | — | 0 |
| game-cnd-isactor | IsActor | reference is an actor | player or ACHR | [conditions.cpp:581-584](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L581-L584) | impl | ACHR only, not ACRE | 0 |
| game-cnd-essential | IsEssential / IsProtected / IsUnique | ACBS flag bits | 0x02 / 0x800 / 0x20 | [conditions.cpp:585-589](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L585-L589) | impl | — | 0 |
| game-cnd-loc | GetInCurrentLoc / GetIsEditorLocation / GetInCurrentLocAlias / LocationHasKeyword | location containment through PNAM | `location_within` walks up to 32 parents; editor location from the cell's XLCN or the grid cell | [conditions.cpp:213-254](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L213-L254), [:590-600](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L590-L600), [:712-713](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L712-L713) | impl | current location prefers the live `ref_location` callback | 0 |
| game-cnd-sameeditor | HasSameEditorLocAsRefAlias | same editor location as an alias, optionally at a keyword | walks both PNAM chains to the keyword, compares | [conditions.cpp:601-620](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L601-L620) | impl | — | 0 when the alias is unfilled |
| game-cnd-reftype | HasRefType | XLRT list | any entry equals param1 | [conditions.cpp:621-625](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L621-L625) | impl | — | 0 |
| game-cnd-linked | HasLinkedRef / IsLinkedTo | XLKR with and without keyword | first XLKR whose keyword matches (0 matches an empty keyword) | [conditions.cpp:335-348](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L335-L348), [:626-629](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L626-L629) | impl | `HasLinkedRef` ignores the keyword; `IsLinkedTo` takes the keyword in param2 | 0 |
| game-cnd-child | IsChild | RACE DATA child flag | race flags & 4 | [conditions.cpp:630-633](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L630-L633) | impl | — | 0 |
| game-cnd-voice | GetIsVoiceType | VTCK or a FLST of them | NPC_ VTCK, player from the race's per-sex VTCK; param1 may be a FLST | [conditions.cpp:634-661](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L634-L661) | impl | — | 0 |
| game-cnd-vendor | GetOffersServicesNow | vendor hours, chest, vendor conditions, location | first faction with VENV hours containing the game hour and a VENC chest | [conditions.cpp:662-681](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L662-L681) | impl | vendor conditions and location are **not tested** (comment) | 0 |
| game-cnd-inlist | IsInList | FLST membership of the base | base form in the FLST | [conditions.cpp:682-694](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L682-L694) | impl | — | 0 |
| game-cnd-reaction | GetFactionCombatReaction | XNAM reaction of one faction to another | scans XNAM for param2 and returns `group_combat_reaction` | [conditions.cpp:695-707](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L695-L707) | impl | neighbours/allies fields ignored | 0 |
| game-cnd-exists | Exists | 1 when the reference exists | `is(subject != param1)` | [conditions.cpp:708-709](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L708-L709) | impl | **looks wrong**: compares the subject with param1 instead of testing existence | 0 |
| game-cnd-keyword | HasKeyword / LocationHasKeyword | KYWD on the base, race or location | base KYWD, then race KYWD; location KYWD | [conditions.cpp:283-291](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L283-L291), [:710-713](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L710-L713) | impl | — | 0 |
| game-cnd-eventdata | GetEventData | reads the current story event member | `param1>>16` is the member code; low 16 bits pick GetIsID (0), IsInList (1), GetValue (2), HasKeyword (3), GetItemValue (4) | [conditions.cpp:723-752](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L723-L752) | impl | 5 of the vanilla member functions | 0 for any other selector |
| game-cnd-vmvar | GetVMQuestVariable / GetVMScriptVariable | reads a script variable | indices 629/630 read the CIS text as the variable name through `ConditionFacts::quest_variable` | [conditions.cpp:800-803](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L800-L803) | impl | index 629 is the quest form, 630 a script; spud treats both the same | 0 |
| game-cnd-graphvar | GetGraphVariableInt / Float | animation graph variables | indices 675/447 through `graph_value` / `graph_real` on the subject | [conditions.cpp:804-811](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L804-L811) | impl | — | 0 |
| game-cnd-default | unsupported function | returns the game's default | falls through the switch to `return 0.0` | [conditions.cpp:719-720](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L719-L720) | impl | every one of ~669 unhandled indices answers 0, which satisfies `== 0` and `< 1` conditions | 0 |

## Support matrix

Counts are measured. Commands are shown, shortened where the same filter repeats.

Condition functions (`src/game/conditions.cpp`):

| set | count | command |
|---|---|---|
| declared available (`k_available`) | 65 | `sed -n '/k_available\[\] = {/,/^};/p' conditions.cpp \| tr ',' '\n' \| rg -c '[A-Za-z]'` |
| constant-zero indices (`k_constant_zero`) | 17 | same pattern for the array; values `36,53,64,79,112,192,225,229,282,306,309,315,437,473,488,520,530` |
| constant-one (`IsWin32`) | 1 | `k_is_win32 = 521` |
| `available()` true, total | **83** | 65 + 17 + 1 |
| functions answered from records in `value()` | 67 | `rg -o 'case [A-Za-z_0-9]+'` over the `value()` span, unique → 68 labels, minus `k_is_win32` |
| answered but reported unavailable | 2 | `comm -13` of the two sets: `HasPerk`, `HasSpell` |
| answered or constant-one, total | 68 | 65 available + HasPerk + HasSpell + IsWin32 |
| named function indices in the `Function` enum | 68 | `sed -n '/^enum Function/,/^};/p' \| tr ',' '\n' \| rg -c '[A-Za-z]'` |

Denominator: Skyrim's condition index space in Mudcrab's own schema enum runs from 0 to 735
(`crates/converter/src/records/families/conditions.json`, highest key `"735": "GetActorWarmth"`),
so at most **736** slots, of which that enum names **407** (measured with
`awk 'NR>=65 && NR<=480' ... | rg -c '":\s*"'`). Against 736 slots spud therefore answers
83 (11.3%) and returns record data for 67 (9.1%). The 736 is schema-derived, not an independent
count of vanilla functions; treat it as an upper bound.

Supported functions (67, the 65 reported available plus HasPerk and HasSpell): GetDistance, GetActorValue, GetBaseActorValue, GetPermanentActorValue,
GetActorValuePercent, GetHealthPercentage, HasPerk*, HasSpell*, GetCurrentTime, GetInSameCell,
SameFaction, SameFactionAsPC, SameRace, SameRaceAsPC, SameSex, SameSexAsPC, GetFactionRankDifference,
IsRaining, IsSnowing, IsPleasant, IsCloudy, GetInCell, GetInCellParam, GetIsClass, GetPCIsClass,
GetIsRace, GetPCIsRace, GetIsSex, GetPCIsSex, GetInFaction, GetPCInFaction, GetIsID,
GetFactionRank, GetGlobalValue, GetLevel, GetIsReference, GetWindSpeed, GetCurrentWeatherPercent,
GetIsCurrentWeather, GetIsPlayableRace, HasAssociationType, HasFamilyRelationship,
HasParentRelationship, GetRelationshipRank, IsOwner, IsInInterior, GetInWorldspace, IsActor,
IsEssential, IsProtected, IsUnique, GetInCurrentLoc, GetInCurrentLocAlias,
HasSameEditorLocAsRefAlias, GetIsEditorLocation, HasRefType, HasLinkedRef, IsLinkedTo, IsChild,
GetIsVoiceType, GetOffersServicesNow, IsInList, GetFactionCombatReaction, Exists, HasKeyword,
LocationHasKeyword, GetEventData. (`*` = not reported available.)

Defaulted to a constant: 17 unknown indices give 0; `IsWin32` gives 1; `GetActorValuePercent` and
`GetHealthPercentage` give 1 for any actor; `GetOffersServicesNow`, `Exists` and `SameFaction` have
placeholder logic. Unsupported: the remaining ~653 indices answer 0 through the switch default.

Quest data coverage:

| item | count | note |
|---|---|---|
| QUST subrecords decoded | 39 sigs | [`quests.cpp:329-471`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L329-L471) |
| scene subrecords decoded | 20 named fields | [`scenes.cpp:37-83`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L37-L83), name-driven |
| dialogue record types | DLBR, DIAL, INFO | [`dialogue.cpp:45-132`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L45-L132) |
| story tree node types | SMEN, SMBN, SMQN | [`story_manager.cpp:54-56`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L54-L56) |
| OnStory event names mapped | 30 | [`story_manager.cpp:16-32`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L16-L32) |
| quest natives bound | 23 `Quest` + 7 alias/location names | [`quests.cpp:1553-1732`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1553-L1732) |
| scene natives | 6 | [`scenes.cpp:635-664`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L635-L664) |
| dialogue natives | 4 | [`dialogue.cpp:947-982`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L947-L982) |
| stage item flags | 2 | complete, fail |
| objective states | 6 | 0-5 |
| alias fill methods | 8 | 0-7 |

## Mudcrab mapping

Mudcrab already decodes these records but has no runtime for them. Measured facts:

- `crates/converter/src/records/families/dialogue.json` defines `QUST` (line 13), `DIAL` (717),
  `INFO` (942), `DLBR` (1212), `SCEN` (1309), `SMBN` (1785), `SMQN` (1841) and `SMEN` (1950).
  `crates/converter/build.rs:11-35` assembles that file and nine other families with
  `src/records/schema.json` into the compiled schema (`crates/converter/src/records/mod.rs:31`),
  so these records are decoded today.
- `crates/converter/src/records/families/conditions.json` defines the 32-byte CTDA struct with
  flag/operator bits, run-on, and a 407-name `function_index` enum through index 735 plus SKSE
  1024-1028.
- The world database has no quest, condition, dialogue or story tables: `db-schema.md` sections
  1-19 cover plugins, records, worldspaces, cells, references, spatial index, land, LOD, scripts,
  FormID map, conversion cache, lights, waters, grass, rooms, emittance, doors, actor models and
  projection omissions (#269 tracks this class of gap; #498 names "quests/dialogue/scenes" and
  "scripts/conditions" as two of the 13 record workstreams, and says those category issues have
  not been created).
- No engine module owns quests. `crates/engine/src/world/` has `database.rs`, `components.rs`,
  `enable_state.rs`; `crates/engine/src/papyrus_runtime.rs` plus
  `crates/shared/src/papyrus_runtime.luau` provide a sandboxed Luau VM with a class registry;
  `docs/specs/converters/pex-to-lua.md` defines the Papyrus-to-Luau mapping. Roadmap 4.3 asks for
  "quest stage state machine synchronized with Luau events and SQLite database record updates"
  (`docs/roadmap/04-gameplay-and-physics.md:19-22`).

| ledger group | Mudcrab today | gap | where it should live |
|---|---|---|---|
| quest record decode | schema decodes QUST in `dialogue.json`; the DB has no `quests` table | no projection, no stage/objective/alias tables, no runtime object | new `quests`, `quest_stages`, `quest_stage_items`, `quest_objectives`, `quest_objective_targets`, `quest_aliases` tables in `docs/specs/converters/db-schema.md`, written by `crates/converter/src/esm/exporter.rs`; FormIDs become FormKeys per #542 |
| quest state machine | nothing | whole subsystem | new `crates/engine/src/quests/system.rs` over `world/database.rs`; state in the SQLite world DB as roadmap 4.3 requires; stage bits and objective states must be save-round-tripped like `crates/engine/src/world/enable_state.rs` |
| stage items and fragments | Papyrus-to-Luau spec covers fragments conceptually (`pex-to-lua.md` §2) | no fragment table, no dispatch, no latent-wait bridge | converter emits fragment rows (quest, stage, item, script, function) from VMAD; engine dispatches through `papyrus_runtime.rs` and must support the latent resume that spud gets from `item_done` |
| objectives and journal | no journal UI; no state | full | `crates/engine/src/quests/objectives.rs` plus a UI source like spud's `GameJournal`; state table mirrors spud's six states |
| aliases and fills | nothing; `references` table has placement data | fill algorithms, reservations, ALFI, create-object refs, instance text | `crates/engine/src/quests/aliases.rs`, using `references`, `cells`, `exterior_spatial` and streaming residency; "loaded area" flag maps to the engine's loaded-cell set |
| conditions | CTDA decode only | evaluator, function table, facts seam | new `crates/engine/src/rules/` (spud's architecture doc names `src/rules/tes5` as the intended home for "Skyrim's rules", [`docs/ARCHITECTURE.md:32`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/ARCHITECTURE.md#L32)); function table generated from `conditions.json`, facts provided by actor/actor-value/weather/global systems |
| story manager | `SMEN`/`SMBN`/`SMQN` decode only | tree walk, event queue, run-once/reset bookkeeping | `crates/engine/src/quests/story.rs`; event queue must be an exclusive Bevy system so quest mutation is single-writer |
| scenes | `SCEN` decode only | phase machine, action lifecycle, random scenes | `crates/engine/src/quests/scenes.rs`; needs actor, package, AI-talk and `look_at` seams |
| dialogue | `DIAL`/`INFO`/`DLBR` decode only | topic list, INFO gating, voice, menu | `crates/engine/src/quests/dialogue.rs` + a UI menu; voice path needs the audio crate |
| authored constants | #264 requires provenance | spud hard-codes weather fade multipliers, `sOr`, time-span defaults, 576 days scaling | all of these must come from GMST rows through the settings table, not literals |

## Port notes

Ordering and lifecycle pitfalls, in the order they bite:

1. **Current stage is a high-water mark.** `set_stage` only raises `current_stage` ([quests.cpp:805](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L805));
   a reset or a `SetStage` back to a lower index leaves it high. `IsStageDone` is the reliable
   query, not `GetCurrentStageID`.
2. **A stage item that fails its conditions still counts as done** ([quests.cpp:838-841](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L838-L841)). Any latent
   waiting on the stage resumes. Copy this or `SetStage` latents hang on conditional stages.
3. **Stage items always run with the player as subject** ([quests.cpp:837](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L837)). If Mudcrab generalises
   the subject, fragments that assume the player will silently change behaviour.
4. **Fill happens before reservation and before scripts exist.** Aliases fill during
   `fill_aliases`, alias effects apply at `finish_promotion` ([quests.cpp:618](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L618)), and only then does
   `init_quest` reset and run the quest ([quests.cpp:661](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L661)). Alias scripts must be bound before init.
5. **ALFI needs two phases.** At fill the ref is written to both aliases; the created-object alias
   gets its real reference only at promotion ([quests.cpp:636-650](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L636-L650)). A one-phase port will hand
   scripts a base form instead of a reference.
6. **External aliases depend on fill order.** `ALEQ`/`ALEA` read the other quest's map directly
   ([quests.cpp:1280](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1280)). `fill_order` exists but is not used to sort; a port that reorders quests
   changes which aliases can fill.
7. **The condition alias scope is RAII.** `Conditions::QuestScope` and `set_aliases` are only valid
   during a fill ([conditions.hpp:131-141](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.hpp#L131-L141), [quests.cpp:1382-1386](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1382-L1386)). Any deferred/async fill in Bevy
   must carry the alias maps explicitly.
8. **Condition evaluation is record-first, facts-second, 0 last.** Live facts win over records
   ([conditions.cpp:355](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L355)). Unhandled functions answer 0, so `== 0` and `< 1` conditions pass by
   default. Mudcrab should log or count every fall-through; that is the single best parity metric.
9. **Two "available" answers exist.** `available()` is a report, not a gate: HasPerk/HasSpell are
   answered but reported unavailable, and 17 indices are reported available while returning 0
   ([conditions.cpp:127-131](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L127-L131)). Only the fill-failure message uses it ([quests.cpp:1412](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1412)).
10. **Story manager uses per-pass stamps** to stop a node running twice per event
    ([story_manager.cpp:193-227](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L193-L227)). The Do All retry recurses once with conditions skipped
    ([story_manager.cpp:321](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L321)). Without the stamp, a cyclic tree loops.
11. **Priority events are invented.** QSTR and AFAV jump the queue ([story_manager.cpp:33-35](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L33-L35), [:123](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L123)). This changes observable ordering against vanilla and should not be copied blindly.
12. **Scenes idle while a menu is open** ([quests.cpp:788](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L788)) and actions whose actor is the current
    conversation speaker are reported complete ([scenes.cpp:466](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/scenes.cpp#L466)). Both are behaviours a port needs
    deliberately, not by accident.
13. **Journal text freezes per instance.** One instance record per promotion, one log slot, alias
    forms stored as bases ([quest_text.cpp:81-111](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L81-L111)). A later `ForceRefTo` updates only the current
    instance ([quest_text.cpp:113-121](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L113-L121)).
14. **`Exists` looks wrong** ([conditions.cpp:708](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L708)) and **`SameFaction` is over-permissive**
    ([conditions.cpp:421-434](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L421-L434)). Do not port either as-is.

Minimal vertical slice that proves the subsystem:

Fixture: one QUST with a start-up stage (2 items, second conditional on `GetIsID` of the player),
one normal stage with `item_complete_quest` and a log line, two objectives (one `ored`), one
reference alias filled by `ALFR` to a placed ref, one location alias filled from it, and one INFO
on a top-level branch whose condition is `GetStageDone`.

Check, in order: start → the alias holds the expected ref → the start-up stage runs both items and
the conditional one is marked done → the normal stage sets the quest completed and appends the log
→ the objective list shows the two objectives in one row with the "or" prefix → the INFO appears in
the dialogue list only after the stage is set → save and load restore stage bits, objective states
and filled aliases.

## Test ideas

All deterministic and headless; no renderer, no audio device, fixed seeds.

1. **Operator matrix.** Six comparison operators × {equal, below, above} against a stub fact
   provider; assert spud-equivalent truth values and that operators 6/7 are false
   ([conditions.cpp:823](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L823)).
2. **Grouping truth table.** Lists of 0, 1, 2, 3 items with every OR-flag pattern, including a
   trailing OR and an all-OR list; assert AND-of-OR-groups and that the empty list is true
   ([conditions.cpp:866](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L866)).
3. **`fails_for_all` equivalence.** For random subject-free lists, assert
   `fails_for_all(list) == forall subject: !evaluate(list, subject)` — the property spud relies on
   to skip scans ([conditions.cpp:848](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L848)).
4. **Run-on table.** Each `RunOn` value with a stub ref graph; assert subject/target swap,
   reference override, linked-reference resolution, event-member resolution, and that
   alias/package/combat-target produce "no subject" failures ([conditions.cpp:754-780](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L754-L780)).
5. **Alias parameter remap.** `condition_aliases` with present and absent aliases; plus the
   `takes_target` fallback when param1 is 0 ([conditions.cpp:786-796](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/conditions.cpp#L786-L796)).
6. **Candidate narrowing equals brute force.** Property test: a random alias condition list either
   yields no candidate set, or every ref it rejects also fails full evaluation. This is the test
   that catches spud's invented optimisation diverging ([quests.cpp:1102](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1102)).
7. **Stage state machine.** Table: running/not running × known/unknown stage × repeat flag ×
   already-done; assert return values, high-water `current_stage`, and that a conditional item is
   still done ([quests.cpp:800-846](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L800-L846)).
8. **Stop and reset.** Quest with a ShutDown stage of 2 items: assert running flips false at once,
   the quest stays in the stopping list until both items finish, and `Reset` cannot restart a
   run-once quest ([quests.cpp:686-734](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L686-L734)).
9. **Story manager tree.** Synthetic SMEN/SMBN/SMQN: ordered vs random children, `max_concurrent`,
   `MNAM`, `Do All` retry after exhaustion, reset days, run-once ([story_manager.cpp:228-331](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/story_manager.cpp#L228-L331)).
10. **Text tags.** Golden files for `<Alias=X>`, `<Alias=X.Cap>`, `<Global=G>`, `<Global=G.Timer>`,
    `<Global=G.Hour12>`, `<Global=G.TimeSpan>`, `<Relationship=A=B>`, a missing alias
    (`sInvalidTagString`), and an unknown tag left as text ([quest_text.cpp:329-395](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quest_text.cpp#L329-L395)).
11. **Journal state mapping.** Six objective states → displayed/completed/failed/active in the
    journal rows, including the `ored` join ([journal_source.cpp:60-110](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/journal_source.cpp#L60-L110)).
12. **Save round trip.** Stage bits, objective states, filled refs/locations, instance records and
    player objectives survive save→load ([quests.cpp:1734](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1734), [:1820](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1820)).

Fixtures to extract from vanilla data:

- A start-game quest with several stages and at least one start-up stage (the opening quest is the
  obvious candidate; confirm the exact stage indices from the record before pinning them).
- A quest with `ored` objectives, so the OR join path has real text.
- A quest with an `ALFR` reference alias plus an `ALFA` location alias, to exercise both fill maps.
- An INFO with a non-zero reset hours value and one with `say once`, for the memory paths.
- A vendor faction (`VENV`/`VENC`) for `GetOffersServicesNow`.
- A weather record with precipitation fade in/out, for the rain/snow blend.
- Two RELA records (parent/child and association type), for the relationship conditions and the
  `<Relationship>` tag.

## Open questions

1. Whether the stage log is written before or after the fragment in vanilla. Spud writes the log,
   then calls the fragment ([quests.cpp:850-878](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L850-L878)); no reference source was available to confirm.
2. Whether vanilla's "current stage" also never decreases. Spud's high-water behaviour is asserted
   only in its own code.
3. Whether an INFO's `quest dialogue conditions` really are the QUST top-level CTDA list that
   precedes the `NEXT` marker ([quests.cpp:345-348](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L345-L348) maps them that way).
4. Whether `IsRaining`/`IsSnowing` thresholds (0.0039176475, +0.001) reproduce vanilla's
   precipitation blend. They look like derived 1/255 constants, `unverified`.
5. Whether Skyrim's condition index space really ends at 735. The 736-slot denominator comes from
   Mudcrab's schema enum, not from an independent count of vanilla functions.
6. Whether `GetEventData` has exactly the five member functions spud implements, and whether
   event member codes are limited to six.
7. Whether the dialogue topic list really caps at nine rows. Spud's comment says so
   ([dialogue.cpp:498](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L498)) but the code does not cap ([dialogue.cpp:504-557](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/dialogue.cpp#L504-L557)).
8. What Mudcrab wants to do with the invented `alias_candidates`/`fails_for_all` fast paths: they
   are a real performance win on 4.3's "tens of milliseconds" fill ([quests.cpp:1100-1102](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/game/quests.cpp#L1100-L1102)), but they
   are also the most likely source of divergence.
9. Issue **#566** had not been read when this report was written, so its content
   is unknown here.


