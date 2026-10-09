# In-house record schema, version 1

`schema.json` and the independently owned family modules are authored data for
MudCrab's generic Skyrim Special Edition record decoder. The base describes the
existing converter's fields; family overlays add native record data and typed
links. A record entry does not claim complete gameplay support for its type.

The entries are independently written from format facts in the pinned xEdit
`xedit-4.1.5f` definitions, UESP, and MudCrab's existing consumer contract. The
schema contains no translated Pascal, generated xEdit definitions, or Mutagen
source or XML. xEdit is a facts reference, not a runtime dependency. Each record
has a `source`; fields and shared definitions override that source where needed.
Nested fields inherit the nearest source. The `sources` table records the
primary reference, revision and URL. Names are our own snake_case names.

## Envelope and validation

The JSON envelope contains `version` (currently `1`), a `sources` object,
`definitions` (named reusable fields), `common_fields` (explicit hooks for
otherwise opaque record types), and an ordered `records` array. A record
has a four-character `signature`, `source`, ordered `fields`, and optional
`allow_unordered` (default false). Verified format flags enable unordered matching
for TES4, CELL, CONT, REFR, ACHR and the eight projectile/hazard placed types.
These flags are format facts from the pinned definitions and the named parameter
in `wbInterface.pas`; they are not inferred from the definition's display order.
WRLD, LAND and the other verified initial native record types remain ordered.
Legacy ACRE remains conservative because no permitted SSE definition exists.

Rust includes the JSON and validates it before decoding plugin data. Invalid
schema is a programming/configuration error. Malformed plugin data is a bounded
field, record or plugin diagnostic: it must not abort conversion of valid
neighboring records. JSON parsing needs no additional dependency beyond the
converter's existing serde_json.

Build-time assembly loads the items, actors, magic, and dialogue overlays under
explicit record ownership sets. Each family may replace its own partial base
entry or add an assigned entry, but cannot edit common fields or replace another
family's record. Shared definitions must agree exactly. The definitions-only
conditions module owns `condition` and `conditions`; it cannot add records or
unrelated definitions. After assembling every overlay, assembly activates
documented pending CTDA/CIS1/CIS2 fields and `pending_conditions` references
through that component. This avoids editing separately owned family files and
does not alter unrelated opaque fields.

## Fields and values

| Property | Meaning |
| --- | --- |
| `signature` | Four-character subrecord signature. Absent on struct members and groups. |
| `name` | Stable decoded value name. |
| `kind` | `u8`, `i8`, `u16`, `i16`, `u32`, `i32`, `u64`, `i64`, `f32`, `zstring`, `lstring`, `form_id`, `bytes`, `struct`, `array`, `union`, or `group`. |
| `size` / `sizes` | Exact byte width or explicitly accepted widths, when present. |
| `offset` | Byte offset within a struct or array element; default zero. |
| `members` | Struct or array element fields, with explicit offsets. |
| `fields` | Ordered group children or union alternatives. |
| `repeat` | Allow repeated subrecords or restart an entire repeated group. |
| `repeat_terminated` | Default false. A repeating group with an initial signature and final empty marker can restart only at its initial signature after that marker. |
| `targets` | Allowed record signatures for a FormID link. `ANY_` explicitly permits an unchecked target type for legacy ACRE. |
| `definition` | Reuse a named field, with occurrence-specific signature/name overrides. |
| `count` | Array count rule: `{ "kind": "fixed", "value": N, "stride": S }`, `{ "kind": "remaining", "stride": S }`, or `{ "kind": "prefix_u32", "stride": S }`. |
| `decider` | Named Rust hook for a contextual layout. |
| `flags` | Object mapping numeric bit masks, as decimal strings, to names. |
| `enum` | Object mapping numeric values, as decimal strings, to names. |
| `source` | More specific format-fact provenance. |

