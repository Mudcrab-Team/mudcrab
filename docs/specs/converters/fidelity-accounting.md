# Converter fidelity accounting (#498)

`record-fidelity` inventories the compiled record schema and compares preserved winning source
subrecords with the canonical `records.data` archive. It does not certify the converter or engine.

Usage classifications and consumer implementation ownership come from the canonical record audit
and its category workstreams in [fidelity-ownership.json](fidelity-ownership.json). That registry
pins the 9,860-leaf matrix and all five spawned consumer threads. Existing category issues and PRs
keep their owners. This branch supplies retention/publication repairs and fidelity enforcement;
it does not reimplement movement, interior/local lighting, water, material substitutions or snow.

On branches without `scripts/workspace.py`, use the installed shared-storage bootstrap as described
in `AGENTS.md`. Redirect reports into task-owned shared `evidence/`, never into a registered pack.

```sh
python3 scripts/workspace.py cargo --isolated fidelity-498 run --locked -p converter \
  --bin record-fidelity -- schema > /absolute/evidence/record-declarations.json
python3 scripts/workspace.py cargo --isolated fidelity-498 run --locked -p converter \
  --bin record-fidelity -- audit /absolute/immutable-pack > /absolute/evidence/record-storage.json
```

## Declared denominator and CI

`schema` expands the exact `SCHEMA_BYTES` used by the decoder. Every record declaration,
container, array member, union alternative, common hook and shared definition has an ordinal path,
descriptor, inherited format source and contract hash. Repeated names retain separate paths.
Standalone definitions, common hooks and expanded record fields are separate namespaces; their
counts must not be added together and called a count of distinct physical source fields.

`schema-baseline.json` stores root counts and fingerprints of every expanded field path and
contract. The ordinary converter library suite checks it. New, removed, reordered or changed
fields, flags, enum mappings, offsets, source citations and record ordering rules fail that check.
After reviewing the changed declarations and their owning fidelity gap, regenerate it with:

```sh
python3 scripts/workspace.py cargo --isolated fidelity-498 run --locked -p converter \
  --bin record-fidelity -- baseline > crates/converter/src/records/fidelity/schema-baseline.json
```

Updating this denominator acknowledges a schema change; it grants no semantic acceptance.
Every declaration currently says `semantic_support: unverified`. The baseline is not a
publication/consumer disposition map. A publication change with unchanged schema needs behavioral
fixtures; a counter cannot replace them. The projectile/hazard fixture changes one model and checks
the exact join used by the engine while unchanged placements stay unchanged. A separate schema
guard checks that every declared placed transform reaches the common publication predicate.

## Observed denominator

`audit` opens an immutable SQLite URI without journals or SHM writes. It rejects a nonempty WAL
or rollback journal at entry and before returning the report. A legacy pack or mismatched producer
identity is also rejected explicitly. An older pack can supply its exact
retained built-in `records-schema.json` through `--schema FILE`. For a pack without add-ons its
bytes must match `record-reader.json` directly. For an add-on pack they participate in the original
ordered bundle fingerprint described below. The same option inventories the selected built-in
schema with `schema`. Input and audit-binary
hashes accompany the report. Keep the pack settled and immutable throughout the read; before/after
file hashes detect drift but are not a filesystem snapshot or protection against concurrent writers.

The denominator is **winning preserved in-house payloads**. It excludes original record headers,
GRUP framing, overridden/deleted/rejected versions and bytes never admitted to that table. It
does not infer absent records from its own inputs. Both directions of source/canonical row absence
are counted. Report version 3 adds `coverage`: matched, source-only and canonical-only row counts
with an explicit status. A canonical row without retained source evidence fails the CLI. Two empty
tables produce status `empty`, which proves no retention. Source-only rows remain observable because
the producer can preserve an admitted source whose defensive runtime projection is rejected.
Matched rows do not certify byte equality or semantic support. This reader restriction does not claim legacy coverage.

