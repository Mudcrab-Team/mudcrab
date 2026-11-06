# Pre-attribution-correction snapshot

This directory holds the four N003 files that the attribution correction changed:
`claims-imagespace.json`, `imagespace-audit.md`, `intake-summary.md` and
`corpus-receipt-20261010-02.json`. `snapshot-receipt.json` pins their hashes and records why
the snapshot was taken: the root review found older N002 assertions attributed to N003 in P07
and P15, so this state precedes the correction and is excluded from claim-ledger discovery.

Nothing was rewritten when the files were copied, so `intake-summary.md` is a byte-for-byte
copy of `note-0003/intake-summary.md` and its relative links still resolve in the original
location, one directory up, rather than from here:

- `source-sections.json`, `claims-native-lighting.json`, `claims-mod-interventions.json` and
  `input-receipt.json` are in `../../` (`note-0003/`).
- `../world-model.md` and `../corpus-receipt-20261010-02.json` are in `../../../`
  (`research-notes/`).

A link in the copied summary that does not resolve from this directory is therefore not
missing evidence. Files unchanged by the correction are stored once, in the note directory.