All numeric bytes are little endian. `form_id` is exactly four bytes and retains
its typed target information through file-relative to load-order remapping.
Zero is an absent link. Zero-size `bytes` fields describe valid empty markers.
A target signature in a winning runtime record is checked against the resolved
record catalog; it is not inferred from the FormID's high byte. Unknown flag
bits and enum values retain their numeric value.

Record header identities follow the existing MudCrab owner policy: a source
index at or beyond the number of masters names the current plugin. Indices
beyond that count are retained for compatibility with shipped exceptions such
as Skyrim.esm's GMST0123C00E. Optional field links use the stricter bounded
master-index check and become zero when invalid. Duplicate or colliding
normalized record IDs in one plugin preserve the first record and diagnose the
later occurrence locally. This compatibility does not establish universal
validation of modded record headers.

TES4 headers are retained separately in `ReadResult.headers`, preserving decoded
HEDR and master-list fields. Their ONAM overridden-form list is historical
metadata: this phase remaps its file-relative links but exempts it from final
winner-catalog target validation. An overridden or deleted form can legitimately
be absent from that catalog. No runtime projection currently consumes these
headers; the exemption does not extend to ordinary winning-record links.

`zstring` consumes an inline zero-terminated byte string. A fixed-size zstring,
such as a 260-byte STAT LOD slot, stops at its first zero but preserves the slot
width. `lstring` selects an inline string in an unlocalized plugin or a
four-byte external string ID in a localized plugin using TES4's localized flag.
String-table lookup belongs to the reader/integration context; an ID alone is
not a display name. No schema field assumes UTF-8 for all plugin text.

`bytes` marks opaque data. It is deliberate preservation, not a claim that
links inside the payload have been decoded. `struct` members use byte offsets;
padding is explicitly represented as bytes. Arrays validate the count, stride,
remaining byte divisibility, and bounds before accessing any element. Nested
arrays, such as LAND's 1,089 height deltas, apply their offset to the parent.

## Ordered matching and groups

For ordered record types, the input cursor visits subrecords in file order and
the schema cursor advances
through optional fields to the next matching signature. An unknown signature
does not advance the schema cursor: report it and continue. A known field
encountered after its ordered position is also a diagnostic; do not silently
reinterpret it as a different field with the same signature.

For the explicitly unordered types, recognized fields can occur before or after
their position in the display-oriented definition. Unknown signatures still
produce bounded diagnostics. Preserve input occurrence order and array/group
semantics; enabling the record flag does not declare array element order or
variable-layout context irrelevant. Official REFR records place XSCL after
activate parents or XESP, and XRDS after light/map fields. Those are legitimate
physical variants, as is the existing dummy light's NAME/DATA/XRDS/XESP order.

A group spans several subrecords without adding bytes of its own. Its children
are optional unless a decoder-specific constraint requires them. A repeated
group restarts its child cursor when a new occurrence begins; a repeated leaf
accepts another occurrence at its current position. Keep duplicate values and
their occurrence order. TES4 master groups associate each MAST with its DATA;
LAND layer groups associate ATXT with its VTXT. Grouping prevents repeated
shared signatures from being treated as a global last-value map.

Typed decoding is separate from source preservation. `records.data` retains its
existing rkyv encoding and contains validated canonical subrecord bytes for
consumers. The auxiliary `inhouse_source_records` table holds the original
decompressed payload, including unknown subrecords and padding; its embedded
FormIDs remain file-relative. Malformed fields are omitted from canonical
consumer bytes while their source payload remains available as provenance.
These omissions do not invent a value or damage valid neighbors.

