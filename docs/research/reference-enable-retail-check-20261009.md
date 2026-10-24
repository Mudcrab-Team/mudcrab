# Pending clean-save enable-state comparison

> Prepared with Codex. No retail or Mudcrab runtime observations are claimed here.

PR #189 remains a draft. This check starts from
`8247499a1de8273cf03c4742d68a1dad2c4fba97` and integrates main
`c39449b5a8b6a62c7c3c43bb60164e8ba6911839` without rewriting history.
The [earlier evidence note](reference-enable-state-20261005.md) defines the
documentary rule, PlayerRef assumption, and limits of the bootstrap snapshot.

## Available evidence

No accessible clean retail session was established during these checks.
The sample manifest is retained locally because it contains game-derived
placements; the public tool regenerates it from separately supplied inputs.

The database contains 930,419 references, world schema 7, and 80 plugins.
All 80 plugin SHA-256 values stored by the converter matched the files in the
supplied Data directory during this check. This establishes source-file identity;
it does not establish save state or retail behavior.

The locally retained observation manifest selects
20 of 22 case/space combinations. Each selected sample carries its placement,
source FormID mapping, complete bounded parent chain, and static prediction.
It covers own enabled/disabled flags, both parent override directions, inversion
of either parent state, PlayerRef ancestry, siblings with differing own flags,
pop-in bit 1, and reserved XESP bytes in interiors and exteriors. Both
non-inverted PlayerRef cases have no sample within the selection bound. The
selected PlayerRef children invert the assumed enabled PlayerRef, and both are
authored at Z = -30000; their absence from a screenshot cannot prove the rule.

Every retail and Mudcrab observation remains `pending`, with null results and
evidence. Python predictions describe the Rust contract; they are not engine
execution. The snapshot digest covers the sampled metadata and plugin list,
not every byte or record in the database.

## Reproduce the manifest check

Run from the repository root; neither command modifies the database:

```sh
python3 scripts/reference-enable-observations.py /path/to/modern_assets/skyrim_world.db \
  --verify /path/to/reference-enable-observations.json
python3 scripts/reference-enable-observations.py /path/to/modern_assets/skyrim_world.db \
  --output /tmp/reference-enable-observations.json
```

Selection inspects at most 64 candidates per case and 256 ancestors per chain,
with a 120-second SQLite deadline. It does not imply a full census when no
sample is selected. Verification requires the unchanged source manifest; record
runtime results in a separate copy and retain the source manifest.

## Bounded runtime checklist

1. Use a separate clean retail profile and an untouched save. Record the game
   version, save hash, new-game/quest position, plugin order and hashes, and
   whether scripts had already changed a sampled reference. Match the manifest's
   source list before comparing. Resolve each retail FormID from the recorded
   plugin/internal ID and the actual load order; do not assume a converted ID
   is the retail ID. Confirm the reference identity before observing it.
2. Cap the run at the 20 selected case/space combinations, their two recorded
   siblings, and 10 minutes per engine. Stop at the cap and leave unobserved
   cases pending. Reload the untouched save between cases. Do not enable,
   disable, delete, move, or resurrect a sampled reference or its parent.
3. For each case, record the child's effective state and its ancestors' states
   using reference-level inspection, plus a screenshot or log with the runtime
   FormID and cell. Inspect PlayerRef `00000014` explicitly. Check both siblings
   in each differing-child-flags case. If a script or quest altered the state,
   mark that sample unsuitable for the static comparison rather than changing
   the expected result.
4. For Mudcrab, record the exact binary commit and package identity. Inspect
   `ReferenceRow.initially_enabled` from the database worker, then record whether
   the common spawn path suppresses the reference before entity/model/light/
   collider work. An enabled prediction means eligible for spawning; it does
   not guarantee a visible mesh. Keep raw database rows, enabled placements,
   and spawned entities as separate counts.
5. For exteriors, use the manifest's `load_key` worldspace/grid. It is derived
   from placement XY because persistent references may belong to a holding
   cell. For interiors, exercise `CellKey::Interior(interior_cell_id)` and the
   common interior spawn path. This head has no direct interior startup CLI;
   an actual runtime entry path or explicit inspection harness is still needed.
6. Add an identified, separately documented retail fixture for a non-inverted
   PlayerRef child in each space. Do not fabricate official samples or treat
   the synthetic PlayerRef unit test as retail evidence. Record the fixture's
   plugin hash and metadata in the result copy.
7. Compare measured retail state, measured Mudcrab state, and the prediction.
   Explain mismatches with captured parent/child metadata and save state. Keep
   the draft gate open for pending, unsuitable, or unexplained cases. Human
   review still follows the repository's policy and workflow.

## Database layout coordination with #128

The reviewed #128 head is `386267dc6569154036d6c45ebb04a91dc7c9e5aa`.
Both original branches appended fields at offset 25. Combining their suffixes
without changing the decoder can fail cell loads or misread door destinations.

This branch leaves the shared columns at 0–24, reserves #128's nine door slots
at 25–33 as NULL, and reads enable inputs at 34–36. The separate global-parent
query still starts at 0. The new regression supplies distinct destination and
arrival values in the reserved slots and checks enable decoding and both cell
paths. No door tables, transitions, or destination logic are implemented here.

When combining #128, replace the reserved NULL suffix with its optional door
projection at the same offsets. Preserve its absent/malformed-table handling,
the mutable enable resolver, tuple return, global parent lookup, and all main
SPEC rows. Re-run legacy/partial enable-column cases and both cell paths with
door metadata present and absent; check exact door arrival values separately.

## Spawn/profiling coordination with #206

The reviewed #206 head is `b7e31e66e1d6694405cb57d73de3f7f69cceb030`.
Keep #189's disabled guard before entity creation and #206's scene-observation
hook. Preserve spawned and suppressed counters, completion-latency categories,
and the existing 32-models-per-frame admission setting. Enabled placement count
and unique observed scene count have different denominators.

The combined branch still needs a tracing-enabled check proving a disabled
modeled/lit/linked-door reference schedules no work or scene observation, an
all-disabled cell loads, shared enabled models produce one observed scene ID,
and unload releases pending handles. #206 is not merged by this task.

Dynamic quest/save state, revival, and exclusion from unconditional object LOD
remain separate follow-ups. This observation plan does not implement them.
