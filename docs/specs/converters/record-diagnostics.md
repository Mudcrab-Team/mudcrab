# In-house record diagnostics

The in-house reader keeps its console and `inhouse-reader-diagnostics.json`
summaries small: per-plugin counts and first examples. Complete diagnostic
details are scalar rows in the optional `record_diagnostics` SQL table. This
table is tooling provenance and is refreshed in the same transaction as source
records and override history.

Bounded decoder and adapter examples are selected by a fixed provenance order
within each plugin, rather than observation order. Known physical source offsets
sort before missing offsets, followed by source FormID, canonical FormID, record
signature, bounded field path, link occurrence, stage, category, action, link
value and bounded reason. Field paths compare lexically; full-text digests break
ties between equal bounded prefixes. Decoder `first_example` is the smallest
category example under the same order. Replay merges retain that provenance;
manually authored summaries without it use a deterministic bounded-text fallback.
Counts and complete event identities are unchanged. Complete SQL diagnostics
already publish in `(plugin_id, event_key)` order, independently of summary
selection.

Diagnostic events include the zero-based supplying `plugin_id`, physical
`source_record_offset`, native `source_form_id`, resolved `form_id` when known,
record signature, stage, field/link occurrence, category, reason and action.
Join `plugin_id` to `record_overrides` or the load-order metadata using their
existing numbering contract. A skipped record may have no canonical `records`
row; group/plugin framing failures may have no record identity. NULL means the
identity is unavailable, rather than a fabricated FormID.

Decode-stage field occurrence indices refer to native framed subrecord order.
Target-validation indices refer to the surviving canonical fields. Deferred
admission omissions use an `omitted` ordinal within the rejected field list.
Stage and the explicit occurrence label distinguish these views. Nested member names,
array indices and per-field link ordinals distinguish repeated equal references.
Runtime target-validation link values are global; decode-stage optional link
values retain their original file-relative representation.

Actions report the existing recovery decision: `skipped`, `cleared`,
`kept_earlier_version`, `projected`, or an explicit adapter action such as
`unresolved`. Observations do not change admission, override order, reference
resolution or terrain policy. Deferred decoding merges newly discovered events
without duplicating the same physical occurrence's first-pass diagnostics.

The event journal uses temporary disk storage, with a fixed small SQLite cache.
There is no cap on event count and no in-memory list of every problem. Field and
reason text are bounded UTF-8 strings; truncation is explicit in the table.
The diagnostic identity uses full text so two long reasons with an equal visible
prefix remain distinct. Publication streams rows in deterministic order and
replaces older diagnostic snapshots atomically. Storage failures are
infrastructure failures and cannot silently produce an apparently complete
diagnostic table.

Diagnostics have an independent format version in the reader metadata identity.
Changes to this optional table do not advance runtime database, cell-cache or
asset producer versions. Existing packs without the table remain usable by
runtime consumers. A normal in-house export supplies the new tables and metadata
identity. Its broad configuration hash changes; current per-kind asset settings
and source proofs remain unchanged. Adapter-11 metadata explicitly recognizes
the exact adapter-10 combined proof for unchanged asset settings and independently
eligible producers. Metadata-only migration retains its complete original
reader/schema identity rather than relabeling those bytes. Other older in-house
identities remain unsupported by this explicit compatibility pair.

New main-database snapshots also store existing lists as scalar child rows.
`inhouse_plugin_masters(plugin_id, master_index, master_name)` preserves the
authored MAST order with contiguous zero-based indices. A TES4 history row with
no child rows represents a plugin with no masters. `record_overrides` no longer
contains `plugin_masters` JSON. Lazy history readers use the child table when
present and can still read the former JSON column in older packs.

`inhouse_unresolved_terrain_textures(form_id, texture_form_id)` stores each
distinct nonzero unresolved texture key for its LAND owner.
`inhouse_terrain_layers` retains its original normalized `layer_data` BLOB,
cell and load order, without the former `unresolved_texture_ids` JSON column.
Both child snapshots are replaced within the provenance transaction. Runtime
terrain selection and both cache encodings remain unchanged.

In-house runtime script projections also replace their former
`scripts.properties_json` list with scalar `script_properties` headers and
`script_property_values` rows. The parent retains its `(form_id,script_name)`
key and VMAD bytes. Property and array element indices preserve order, while
nullable scalar columns retain each typed value, including exact float bits.
Type-zero properties and empty arrays keep their header without value rows.
This script snapshot refresh belongs to the record transaction; archive-only
bundles leave these runtime projections empty. See [the script table contract](db-schema.md#9-script-attachments-scripts).
Legacy-reader output keeps its existing projection contract.

For example, inspect cleared links without parsing a JSON list:

```sql
SELECT plugin_id, source_record_offset, source_form_id, form_id,
       field, occurrence, link_value, reason
FROM record_diagnostics
WHERE category = 'link' AND action = 'cleared'
ORDER BY plugin_id, source_record_offset, field, occurrence;
```