Localized field occurrences are retained even when their key is zero or absent
from the selected plugin's string bank. Canonical bytes contain an empty
NUL-terminated text value in those cases. `inhouse_localized_fields` records the
canonical field index, signature, field name, bank, original string ID, winning
load order, nullable resolved text and status (`resolved`, `null_id`, `missing`).
An explicitly resolved empty string remains distinct from a null or missing ID.
Runtime nullable text projections omit only unresolved occurrences during
extraction, so a missing NPC name remains NULL. Original ID bytes remain in the
source payload; no decimal ID or raw four-byte key is presented as text.
Ordinary inline strings and English localization banks decode as Windows-1252;
valid UTF-8 byte sequences do not override that native encoding. VMAD names and
values retain their separately specified UTF-8 decoder. Canonical runtime text
is UTF-8, with original encoded bytes preserved in the source payload or bank.

Multiple live LAND identities can share one parent cell. Both the database's
cell-keyed terrain projection and cache select the record with the highest
winning load order, then the latest physical source-record offset. FormID is
only a deterministic final tie-breaker for callers without physical order.
`inhouse_terrain_source_order` retains accepted LAND offsets for the full
pipeline and diagnostic exporter. All winning LAND identities and their native
fields remain in the records/source tables, including unselected alternatives.

`inhouse_terrain_layers` retains normalized, globally remapped original texture
assignments for accepted LAND records, with their cell, load order and unresolved
texture IDs. Source duplicate slots and authored NULL assignments are resolved
before optional target clearing. `cell_cache_preserved.rkyv` uses these original
assignments. The runtime cache replaces unusable texture keys with zero while
retaining each selected assignment's quadrant, slot and blend weights; these
placeholders must not be normalized again as native NULL assignments. Both
caches select the same complete LAND record. Original VTXT padding and authored
NULL weights remain in the independent native source payload.

Decoded records report `payload_complete` and deduplicated `rejected_fields`.
An incomplete subrecord boundary preserves the safely framed prefix and marks
the payload incomplete; it cannot establish whether a required field was absent
in the undecodable remainder. Known fields rejected during value decoding or
ordered matching are distinct from fields that the source omitted; unknown
signatures remain unexpected rather than rejected known fields. The public typed reader retains these facts for
consumers rather than guessing missing values.

The runtime adapter checks required terrain and movement semantics on each
candidate before committing it as an override winner. An unusable candidate
leaves its earlier usable predecessor and records a bounded diagnostic. TES4
headers remain separate, and header-only deletions still apply. Genuine
texture/color-only LAND without VHGT uses the shared cache's established zero
heights; an incomplete LAND payload or rejected authored VHGT cannot take that
fallback. The decoder's source metadata supports this decision without using
the original payload as an alternative runtime projection.

## Decider hooks

| Hook | Context and alternatives |
| --- | --- |
| `size` | Pick an exact size alternative, e.g. STAT DNAM 8/12 bytes or XPWR reference-only/reference-and-flags 4/8 bytes. |
| `gmst_value` | First character of EDID: `s` localized string, `i` signed integer, `f` float, `b` unsigned Boolean. No fallback type for an unknown prefix. |
| `legacy_linked_reference` | XLKR 4 bytes contains only a reference; 8 bytes contains keyword/reference followed by reference. |
| `cell_grid` | XCLC 8 bytes has two i32 coordinates; 12-byte SSE layout adds one byte of land flags and three padding bytes. |
| `movement_speeds` | SPED 40 bytes has ten floats; 44 bytes adds rotate-while-moving-run. |
| `water_visual` | DNAM 228 bytes has 57 four-byte slots; 232 bytes adds flowmap scale. |
| `alternate_textures` | MODS/MO2S/MO4S has a u32 entry count, then entries of u32 name length, name bytes, TXST FormID and u32 shape index. Validate every variable boundary. |
| `vmad` | Decode version1-5 scripts, all scalar/array property types and object formats1/2, owner-specific INFO/PACK/PERK/QUST/SCEN fragments, and quest alias scripts. Remap every typed object FormID while preserving other canonical bytes. |

