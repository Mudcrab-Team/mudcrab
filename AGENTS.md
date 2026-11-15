# Working in Mudcrab

## Core game files and artifact reuse

- Use `~/SkyrimSE` as the existing core game installation. Its game data is in `~/SkyrimSE/Data`. Do not copy or delete this installation.
- Always inspect existing builds, conversions, packs, and their receipts before creating replacements. Reuse compatible artifacts in place when their source, settings, schema, and required checks match the task.
- Reuse partial conversion results through the supported cache and provenance checks. Do not rebuild unchanged stages only because another thread owns the existing output.
- Delete stale generated artifacts after checking ownership, active processes, leases, open files, downstream consumers, and required retained evidence. Age alone does not prove that an artifact is stale.
- Use guarded managed-cache pruning for eligible builds. Preserve active artifacts, immutable registered packs, original game files, source changes, and evidence needed for current claims.

## Storage and worktrees

Read `docs/contributing/storage.md` before building, converting assets, launching the engine,
or retiring a branch. On older branches without that document, read the installed copy at
`<shared storage>/bin/storage.md`.

- Worktrees hold source. Do not create a new `target/`, game-data copy, extracted tree,
  converted pack, or ingestion cache inside a worktree. Do not copy another worktree's
  `target/` to speed up a build.
- Use `python3 scripts/workspace.py paths` to discover this repository's shared storage.
  All worktrees resolve the same directory: `.mudcrab-local` beside the common Git directory.
  Respect an explicitly configured `MUDCRAB_WORKSPACE_ROOT`; do not invent a second root.
- Run Cargo through `python3 scripts/workspace.py cargo <command> ...`. This leases one of
  two reusable build slots, sets `CARGO_TARGET_DIR`, and checks the storage budget. If both
  slots are busy, wait or inspect the existing build. Do not bypass the limit with another
  output directory. Use `cargo run` through this wrapper for launches so the lease covers
  the engine's entire lifetime. Do not launch a binary directly from a shared slot.
  For a script that builds and then launches binaries, wrap the entire foreground command
  with `workspace.py exec --isolated <task-label> -- <command> ...`. Do not detach children.
- For exact-head review, acceptance evidence, or benchmarks, use
  `python3 scripts/workspace.py cargo --isolated <task-label> ...` from the intended checkout.
  This keeps validation separate from shared development builds and disables incremental
  compilation. Use `--release` for performance measurements. Record the source head,
  dirty diff, command, and binary checksum with the results.
- On older branches without `scripts/workspace.py`, use the installed bootstrap:
  `python3 "$(git rev-parse --path-format=absolute --git-common-dir)/../.mudcrab-local/bin/workspace.py" ...`.
  With a custom root, use `$MUDCRAB_WORKSPACE_ROOT/bin/workspace.py` instead.
- Before converting, inspect `packs.json` in shared storage and reuse a compatible registered
  pack through `pack-path <name>` and the engine's existing `--assets <absolute-path>` option.
  Register existing packs in place; do not copy them to satisfy a relative path. Check source
  inventory, plugin order, converter revision, format/schema, and encoding settings before reuse.
- Registered packs are immutable by policy. Never convert, repair, or write runtime outputs
  into a registered pack. Changed inputs/settings need a separate pack and provenance.
  Keep conversion caches beside their pack. Share source game data in place. Hard links are
  only safe when every writer honors immutability; use APFS clones for writable experiments.
- Before a full conversion or large experiment, run `preflight --output <path> --needed-gib N`
  with a realistic peak estimate, including staging, cache, and copy fallback. Default reserve
  is 20 GiB; managed build budget is 60 GiB. These checks are not a filesystem quota.
- Put temporary experiments in shared `scratch/` and retained reports, receipts, and required
  binaries in shared `evidence/`, with a task label and owner. Reuse a running conversion;
  do not start another because a terminal lost its output.
- At task completion, run `settle`, record retained artifacts and their owner/purpose, then
  inspect `prune`. `prune --apply` only retires idle, tool-owned build directories after a
  lease and open-file check. Packs, scratch, evidence, original game data, and legacy builds
  are never automatically deleted. Retire task-owned scratch separately after checking consumers.
- Before deleting a legacy artifact or worktree, check local/untracked changes, active agents,
  processes, open files, dependent work, and retained evidence. A merged PR or old timestamp
  alone does not authorize deletion. Never use blanket `git clean -fdx`, wildcard deletion,
  or `cargo clean` before each build. Report filesystem free space before and after cleanup;
  APFS directory sizes do not prove reclaimed bytes.

## Validation and publication

Keep source/test evidence, native runtime, performance, and visual parity claims separate.
Do not launch games, alter remote systems, publish GitHub text, push, or merge unless the
user's task authorizes it. Follow `docs/AI_POLICY.md` when submitting work.
