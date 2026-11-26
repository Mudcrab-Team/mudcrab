# Shared storage for local development

Git worktrees already share Git objects. Large Rust builds and game assets do not share
storage automatically. Use the workspace entrypoint so opening another branch does not
silently create another full build or conversion.

Requires Python 3.9+, Git, and Rust. The entrypoint uses only Python's standard library.
CI can keep its existing ephemeral Cargo directories.

## Paths and ordinary builds

```sh
python3 scripts/workspace.py setup
python3 scripts/workspace.py paths
python3 scripts/workspace.py cargo check --locked --workspace
python3 scripts/workspace.py cargo build --locked --profile quick --bin engine
python3 scripts/workspace.py cargo test --locked -p dummy-content
```

Default storage is `<main checkout>/.mudcrab-local`, derived from the common Git directory
rather than the current linked checkout. It survives removal of a linked worktree. Set
`MUDCRAB_WORKSPACE_ROOT` to one dedicated persistent directory if you need another disk;
all worktrees and lifecycle hooks must use the same override.

| Directory | Purpose | Retirement |
| --- | --- | --- |
| `builds/slot-0`, `builds/slot-1` | Two reusable Cargo output directories | `incremental` trimmed above a cap; idle pruning |
| `pins/` | `--keep` pins for isolated builds | Expire by themselves |
| `builds/isolated-*` | Validation builds owned by a checkout and task | Retired after a successful check or test, when superseded, or by `prune` |
| `packs/` | Named converted packs and adjacent conversion caches | Explicit owner review |
| `scratch/` | Temporary task-owned experiments | Owner checks consumers and removes finished work |
| `evidence/` | Reports, checksums, receipts, selected binaries | Retain what supports current claims |
| `bin/` | Installed bootstrap, agent rules, and this guide | Refresh with `install` |

The wrapper holds a process lease for the entire Cargo command, including `cargo run` and
tests. Matching compiler/dependency/configuration keys get first choice of a free slot;
other builds reuse either idle slot. There are two slots total, not two per branch or key.
Cargo decides which artifacts need rebuilding. Leases are released by the OS after exit.

Cargo gives a path package (a workspace member or a `vendor/` crate) the same fingerprint in
every checkout, and judges freshness by mtime. It can therefore report another checkout's
output as `Fresh`. When a slot last served a different checkout, the wrapper removes the
slot's fingerprints for all packages that do not come from a registry or git, before the
command starts. Cargo then rebuilds those packages and the packages that depend on them.
The wrapper prefers the slot that last served your checkout. `cargo fmt` removes nothing.
An older copy of `workspace.py` does not do this, so use `--isolated` from a branch that
carries one. If the wrapper does not recognize Cargo's output layout, it stops the command.
When both slots are busy, the command fails without allocating another directory. Retry
when a slot becomes free. Do not run slot binaries directly; another build can replace them.

For scripts that invoke Cargo and then launch the produced binaries, lease the whole
foreground campaign instead of its individual builds:

```sh
python3 scripts/workspace.py exec --isolated phase2 -- pwsh -File scripts/phase2-profile.ps1
```

The PowerShell acceptance and visual-baseline runners and `capture-lod-phase1.sh` do not call
this entrypoint themselves. Wrap the whole campaign with `exec --isolated`, as shown above.
They keep their existing ephemeral output paths on CI.
Managed campaigns put reports under shared `evidence/`. Do not detach background processes
from a leased command; keep the parent alive until every child using its build directory exits.

The wrapper overrides inherited `CARGO_TARGET_DIR` and refuses Cargo output/config/manifest
overrides. It runs from the selected checkout and defaults development debug information
to level 1; an explicit `CARGO_PROFILE_DEV_DEBUG` remains respected. It does not install or
enable a compiler cache. An existing `sccache`/`kache` wrapper remains supported, but its
separate cache needs its own size limit. Rust binaries and incremental state still consume
space when using a compiler cache.

## Exact-head validation

```sh
python3 scripts/workspace.py cargo --isolated pr123 build --locked --release --bin engine
python3 scripts/workspace.py cargo --isolated pr123 test --locked -p converter
```

Isolated directories are owned by checkout, task label, and source commit. A dirty working
tree can change under the same commit; record the diff and do not edit during validation.
Incremental compilation is disabled for isolated commands. The entrypoint prints the
absolute target directory. Preserve a required binary and its checksum under a named
`evidence/` directory before retiring its build. Use `--release` for measurements; the
`quick` profile is for development. A passing build does not prove runtime or visual parity.