The alternate texture hook is attached to an otherwise opaque `bytes` field.
It produces bounded typed entries and remaps their TXST links;
the hook must not treat a missing or malformed entry as a whole-run error. VMAD
uses the independently authored bounded `records/vmad.rs` hook and returns a
typed struct while retaining its canonical byte representation. Script/property
names and values use the native UTF-8 encoding. Primary and alias objects retain
their signed alias numbers and unused words; only FormIDs are rewritten. The
shared `vmad` definition applies to explicit VMAD occurrences and to the
top-level `common_fields` occurrence on otherwise opaque record types. That
fallback preserves the existing generic script consumer contract; it does not
provide full semantic coverage for those record types. Each owner-specific
fragment tail must be fully consumed, including QUST aliases and their separate
script headers. Every set event flag bit frames one named fragment; bits with
unknown meaning retain their numeric identity. Malformed fields, unsupported
layouts or incomplete tails omit only VMAD. Original source bytes remain in the
auxiliary provenance table. The existing exporter still writes primary scripts;
its compatibility parser also accepts explicit absent type0 properties. Typed
fragment/alias decoding does not introduce script execution or alias runtime APIs.
Future contextual layouts add Rust hooks explicitly rather than executable
code or expressions embedded in schema data.

REFR and the shared projectile/hazard placed definitions permit XPWR's older
four-byte reference-only prefix: the pinned struct fact requires only its first
member. The eight-byte form adds u32 reflection/refraction flags. Both forms
retain exact widths, resolve the REFR link, and preserve unknown flag bits. The
official inventory found 1,314 reference-only XPWR fields in Skyrim.esm; omitting
the optional-tail fact incorrectly dropped usable fields in the first prototype.

## Examples

These abbreviated examples show the actual format; the complete entries and
sources live in `schema.json`.

TXST is a group of ordered texture slots. The mask and glow assignments are
not the order used by the old exporter's runtime-column projection.

```json
{"signature":"TXST","fields":[
  {"signature":"EDID","name":"editor_id","kind":"zstring"},
  {"signature":"OBND","definition":"bounds"},
  {"name":"textures","kind":"group","fields":[
    {"signature":"TX00","name":"diffuse","kind":"zstring"},
    {"signature":"TX01","name":"normal_gloss","kind":"zstring"},
    {"signature":"TX02","name":"environment_mask_subsurface_tint","kind":"zstring"},
    {"signature":"TX03","name":"glow_detail","kind":"zstring"},
    {"signature":"TX04","name":"height","kind":"zstring"},
    {"signature":"TX05","name":"environment","kind":"zstring"},
    {"signature":"TX06","name":"multilayer","kind":"zstring"},
    {"signature":"TX07","name":"backlight_mask_specular","kind":"zstring"}
  ]}
]}
```

XESP is a subrecord structure shared by placed types. Its flags are one byte;
the trailing three bytes are padding and cannot contribute to the flags value.

```json
{"name":"enable_parent","kind":"struct","size":8,"members":[
  {"name":"reference","kind":"form_id","offset":0,"targets":["PLYR","ACHR","REFR","PGRE","PHZD","PMIS","PARW","PBAR","PBEA","PCON","PFLA"]},
  {"name":"flags","kind":"u8","offset":4,"flags":{"1":"opposite_parent","2":"pop_in"}},
  {"name":"unused","kind":"bytes","offset":5,"size":3}
]}
```

STAT separates bound coordinates from the model group and its size-dependent
direction-material structure. The abbreviated union illustrates a typed link
inside a larger subrecord.

