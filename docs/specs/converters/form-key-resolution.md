# Converter FormKey resolution

FormKey uses Mutagen's owner/local identity concept. A converter run assigns
pack-local plugin numbers independently of load priority. Source words select
an owner through the containing file's original MAST list before the checked
`LegacyIdView` packs a global u32. Existing u32 entry points and stored formats
retain their signatures and widths.

For a MAST list of length N, selectors below N name that master, N names self,
and selectors above N reject the record header. The readers count and warn for
that candidate and preserve earlier complete winners and valid neighbors.
Optional invalid links clear locally. GMST headers have a separately named
conventional identity because settings override by EditorID; this does not
authenticate an original owner and is never the reference-resolution rule.

`EsmParser::merge_plugins_form_keys` accepts an already recovered load order
without full/light slot caps. Its records retain native source words separately
from resolved header, group and recognized link identities. It does not publish
these source words as legacy canonical fields. The default typed and legacy
readers resolve links before checked u32 projection and merge winners by FormKey.
The existing selection and public u32 reader APIs retain their legacy admission
policy; callers of the native extended path supply their complete accepted order.

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
This establishes a reproducible physical layout without intentionally changing
logical rows. Verification compares every schema object and sorted rows of every
table, including virtual-table shadow tables. Non-SQLite serialization retains
its established ordering. Database-cache receipts and generated-artifact seals
hash physical DB bytes and can therefore differ. Existing cache payloads remain
valid against their own stored hashes; the rebuilt producer changes the database
cache identity. Independent asset-cache formats and keys do not change here.