## Asset reuse

Inspect `<storage>/packs.json` before creating another pack. Register an existing completed
pack in place, including a pack outside shared storage:

```sh
python3 scripts/workspace.py register-pack vanilla-v1 /absolute/path/to/existing-pack
assets=$(python3 scripts/workspace.py pack-path vanilla-v1)
python3 scripts/workspace.py cargo run --locked --profile quick --bin engine -- --assets "$assets"
```

Registration requires `conversion-manifest.json` with `complete: true` and
`integration-report.json` with `passed: true`. It records their hashes and rejects changing
an existing name to another pack. `pack-path` rejects changed manifests. Registration is a
reference registry, not a content-addressed asset store or a full artifact checksum check.
Use the converter's full package validation when file integrity needs proving. Published
packs remain immutable by workflow policy; the tool does not chmod a user's existing pack.

Before reuse, compare the source inventory, plugin order, MO2 selection, converter revision,
schema, and encoding settings with the intended run. Retain those details and the conversion
command beside each new pack. Branch names alone do not establish compatibility.
Use `<storage>/packs/<descriptive-version>` for a new conversion. The converter keeps its
ingestion cache at `<output>.assets-cache`; keeping outputs outside worktrees also shares
that cache across callers using the same output. Do not point concurrent conversions at
one writable cache, or repair a registered baseline in place. The engine already accepts
an absolute `--assets` path, so a worktree does not need its own asset copy or symlink.

For writable variants, APFS clones can initially share blocks while keeping writes separate.
Hard links share writes and must only be used under an enforced immutable-file contract.
Never hard-link mutable build directories or databases as a general deduplication scheme.

## Archive extraction staging

The converter writes temporary archive payloads beside their destinations. It keeps the
run registry and lock in a private state directory outside the extraction root, so the
root itself can be read-only when the destination directories are writable.

- On Unix, the preferred directory is `$XDG_STATE_HOME/.mudcrab-extraction-state` when
  `XDG_STATE_HOME` is an absolute path. Otherwise, it uses `~/.mudcrab-extraction-state`.
  If it cannot create a registry there, it tries `.mudcrab-extraction-state-<effective UID>`
  under the system temporary directory, usually `$TMPDIR` or `/tmp`.
- On Windows, the directory is `%LOCALAPPDATA%\.mudcrab-extraction-state` when
  `LOCALAPPDATA` is an absolute path. Otherwise, it uses your home directory.

After an interrupted extraction, retry against the same root to reclaim verified staging
files. Cleanup reports entries it skips, including legacy `.mudcrab-extract-run-*` entries
in the extraction root. A staging prefix alone does not establish ownership.

Stop all archive extractions for your account before removing any staging state. You can
delete an empty state directory. For an abandoned run, remove only files you can verify
belong to that run, and remove its payloads before its registry. The registry lets a retry
find the payloads. Keep unknown files and symlinks, including any entries with a legacy
prefix whose origin you cannot verify.

## Reference library

`<storage>/reference/` is a local, read-only library of third-party modding references for
retail-parity research. Git does not track it. Start at `reference/INDEX.md`.

- `<name>@<commit>/src` holds a shallow git clone. `<name>@<version>/files` holds an
  unpacked archive. Archive copies are in `reference/downloads/`.
- Each item has a `REFERENCE.md` card: source URL, licence file, commit or SHA-256,
  retrieval date, tier and use. Adding an item means writing its card and an `INDEX.md` line.
- `local-pointers/` holds notes that point to installs or other projects. They are never copies.
- `derived/` holds private outputs built from the references, each with a `RECEIPT.md`.
- Clones and unpacked files are `chmod a-w`. Never write into them.
- Read and cite by default. Never copy code from `reference/` into the repo. Never
  redistribute the archives or any data files they contain. Outputs derived from copyleft
  or decompiled code stay private. They never go into a PR, issue or GitHub text.
- Downloads are untrusted. Do not run their build scripts or binaries. Run Python that
  parses them with `-I`.
- `prune` and `settle` delete only inside `builds/`, so they never remove `reference/`. Delete an
  item only after you check its consumers and its card.

## Budgets and cleanup

```sh
python3 scripts/workspace.py audit
python3 scripts/workspace.py preflight --output /absolute/path/to/new-pack --needed-gib 80
python3 scripts/workspace.py settle
python3 scripts/workspace.py prune
python3 scripts/workspace.py settle --apply
python3 scripts/workspace.py prune --apply
```