```json
{"signature":"STAT","fields":[
  {"signature":"EDID","name":"editor_id","kind":"zstring"},
  {"signature":"OBND","definition":"bounds"},
  {"name":"model","kind":"group","fields":[
    {"signature":"MODL","name":"path","kind":"zstring"},
    {"signature":"MODT","name":"texture_hashes","kind":"bytes"},
    {"signature":"MODS","name":"alternate_textures","kind":"bytes","decider":"alternate_textures"}
  ]},
  {"signature":"DNAM","name":"direction_material","kind":"union","decider":"size","fields":[
    {"name":"legacy","kind":"struct","size":8,"members":[
      {"name":"max_angle","kind":"f32","offset":0},
      {"name":"material","kind":"form_id","offset":4,"targets":["MATO"]}
    ]},
    {"name":"sse","kind":"struct","size":12,"members":[
      {"name":"max_angle","kind":"f32","offset":0},
      {"name":"material","kind":"form_id","offset":4,"targets":["MATO"]},
      {"name":"flags","kind":"u8","offset":8,"flags":{"1":"considered_snow"}},
      {"name":"unused","kind":"bytes","offset":9,"size":3}
    ]}
  ]}
]}
```

REFR uses the shared XESP definition and the shared six-float transform. Its
display definition places XSCL before the ownership/enable-parent portion, but
the record's verified unordered flag permits other physical positions. Other
subrecords between the fields below are omitted only from this example.

```json
{"signature":"REFR","allow_unordered":true,"fields":[
  {"signature":"EDID","name":"editor_id","kind":"zstring"},
  {"signature":"NAME","name":"base","kind":"form_id","targets":["STAT","DOOR","ACTI","TREE","CONT"]},
  {"signature":"XSCL","name":"scale","kind":"f32"},
  {"signature":"XESP","definition":"enable_parent"},
  {"signature":"DATA","definition":"transform"}
]}
```

The example's shortened base-target list illustrates the format. The actual
entry includes all base targets documented by the pinned Skyrim definition.

## Initial coverage and limits

The initial base has 42 entries: TES4, WRLD, CELL, LAND, REFR, ACHR, eight projectile/hazard
placed types, legacy ACRE, STAT, sixteen other ordinary world-model types,
ARMO, LIGH, WATR, TXST, LTEX, GRAS, NPC_, RACE, MOVT, and GMST.

Existing runtime tables determine the initial typed subset. Ordinary world-model
types describe EDID, VMAD, OBND and the model group; their remaining gameplay
fields remain verbatim in the auxiliary source payload rather than canonical
`records.data`. Explicit `bytes` fields remain opaque canonical fields.
NPC_ describes identity, race, class, name
and several common compound links; it does not claim full actor conversion.
NPC_ SPLO actor effects admit SPEL, SHOU and LVSP targets, as specified by the
pinned TES5 shared actor-effect definition at lines4092-4093 and its NPC use at
line10942. A spell-only target list would clear valid shouts and leveled spells.
RACE covers identity/name and WKMV/RNMV movement links. WATR's known visual
slots have primitive types, while its unknown slots remain four-byte opaque
values. LAND normal/color triplets retain exact-sized bytes rather than implying
decoded normal units. VTEX retains the existing converter's four-byte legacy
link-array contract; the pinned Skyrim LAND definition uses BTXT/ATXT layers and
does not establish VTEX as an SSE field. ACRE has no Skyrim SSE definition in the permitted source;
its conservative entry follows the existing MudCrab parser only and leaves the
base target type unchecked. Its XRDS radius is a legacy consumer field, not an
assertion about an SSE record type. REFR and ACHR allow XRDS in multiple physical
positions, including after DATA. The projectile/hazard placed definitions do not admit XRDS;
the generic exporter's attempt to read it does not extend those source formats.

The existing exporter already writes model-bearing bases and raw records; those
are not new engine capabilities introduced by the schema. The in-house projection
corrects physical TXST slot semantics, uses ARMO MOD2/MOD4 world models, and reads
XESP flags separately from padding. Texture slots can change shader roles: TX06
means multilayer and TX07 means backlight mask/specular, rather than universally
meaning a detail or specular image. Runtime column naming remains an adapter
contract and does not redefine those source facts.

No complete mod compatibility, all 126 Skyrim record types, gameplay execution,
or visual parity is established by this document. Those claims
require decoder tests and comparison reports for the final implementation.
Optional REFR room/patrol groups reuse INAM with different target types. The
flattened matcher needs group context to distinguish every such nonconsumer
case; their presence in the initial schema does not establish complete
context-sensitive link semantics.

