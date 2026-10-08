# In-house record schema, version 1

`schema.json` is authored data for MudCrab's generic Skyrim Special Edition
record decoder. It describes the fields the existing converter consumes, plus
selected typed links needed for correct remapping. A record entry does not claim
complete gameplay support for its record type.

The entries are independently written from format facts in the pinned xEdit
`xedit-4.1.5f` definitions, UESP, and MudCrab's existing consumer contract. The
schema contains no translated Pascal, generated xEdit definitions, or Mutagen
source or XML. xEdit is a facts reference, not a runtime dependency. Each record
has a `source`; fields and shared definitions override that source where needed.
Nested fields inherit the nearest source. The `sources` table records the
primary reference, revision and URL. Names are our own snake_case names.

## Envelope and validation

The JSON envelope contains `version` (currently `1`), a `sources` object,
`definitions` (named reusable fields), and an ordered `records` array. A record
has a four-character `signature`, `source`, ordered `fields`, and optional
`allow_unordered` (default false). No current record opts into unordered matching.

Rust includes the JSON and validates it before decoding plugin data. Invalid
schema is a programming/configuration error. Malformed plugin data is a bounded
field, record or plugin diagnostic: it must not abort conversion of valid
neighboring records. JSON parsing needs no additional dependency beyond the
converter's existing serde_json.

## Fields and values

| Property | Meaning |
| --- | --- |
| `signature` | Four-character subrecord signature. Absent on struct members and groups. |
| `name` | Stable decoded value name. |
| `kind` | `u8`, `i8`, `u16`, `i16`, `u32`, `i32`, `u64`, `f32`, `zstring`, `lstring`, `form_id`, `bytes`, `struct`, `array`, `union`, or `group`. |
| `size` / `sizes` | Exact byte width or explicitly accepted widths, when present. |
| `offset` | Byte offset within a struct or array element; default zero. |
| `members` | Struct or array element fields, with explicit offsets. |
| `fields` | Ordered group children or union alternatives. |
| `repeat` | Allow repeated subrecords or restart an entire repeated group. |
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
A target signature is checked against the resolved
record catalog; it is not inferred from the FormID's high byte. Unknown flag
bits and enum values retain their numeric value.

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

The input cursor visits subrecords in file order. The schema cursor advances
through optional fields to the next matching signature. An unknown signature
does not advance the schema cursor: report it and continue. A known field
encountered after its ordered position is also a diagnostic; do not silently
reinterpret it as a different field with the same signature.

A group spans several subrecords without adding bytes of its own. Its children
are optional unless a decoder-specific constraint requires them. A repeated
group restarts its child cursor when a new occurrence begins; a repeated leaf
accepts another occurrence at its current position. Keep duplicate values and
their occurrence order. TES4 master groups associate each MAST with its DATA;
LAND layer groups associate ATXT with its VTXT. Grouping prevents repeated
shared signatures from being treated as a global last-value map.

Typed decoding is separate from raw preservation. Keep the safe original
payload, including unknown subrecords and padding, for the existing
`records.data` contract; emit supported typed values and validated canonical
bytes for consumers. Malformed fields can be omitted from those consumer bytes
without inventing a value or damaging valid neighbors.

## Decider hooks

| Hook | Context and alternatives |
| --- | --- |
| `size` | Pick an exact size alternative, e.g. STAT DNAM 8/12 bytes. |
| `gmst_value` | First character of EDID: `s` localized string, `i` signed integer, `f` float, `b` unsigned Boolean. No fallback type for an unknown prefix. |
| `legacy_linked_reference` | XLKR 4 bytes contains only a reference; 8 bytes contains keyword/reference followed by reference. |
| `cell_grid` | XCLC 8 bytes has two i32 coordinates; 12-byte SSE layout adds one byte of land flags and three padding bytes. |
| `movement_speeds` | SPED 40 bytes has ten floats; 44 bytes adds rotate-while-moving-run. |
| `water_visual` | DNAM 228 bytes has 57 four-byte slots; 232 bytes adds flowmap scale. |
| `alternate_textures` | MODS/MO2S/MO4S has a u32 entry count, then entries of u32 name length, name bytes, TXST FormID and u32 shape index. Validate every variable boundary. |

The alternate texture hook is attached to an otherwise opaque `bytes` field.
It may additionally produce bounded typed entries and remap their TXST links;
the hook must not treat a missing or malformed entry as a whole-run error. VMAD
remains opaque in this schema and uses MudCrab's existing parser in the exporter.
Future contextual layouts add Rust hooks explicitly rather than executable
code or expressions embedded in schema data.

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

REFR uses the shared XESP definition and the shared six-float transform. XSCL
precedes the ownership/enable-parent portion in the full ordered definition;
other subrecords between the fields below are omitted only from this example.

```json
{"signature":"REFR","fields":[
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

There are 42 entries: TES4, WRLD, CELL, LAND, REFR, ACHR, eight projectile/hazard
placed types, legacy ACRE, STAT, sixteen other ordinary world-model types,
ARMO, LIGH, WATR, TXST, LTEX, GRAS, NPC_, RACE, MOVT, and GMST.

Existing runtime tables determine the initial typed subset. Ordinary world-model
types describe EDID, VMAD, OBND and the model group; their remaining gameplay
fields are preserved as opaque data. NPC_ describes identity, race, class, name
and several common compound links; it does not claim full actor conversion.
RACE covers identity/name and WKMV/RNMV movement links. WATR's known visual
slots have primitive types, while its unknown slots remain four-byte opaque
values. LAND normal/color triplets retain exact-sized bytes rather than implying
decoded normal units. VTEX retains the existing converter's four-byte legacy
link-array contract; the pinned Skyrim LAND definition uses BTXT/ATXT layers and
does not establish VTEX as an SSE field. ACRE has no Skyrim SSE definition in the permitted source;
its conservative entry follows the existing MudCrab parser only and leaves the
base target type unchecked.

The existing exporter already writes model-bearing bases and raw records; those
are not new engine capabilities introduced by the schema. The in-house projection
corrects physical TXST slot semantics, uses ARMO MOD2/MOD4 world models, and reads
XESP flags separately from padding. Texture slots can change shader roles: TX06
means multilayer and TX07 means backlight mask/specular, rather than universally
meaning a detail or specular image. Runtime column naming remains an adapter
contract and does not redefine those source facts.

No complete mod compatibility, full VMAD typed-link coverage, all 126 Skyrim
record types, or visual parity is established by this document. Those claims
require decoder tests and comparison reports for the final implementation.