`audit` is read-only and includes artifact directories in every Git-registered worktree,
including temporary and nested agent checkouts. It scans metadata, skips symlinks, and
deduplicates hard links across roots. It does not hash files to find identical content.
APFS clones, compression, and snapshots mean these totals do not predict reclaimed space.

Build preflight defaults to 8 GiB estimated additional space, a 40 GiB free-space reserve,
and a 190 GiB managed-build budget. Override the estimate with `cargo --needed-gib N ...`.
Set `MUDCRAB_RESERVE_GIB` or `MUDCRAB_BUILD_BUDGET_GIB` deliberately for a different host.
Conversion preflight checks the destination filesystem and bypasses the unrelated build
budget. Estimates are checks, not reservations or hard quotas; concurrent writers or a
larger-than-estimated build can exhaust them. The next build is blocked if the budget is
exceeded. The wrapper reports a post-build overrun without changing Cargo's exit status.

`settle` lists this checkout's isolated builds that could be retired. `prune` lists builds unused
for a set time. Both are dry runs. Add `--apply` to delete. A deletion needs all checks in the
next section to pass. If any check fails or cannot run, the build stays.
Windows uses native process leases; its campaign changes still need validation on Windows.
Pruning never deletes packs, scratch, evidence, source game data, or pre-existing `target/`
directories. Review legacy artifacts separately; preserve active owners and required evidence.
Do not use blanket `git clean -fdx` or repeatedly run `cargo clean`.

## Automatic build cleanup

Three rules keep build storage bounded. Each rule only touches tool-owned directories under
`<storage>/builds` and prints one `workspace: removed ...` line to stderr for every removal. It also
writes a receipt line to `<storage>/evidence/cleanup-receipts.jsonl` (path, bytes, reason, status,
checks passed). A full receipt `evidence/retired-<build>-<ns>.json` is saved before each build
directory is removed.

| Rule | When | What is removed |
| --- | --- | --- |
| On success | `cargo --isolated LABEL check`, `clippy`, `fmt`, `test` or `nextest run` exits 0 | That isolated build. Not after a failure: the tool keeps that build so you can inspect it, but a later passing check or test of the same label can supersede it after 30 minutes. Copy any evidence you need to `evidence/`. Not if the directory holds a result (see below). |
| Superseded | A passing check or test (the rules above) ends | Older heads of the same checkout and label that hold no result and were idle for 30 minutes. A `build`, `run` or `exec` never supersedes anything, so A/B comparisons keep both builds. |
| Slot incremental | A slot command ends and `target/*/incremental` totals more than 8 GiB | Only `target/<profile>/incremental`. Never `deps`, `build`, binaries or `build.json`. Cargo recreates it, so the next build in that slot is slower. The delete runs while the slot lease is held. |

A directory **holds a result** when any `build`, `run`, `exec`, `doc`, `--no-run`, `--open`,
`--timings` or `--message-format` command ever used it. The tool marks `build.json` with
`"deliverable": true` and keeps the mark for the life of the directory. A directory that only
checks and tests created is marked `"deliverable": false`. A `build.json` with no such key (made by
an older script, or rewritten by one) counts as holding a result. So
`cargo --isolated bench build --release` followed by `cargo --isolated bench test` keeps the
release binary. Only a person who runs `settle --apply` or `prune --apply` can retire such a build.
`settle --apply` and `prune --isolated-ttl-hours` ignore the mark by design: a build that holds a
result and has been idle for 30 minutes (settle) or the TTL is removed unless it is pinned, in
`keep-labels`, or named in a pending receipt. Record a checksum and copy a binary you need under
`evidence/` first.

Keep an isolated build when you need it later:

```sh
python3 scripts/workspace.py cargo --isolated pr123 --keep build --locked --release --bin engine
python3 scripts/workspace.py cargo --isolated pr123 --keep --keep-days 14 test --locked -p converter
```

Put `--keep` and `--keep-days` before the Cargo subcommand. A pin lives in `<storage>/pins/<build>.json`
and also blocks `settle --apply` and `prune --apply` until it expires (7 days by default).
A multi-step flow that reuses one label and runs only checks and tests should use `--keep`,
because a retired build is rebuilt from nothing.

**Keep list.** Labels listed in `<storage>/keep-labels` (one per line or comma separated, `#`
comments, case ignored) and in `MUDCRAB_KEEP_LABELS` are never retired by any command. The file is
shared by every owner. The variable only protects builds in a process that has it set. An unreadable
keep file keeps every build.