## Quest/dialogue overlay and shared conditions

The dialogue overlay describes QUST, DIAL, INFO, DLBR, DLVW, SCEN, SMBN, SMQN,
SMEN, MESG, and LCTN. Repeated groups retain quest stages/logs, objectives,
reference and location aliases, dialogue responses, and scene phases/actions in
physical order. Same-signature fields have distinct role names, including scene
start-phase indices and timer durations. Group matching still exports flat
ordered leaves, rather than a persistent nested group tree.

A repeated group with an explicit final empty marker may declare
`repeat_terminated: true`. Its initial signature is required to enter the
group. Once its final marker is consumed, only that initial signature starts
another occurrence; other tags advance beyond the group. SCEN phases and
actions use this boundary so root quest/index and legacy script fields retain
their roles after HNAM phase-end or ANAM action-end, including scenes with
zero actors/actions and an absent optional NEXT separator. Unmarked repeats
retain their existing matching. The authored phase/action boundary attributes
change the generated schema fingerprint, invalidating previous reader caches.
Terminated groups require ordered matching; schema validation rejects their
use in a record with `allow_unordered: true` and a repeated initial leaf.

Localized quest log CNAM uses DLSTRINGS; INFO NAM1 uses ILSTRINGS. Other declared
quest/objective, topic, prompt and button text uses STRINGS. Native INFO ENAM
keeps the exact encoded reset-hours integer. Display and reference-tool
conversions are comparison policies, not replacements for its native bytes.
DIAL's child-group timestamp and unknown fields likewise belong to the GRUP7
header and must not be confused with the DIAL record's version-control header.

A malformed INFO response TRDT omits only the members in its matched response
group with bounded field diagnostics. A new response or unrelated group ends
that omission. Malformed text beneath a valid TRDT omits only that text, keeping
the usable response and neighboring records.

INFO records also retain their containing GRUP7 label independently of TPIC.
The typed reader and JSON diagnostic export expose the resolved DIAL owner,
the native file-relative label, and the source group/record byte offsets.
An absent, deleted, null or wrong-kind owner is cleared with a bounded link
diagnostic while the INFO record remains usable. Native labels and offsets
remain available for source verification. This metadata does not change the
RawRecord/rkyv or runtime database contract, and does not infer a merged
dialogue execution order from physical source order or PNAM alone.

The shared CTDA layout is 32 bytes. It retains padding and the combined
flags/operator byte. Parent-payload selectors interpret the comparison as f32
or a GLOB link, and select both parameter types using an explicitly authored
function-index registry. Alias and package-data substitutions apply only to
the native object-reference, actor, and package categories. GetEventData's
first slot contains two u16 values; VATS parameter two depends on its own
first-slot selector. Unknown function indices and unknown VATS selectors retain
opaque parameter bytes without guessing references.

Only run-on mode 2 interprets the offset-24 slot as a reference FormID. Other
modes preserve that slot as an unused u32; the signed offset-28 parameter holds
the run-on alias/package/event index. CIS1 and CIS2 are independent native
zero-terminated strings belonging to the preceding CTDA. A safely framed
malformed or out-of-order CTDA and its immediately following CIS1/CIS2 are
omitted with bounded field diagnostics, preserving other conditions and records
through publication. The reader identity also includes the decoder/adapter
behavior version so outputs predating this recovery policy cannot prove reuse.
Invalid optional links are cleared through the existing remap/target-validation path.

The assembled phase-3 schema activates shared conditions in all family overlays
and uses the attached-script component for complete VMAD fragments and aliases.
The components retain their separate schema and parser ownership. Loading
these schemas and conditions does not
establish all other phase-3 owners, a complete mod-list audit, or runtime quest
and dialogue execution.
