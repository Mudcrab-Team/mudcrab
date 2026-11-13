# Converter FormKey resolution

FormKey uses Mutagen's owner/local identity concept. A converter run assigns
pack-local plugin numbers independently of load priority. Source words select
an owner through the containing file's original MAST list before the checked
`LegacyIdView` packs a global u32. Existing u32 entry points and stored formats
retain their signatures and widths.

For a MAST list of length N, selectors below N name that master and selectors
at or above N name self, for every record, field and link type, including VMAD.
Occurrences above N produce counted warning summaries rather than individual
log lines. This matches xEdit's reference lookup, which clamps the selector to
the self entry ([public implementation](https://github.com/TES5Edit/TES5Edit/blob/f5c00f3fa3ee39511185515802647246c807f759/Core/wbImplementation.pas#L3276-L3291)).
Mutagen also falls back to its current mod for indices beyond the appended
self entry ([public implementation](https://github.com/Mutagen-Modding/Mutagen/blob/b55de3715229bca562f99baa3dcae5c001f812d7/Mutagen.Bethesda.Core/Plugins/Masters/SeparatedMasterPackage.cs#L130-L166)).
Missing named masters and invalid local widths remain distinct failures. GMST
still overrides by EditorID; source provenance separately marks an out-of-range
selector as unproven original ownership. A projected collision cannot replace an
earlier complete candidate. No per-record vanilla exception exists.

The named `LegacyTypedReaderProjection` retains the old typed NULL projection only
for optional NAVM ONAM words past the master list. Its scope is the record signature
and physical field path, with no plugin or record-ID allowlist. The official data
contains float-looking ONAM words (for example `3F00003F`);
[xEdit still defines ONAM as FormIDs](https://github.com/TES5Edit/TES5Edit/blob/xedit-4.1.5f/Core/wbDefinitionsTES5.pas#L7794-L7806),
so a different layout is unproven. This publication adapter keeps
official output logically identical pending separate layout work. Native ownership
remains self. Other optional links resolve self in both readers before their existing
target checks. The typed reader builds
a source-header catalogue so forward self links survive, while absent optional
self targets with no observed source header retain NULL and their prior source-context
diagnostic. Deferred replay and lazy history use the same observed-header authority,
including rejected/deleted source versions. XLOC keys and history runtime IDs also
resolve to self. Source provenance still records unproven original ownership.
Checked optional packing failures clear locally; required failures reject one candidate.

Malformed mods intentionally change relative to the old writer: past-master CELL
XOWN/XCWT and required REFR NAME survive as self links; typed enable-parent and
other optional links also keep self when their targets are usable. Signatures and
stored widths remain stable. Explicit plugin paths with invalid identity names
are skipped with warnings, together with their dependents; usable neighbours continue.

`EsmParser::merge_plugins_form_keys` accepts an already recovered load order
without full/light slot caps. Its records retain native source words separately
from resolved header, group and recognized link identities. It does not publish
these source words as legacy canonical fields. The default typed and legacy
readers resolve links before checked u32 projection and merge winners by FormKey.
The existing public u32 reader APIs retain their legacy slot limits;
malformed-selector recovery changes as described above.
`convert_selected_form_key_plugins` and the `formkey-records` CLI use the same
official/CCC selection and dependency recovery with slot exclusions disabled.

`EsmParser::convert_form_key_plugins` additionally requests legacy publication.
For more than 254 full or 4096 light plugins it returns the complete native
winner set and `LegacyWriterOutcome::Unavailable` with one per-run notice,
leaving the database destination untouched. No session stand-ins are allocated.
Publishing extended identities and full schema-decoded resolved references belongs
to the writer/reader transition; the native link list here covers the legacy
reader's recognized layouts and is not a versioned resolved-reference sidecar.

Identity traversal explicitly compares original owner kind (full before light),
owner ordinal within that kind, then local ID. This matches unsigned legacy ID
order within Skyrim's limits. Winning supplier priority remains separate and
continues to select overrides. SQL still stores legacy IDs and keeps its existing
numeric ordering. LAND retains supplier priority and native offset precedence.

The SQLite writer sorts previously unordered CELL, terrain and emittance writes.
The previous CELL pass used an unordered map, so database bytes already varied
from run to run. Fixed ordering establishes a reproducible physical layout without
intentionally changing logical rows. Verification compares every schema object and sorted rows of every
table, including virtual-table shadow tables. Non-SQLite serialization retains
its established ordering. Database-cache receipts and generated-artifact seals
hash physical DB bytes and can therefore differ. Existing cache payloads remain
valid against their own stored hashes; the rebuilt producer changes the database
cache identity. Independent asset-cache formats and keys do not change here.
