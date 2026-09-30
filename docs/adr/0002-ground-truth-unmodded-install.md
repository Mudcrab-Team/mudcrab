# ADR-0002: Ground truth is an unmodded game installation

- **Status:** Accepted
- **Date:** 2026-09-18

## Context

Format validation needs a trustworthy reference for retail Skyrim SE data. Mod-managed
installations (for example MO2 "Stock Game" directories or mod packs) can contain modified loose
files such as body replacements or animation changes, so they cannot be treated as representative
of the retail formats.

## Decision

The canonical format reference is an unmodded Skyrim Special Edition installation. Modified or
mod-managed copies are excluded from validation. Real samples are extracted to `/tmp` only and
never committed. Opt-in tests use `OPENSKYRIM_SKYRIM_DATA` and `OPENSKYRIM_*_FIXTURE` and stay
`#[ignore]`d so CI never needs proprietary data.

## Consequences

- Confirmed formats: BSA v105 (`0x69`, flags `0x87`, 24-byte folder records) and ESM `TES4` header
  version 44 (`0x2C`).
- NIF `20.2.0.7` / user 12 / bethesda 100 and PEX 3.2 are re-verified from unmodded extracts
  before the NIF/ESM writers land.
- Documentation warns that mod-managed directories must not be used as a reference.