Never deleted, whatever the rule: a build that is leased, has an open file or running process
(`lsof +D`; a missing or failing `lsof` keeps the build), is pinned, is on the keep list, is named in
a pending receipt, contains a symlink, contains a mount point or any entry on another device
(`st_dev`), is itself a mount point, lacks a valid `build.json` (including a finite `last_used`),
or sits outside `<storage>/builds`. `prune --apply` and `settle --apply` run all of these checks.
The trim and the crash recovery run only the lease, open-file, symlink and device checks. The trim
touches only the regenerable `incremental` directory. Recovery treats a leftover as the tool's own
only if it holds a `build.json` (for a retired build) or the receipt for that removal lists the
exact leftover path under `builds/`. Older receipts omit those paths. Recovery holds older
markerless leftovers for manual inspection. Whole slots are only removed by `prune` with an age
(7 days by default), never by the automatic rules. The tool never deletes anything in `packs/` or
`scratch/`. In `evidence/` it only adds receipts. It removes a receipt only when it wrote that
receipt moments before and the removal then failed to start.

A pending receipt is any `<storage>/evidence/<task>/build-receipt.json` with
`{"state": "pending", "builds": ["isolated-..."]}` (any state other than `"final"` counts).
Write one before you cite a build as evidence. Set `"state": "final"` when the evidence is saved.
One unreadable receipt keeps every build until it is fixed; the first command that meets it prints a
warning that names the file.

Deletion is crash-safe: the directory is first renamed to `.retiring-<build>-<ns>` (or
`.trim-incremental-<ns>` for slot state), then deleted. A crash leaves the renamed directory.
The next `cargo`, `exec` or `prune --apply` finishes it after the checks above. Cleanup errors
print `workspace: cleanup skipped` and never change the exit status of your command.
A new deletion needs its receipt first. Crash recovery finishes a deletion that already had one,
and it records its own receipt line afterwards. If the evidence volume is full or read-only, no
new deletion starts. Free space by hand in that case: remove an idle `builds/isolated-*` directory you own
after the same checks (no lease, `lsof +D` empty, not pinned), then write a note under `evidence/`.

| Setting | Default | Meaning |
| --- | --- | --- |
| `MUDCRAB_SLOT_INCREMENTAL_GIB` | 8 | Slot incremental size that starts a trim. `0` (or a bad value) turns it off. |
| `MUDCRAB_LABEL_CAP_GIB` | 12 | A command prints a warning when all builds of one label exceed this size. It deletes nothing. `0` turns it off. |
| `MUDCRAB_KEEP_LABELS` | empty | Comma-separated labels that are never retired in this process. Use `keep-labels` for a shared list. |

Flags: `cargo` and `exec` take `--keep` and `--keep-days N` (with `--isolated`). `settle` takes
`--apply`. `prune --older-than-days N` applies one age to every build (default 7 days; other owners'
isolated builds keep that age). `prune --isolated-ttl-hours H` (at least 1) gives isolated builds a
shorter age. It is never applied unless you pass it.

Do not run `workspace.py install` from a checkout that carries an older `scripts/workspace.py`:
it copies that old file over the installed one.

## T3 lifecycle hooks and older branches

Run `python3 scripts/workspace.py install --git-hook` to copy the entrypoint, this guide, and agent rules
into shared `bin/`. This is an explicit local installation; ordinary `setup` never updates
that installed version. `--git-hook` installs setup as Git's shared `post-checkout` hook,
which runs on ordinary branch checkout and `git worktree add` (except `--no-checkout`).
It refuses to replace another checkout hook or a configured `core.hooksPath`; chain setup
through that hook manager instead. Omit `--git-hook` to install only the bootstrap.

Where T3 permissions allow, configure worktree-create and settlement hooks to run that absolute
installed script with `setup` and `settle`, respectively. The hooks work even when an older
branch lacks these tracked files. Setup creates an `AGENTS.md` only when it is absent;
it never overwrites existing instructions. A backfilled file is intentionally visible as
untracked work. Review it before committing or retiring that checkout.

Hooks print the shared paths and required workflow. They do not inject environment variables
into future shells, intercept bare `cargo`, delete old builds, or prevent an agent ignoring
instructions. Keep `AGENTS.md` in the main branch so future agents see the rules directly.
After changing these tools or rules, test them and run `install` again to refresh hooks.
