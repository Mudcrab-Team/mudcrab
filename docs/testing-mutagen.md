# Placed-reference comparison with Mutagen

This ignored test compares base FormIDs, positions (0.01 Creation units), rotations
(0.0001 radians modulo a full turn), scale, record type, and winning plugin.
Deleted winning overrides must be absent. Extra REFR/ACHR rows fail; PGRE and
other types are outside this oracle mode. NaN and infinity fail comparisons.

Use a `placed-lo` JSONL produced by Mutagen for the same plugin files and order.
The JSONL contains no checksums: choosing the matching oracle remains the caller's
responsibility. The test checks database plugin checksums against the supplied
game files and independently assigns full/light slots from TES4 headers.

```sh
MUDCRAB_MUTAGEN_ORACLE=/path/to/mutagen-placed-lo.jsonl \
MUDCRAB_MUTAGEN_DATA='/path/to/Skyrim Special Edition/Data' \
MUDCRAB_MUTAGEN_PLUGINS=/path/to/plugins.txt \
cargo test --locked -p converter --test mutagen_oracle \
  v122_current_references_match_mutagen_winning_overrides -- --ignored --nocapture
```

By default, the test builds a database with the current converter in a temporary
directory. Set `MUDCRAB_MUTAGEN_DATABASE` to compare an existing database in read-only
mode instead; `MUDCRAB_MUTAGEN_PLUGINS` is then unnecessary. No source game files
or existing database are modified. Neither the oracle nor game assets belong in Git.

RE ledger F0008's September 2026 sample has 930,375 matching live references and
270 deleted overrides (217 plain, 50 persistent, three with other flags).
Its 44 additional Mudcrab rows are PGRE. These are format comparisons; they do
not establish retail runtime behavior. Counts are observations, not test allowances.