Each original payload is framed independently, including `XXXX` lengths. Duplicate signatures and
zero-byte occurrences count separately. A malformed suffix contributes its exact byte length and a
bounded sample with payload offset and SHA-256; the safely framed prefix still counts. Samples are
bounded to eight per category; totals are exact. Record/subrecord keys use hexadecimal signature
bytes so arbitrary unknown tags cannot collide through replacement-character decoding.

For each record/signature, the report counts source and canonical occurrences, byte lengths, exact
byte matches and canonical occurrence deficits. Deficits are summed per record: an extra field in
another record cannot cancel a dropped field. Multiset matching avoids guessing which repeated
occurrence survived; whole-record sequence equality also exposes reordering. A changed byte string
may be an intentional FormID remap, text conversion, validation or another transformation. It is
reported as `changed_or_omitted_source_occurrences`, never automatically called data loss.

`declared_signature` means only that some schema branch declares that tag. It does not prove that
the observed occurrence selected that branch or that its nested fields decoded successfully.
Unknown signatures remain observable even when byte-preserved. Neither raw retention nor canonical
publication establishes scalar SQL publication, runtime reads or final behavior.

## Retained schema add-ons

Storage report versions 2 and 3 reconstruct the pack's add-on declarations from the exact documents in
`inhouse_schema_addons`. It never reads the original configured paths. Ordinals must be contiguous
from zero; names, exact-byte hashes, count/size limits and document validation must agree. The
ordered identities and the selected built-in bytes must reproduce `record-reader.json`'s complete
bundle fingerprint. Missing, changed or reordered evidence fails instead of falling back to the
current schema or treating an add-on as unknown data.

`builtin_schema_sha256` identifies the selected base; `schema_sha256` identifies the original
complete bundle, with `schema_addons` retaining its ordered document identities. `addon_declarations`
enumerates new records/definitions and independent extensions. Extensions use `extension/<TYPE>/...`
paths because their matching cursor is independent of the built-in ordered fields. They are not
presented as ordered suffixes. Declared-signature accounting includes both new record types and
extensions, including repeated and zero-byte occurrences.

The retained-base constructor is for inspection. Production configuration still freezes compiled
built-ins and its configured add-ons through the existing path; this does not authorize an old
schema to drive a new conversion. Add-on declarations remain `semantic_support: unverified` and
lie outside the canonical built-in usage matrix's denominator.

## NIF unread-byte accounting

`nif-audit` report versions 3 and 4 include exact unparsed-region/byte totals and at most eight samples per
file. Each sample names the block, source file offset, length and SHA-256. Known-block tails,
whole unhandled blocks, parse failures, parser panics and unread file trailers remain separate
reasons. A trailer can contain legitimate native footer data; unread does not mean malformed.
The containing file hash binds every sample to recoverable source bytes. The source NIF remains
unchanged. This reports gaps rather than implementing their decoded or rendered semantics.
These counters describe the generic NIF block parser. Separate collision or animation readers may
consume an unhandled block through another path; its presence alone is not a confirmed fidelity loss.

Directory traversal errors now fail the command rather than silently reducing its denominator.
Version 4 counts skipped non-regular directory entries and retains at most eight path samples.
An explicitly supplied symlink to a directory is traversed as the audit root, matching ordinary
root path resolution. Descendant symlinks remain unfollowed; their target files and directory
descendants are outside the denominator.
`total_file_bytes` counts enumerated regular NIF inputs. `unaccounted_file_bytes` separately totals
the whole sizes of files with inspection errors; `unparsed_bytes` covers only successfully inspected
files. Neither count measures bytes a parser consumes and discards.
Structural/material failures remain explicit file results; their unavailable internal breakdown
must not be read as a zero-byte proof. No new glTF bytes or NIF producer identity are implied by
these inspection-only counters.

## Remaining closure work

The versioned [gap ledger](fidelity-gaps.json) records current ownership, confirmed mechanisms,
fixes and uninspected domains. #498 remains open. Required remaining evidence includes nested source
field occurrences, application of the canonical publication/consumer mappings, broader mod corpus
coverage, both readers, full/light mod
identity, original plugin history, all non-record formats and cold/warm/rebuild/repair/resume runs.
An official-data storage audit is a measurement of that pack, not qualification of this source head.
