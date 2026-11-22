#!/usr/bin/env python3
"""Fail Clippy's lossy-cast lints on the lines a change adds.

Clippy finds `as` casts that truncate, change sign or wrap, and `==` on floats.
A truncating cast of a form ID, an offset or a count silently changes data. This
gate reads the JSON log of one Clippy pass and fails only when one of the four
lints fires on a line the change adds, in the `converter` or `shared` crate:

  cargo clippy --workspace --all-targets --all-features --message-format=json \\
    -- -W clippy::cast_possible_truncation -W clippy::cast_sign_loss \\
       -W clippy::cast_possible_wrap -W clippy::float_cmp > clippy.json
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json clippy.json --base origin/main

The added lines come from `git diff --unified=0 -M -C <merge-base>...HEAD`. An
added line whose exact text was also deleted in the same diff is a move, not a
new line; the match is a multiset, as in `scripts/check_retail_claims.py`. A
diagnostic counts when any line of its primary span is new, or when any line of
a macro call site is, at every expansion level. An existing cast on unchanged
lines is reported in one information line and never fails a pull request.

The guarantee is "no net-new identical cast line", not "no new cast": a change
can add a cast and delete the byte-identical line anywhere in the same diff,
including a file this gate ignores.

There is no count file. A committed count would be a merge-conflict hotspot:
every change that adds or removes a cast would edit the same line, and two
changes that each add one cast could both pass alone and still leave the count
on main wrong.

The pass runs no `-D warnings`, because that deny still catches the four lints.
The four lints are allow-by-default, but the later `-W` makes one a warning,
and the `-D warnings` deny turns every warning into an error. Clippy says so in the
diagnostic: `-D clippy::cast-possible-truncation` is implied by `-D warnings`.
A probe confirms it: `cargo clippy -- -D warnings -W
clippy::cast_possible_truncation` on `v as u8` exits 101 with the cast as an
`error`. `--force-warn` would avoid the error, but it overrides
`#[allow(..., reason = "...")]`, which would hide the approved sites. Clippy
exits 0 when the tree has only warnings, so this script also fails on every
warning outside the four lints; a hard error still makes Clippy exit nonzero,
and the workflow keeps that status.

The gate covers the `lib` and `bin` targets of the two crates. `--all-targets`
also builds the test harness of every lib and bin, so unit-test casts count too;
the completeness check requires those test artifacts as well as the ordinary
ones. Integration tests, benches and examples are excluded by `target.kind`.
Only messages whose crate is in `CRATES` are considered; path dependencies such
as `mo2` and the vendored parsers report their own hits, which are outside this
gate.

An exception needs a reason at the site, so the reason stays with the cast:

  #[allow(clippy::cast_possible_truncation, reason = "a form ID is below 2^24")]

`reason` needs Rust 1.81 or later; `rust-toolchain.toml` pins a newer release.
No lint enforces the reason (`clippy::allow_attributes_without_reason` is off),
so an allow without one is a review finding, not a CI failure.

Usage:
  python3 tools/cast_ratchet/cast_ratchet.py --lint-flags         # flags for the Clippy run
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log --base origin/main
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log --base <sha>

Exit codes: 0 = no new hit and no other warning, 1 = a new hit or a warning
outside the four lints, 2 = usage, log, diff or required-artifact error.
"""

import argparse
import json
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

# Keep this list in step with the Clippy command: `--lint-flags` prints it, and
# the clippy workflow fails when the printed list differs from its array.
LINTS = (
    "clippy::cast_possible_truncation",
    "clippy::cast_sign_loss",
    "clippy::cast_possible_wrap",
    "clippy::float_cmp",
)
CRATES = ("converter", "shared")
TARGET_KINDS = {"lib", "bin"}
# A log that lacks one of these under `lib` or `bin`, in the ordinary build or in
# the unit-test build (`--all-targets`), undercounts the crate, so the run is an
# error.
REQUIRED_KINDS = {"converter": ("lib", "bin"), "shared": ("lib", "bin")}
COUNTING_LEVELS = {"warning", "error"}
HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")


class LogError(Exception):
    """The Clippy log is missing, malformed or incomplete."""


class DiffError(Exception):
    """The range to compare is missing, or its diff cannot be read."""


@dataclass(frozen=True)
class Hit:
    """One Clippy diagnostic in a gated crate, with its effective locations."""

    crate: str
    lint: str
    message: str
    rendered: str
    locations: tuple


def crate_of(record):
    """The workspace crate directory that produced a Cargo JSON record, or None."""
    manifest = record.get("manifest_path")
    if manifest:
        return Path(manifest).parent.name
    package = record.get("package_id", "")
    if "#" in package:
        return package.split("#", 1)[1].split("@", 1)[0]
    return None


def call_sites_of(span):
    """Every macro call site of a span, innermost first.

    Cargo puts the expansion under `expansion.span`; some Cargo versions also
    expose `expansion.call_site`. A span for an expansion can itself expand, so
    a nested macro has one call site per level. The walk stops at a repeated
    span, which also protects against a cycle.
    """
    sites = []
    seen = set()
    current = span
    while True:
        expansion = current.get("expansion") or {}
        site = expansion.get("call_site") or expansion.get("span") or {}
        key = (
            site.get("file_name"),
            site.get("line_start"),
            site.get("line_end"),
            site.get("column_start"),
        )
        if site.get("file_name") is None or key in seen:
            break
        seen.add(key)
        sites.append(site)
        current = site
    return sites


def span_sites(message):
    """Each primary span, followed by its macro call sites, innermost first."""
    spans = message.get("spans") or []
    primary = [span for span in spans if span.get("is_primary")] or spans
    return [site for span in primary for site in [span, *call_sites_of(span)]]


def span_ranges(message):
    """The file and line range of each primary span and each macro call site."""
    ranges = []
    for site in span_sites(message):
        path = site.get("file_name")
        start = site.get("line_start")
        if path is None or start is None:
            continue
        end = site.get("line_end") or start
        ranges.append((path, int(start), max(int(end), int(start))))
    return tuple(dict.fromkeys(ranges))


def identity_of(message):
    """A stable key for a diagnostic, so the lib and bin reports collapse.

    A local `macro_rules!` reports every use at the definition span. The key
    includes the primary span and every call site, with their columns, so two
    casts on one line or in one expansion stay apart while the lib and lib-test
    reports of one cast share a key, as the count did before the gate.
    """
    return tuple(
        (
            site.get("file_name"),
            site.get("line_start"),
            site.get("line_end"),
            site.get("column_start"),
            site.get("column_end"),
        )
        for site in span_sites(message)
    )


def locations_of(message):
    """The file and line range of every primary span, and of its macro call sites.

    A local `macro_rules!` reports its cast at the definition span, with the
    invocation as the call site. A change can add either one: a new invocation
    is a new use, and a new body line is a new cast. Both count. Without a
    primary span the diagnostic cannot be placed, and the caller fails closed.
    """
    return span_ranges(message)


def read_log(path):
    """Read a Cargo Clippy JSON log.

    Returns the in-scope diagnostics, the warnings outside the four lints and a
    list of problems that make the log incomplete. A malformed record raises
    LogError, because no count from it is trustworthy.
    """
    diagnostics = []
    artifacts = set()
    seen = set()
    reported = set()
    ungated_seen = set()
    ungated = []
    finished = False
    problems = []
    try:
        handle = open(path, encoding="utf-8")
    except OSError as error:
        raise LogError(f"cannot read {path}: {error}") from error
    with handle:
        for number, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                record = json.loads(line)
            except ValueError as error:
                raise LogError(f"{path}:{number}: not a JSON record: {error}") from error
            if not isinstance(record, dict):
                raise LogError(f"{path}:{number}: not a JSON object")
            reason = record.get("reason")
            if reason == "build-finished":
                finished = bool(record.get("success"))
                continue
            if reason == "compiler-artifact":
                crate = crate_of(record)
                if crate:
                    kinds = (record.get("target") or {}).get("kind") or []
                    test = bool((record.get("profile") or {}).get("test"))
                    artifacts.update((crate, kind, test) for kind in kinds)
                continue
            if reason != "compiler-message":
                continue
            message = record.get("message") or {}
            code = (message.get("code") or {}).get("code")
            level = message.get("level")
            crate = crate_of(record)
            if level not in COUNTING_LEVELS:
                continue
            key = (crate, code, message.get("message"), level, identity_of(message))
            if level == "error" and message.get("rendered") and key not in reported:
                reported.add(key)
                print(message["rendered"], file=sys.stderr)
            if code not in LINTS:
                # Clippy exits 0 when it reports only warnings, so this gate is the
                # only check on every other lint. Print each one once.
                if key not in ungated_seen:
                    ungated_seen.add(key)
                    if level != "error" and message.get("rendered"):
                        print(message["rendered"], file=sys.stderr)
                    ungated.append(
                        f"{crate or 'unknown crate'}: {code or 'no lint code'}: "
                        f"{message.get('message')}"
                    )
                continue
            if crate is None:
                raise LogError(f"{path}:{number}: {code} has no crate manifest path")
            if crate not in CRATES:
                continue
            kinds = set((record.get("target") or {}).get("kind") or [])
            if not kinds:
                raise LogError(
                    f"{path}:{number}: {code} in {crate} has no target kind, so the "
                    "span cannot be attributed; check the Cargo JSON format"
                )
            if not (kinds & TARGET_KINDS):
                continue
            locations = locations_of(message)
            if not locations:
                raise LogError(
                    f"{path}:{number}: {code} in {crate} has no span, so the gate cannot "
                    "tell whether the line is new; check the Cargo JSON format"
                )
            key = (crate, code, message.get("message"), identity_of(message))
            if key in seen:
                continue
            seen.add(key)
            diagnostics.append(
                Hit(
                    crate=crate,
                    lint=code,
                    message=message.get("message") or "",
                    rendered=message.get("rendered") or "",
                    locations=locations,
                )
            )
    if not finished:
        problems.append(
            f"{path}: no successful build-finished record; the Clippy run did not finish"
        )
    missing = [
        f"{crate} {kind}{' test' if test else ''}"
        for crate in CRATES
        for kind in REQUIRED_KINDS[crate]
        for test in (False, True)
        if (crate, kind, test) not in artifacts
    ]
    if missing:
        problems.append(
            f"{path}: no compiler-artifact record for "
            + ", ".join(missing)
            + "; check that the Clippy command selects every gated target and "
            "builds its unit-test harness with --all-targets"
        )
    return diagnostics, ungated, problems


def git(root, *args):
    """Run git in the repository and return stdout, or raise DiffError."""
    command = ["git", "-C", str(root), "-c", "core.quotePath=false", *args]
    result = subprocess.run(
        command,
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        check=False,
    )
    if result.returncode:
        message = result.stderr.strip() or result.stdout.strip()
        raise DiffError(f"{' '.join(command)}: {message}")
    return result.stdout


def read_diff(root, base, head="HEAD"):
    """Return the added lines per path and the texts of the deleted lines."""
    # `git diff A...B` needs a merge base. Ask for it first: an empty answer
    # would compare with the empty tree and treat every line as new.
    merge_base = git(root, "merge-base", base, head).splitlines()
    if not merge_base or not merge_base[0].strip():
        raise DiffError(f"{base} and {head} have no merge base")
    diff = git(
        root,
        "diff",
        "--unified=0",
        "-M",
        "-C",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        f"{merge_base[0].strip()}...{head}",
    )
    return parse_diff(diff)


def parse_diff(text):
    """Return added lines per path and the multiset of deleted line texts.

    A whole-file deletion prints `+++ /dev/null`, so it has no path and no new
    lines. Its `-` lines still count as deleted text: a cast that moves out of
    a deleted file is a move, not a new cast.
    """
    added = {}
    removed = Counter()
    path = None
    new_line = None
    in_hunk = False
    for raw in text.splitlines():
        if raw.startswith("diff --git "):
            path = None
            new_line = None
            in_hunk = False
        elif not in_hunk and raw.startswith("+++ "):
            target = raw[4:]
            if target == "/dev/null":
                path = None
            else:
                path = target[2:] if target.startswith("b/") else target
                added.setdefault(path, {})
        elif raw.startswith("@@"):
            match = HUNK_RE.match(raw)
            if match is None:
                raise DiffError(f"cannot parse diff hunk: {raw}")
            new_line = int(match.group(1))
            in_hunk = True
        elif in_hunk and new_line is not None:
            if raw.startswith("+"):
                if path is not None:
                    added[path][new_line] = raw[1:]
                new_line += 1
            elif raw.startswith("-"):
                removed[raw[1:]] += 1
            elif raw.startswith("\\"):
                continue
            else:
                new_line += 1
    return added, removed


def new_lines(added, removed):
    """Drop added lines whose exact text was also deleted: they moved, not new."""
    kept = {}
    for path in sorted(added):
        lines = {}
        for line in sorted(added[path]):
            text = added[path][line]
            if removed[text] > 0:
                removed[text] -= 1
                continue
            lines[line] = text
        if lines:
            kept[path] = lines
    return kept


def relative_to_root(path, root):
    """A repository-relative POSIX path for a span or diff file name."""
    candidate = Path(path)
    if candidate.is_absolute():
        try:
            candidate = candidate.relative_to(root)
        except ValueError:
            return path
    return candidate.as_posix()


def added_location(locations, added, root):
    """The first line that is added inside a diagnostic span, or None."""
    for path, start, end in locations:
        relative = relative_to_root(path, root)
        present = added.get(relative, ())
        for line in range(start, end + 1):
            if line in present:
                return relative, line
    return None


def print_ungated(ungated):
    """Report the diagnostics outside the four lints that make the pass fail."""
    print(
        f"cast gate: {len(ungated)} diagnostic(s) outside the four gate lints. "
        "The Clippy pass does not deny warnings, so this gate does; fix them.",
        file=sys.stderr,
    )
    for label in ungated:
        print(f"cast gate:   {label}", file=sys.stderr)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--clippy-json", type=Path, help="Cargo Clippy JSON log")
    parser.add_argument(
        "--base",
        help="commit or ref to compare with; the diff uses its merge base with HEAD",
    )
    parser.add_argument("--root", type=Path, default=Path("."), help="repository root")
    parser.add_argument(
        "--lint-flags",
        action="store_true",
        help="print the Clippy flags for these lints and exit",
    )
    args = parser.parse_args(argv)

    if args.lint_flags:
        if args.clippy_json is not None or args.base is not None:
            parser.error(
                "--lint-flags prints the flags and runs nothing; run the gate in a "
                "second command with --clippy-json and --base"
            )
        print(" ".join(f"-W {lint}" for lint in LINTS))
        return 0
    if args.clippy_json is None:
        parser.error("--clippy-json is required unless --lint-flags is given")
    if args.base is None:
        parser.error("--base is required unless --lint-flags is given")

    root = args.root.resolve()
    try:
        diagnostics, ungated, problems = read_log(args.clippy_json)
    except LogError as error:
        print(f"cast gate: {error}", file=sys.stderr)
        return 2
    try:
        added, removed = read_diff(root, args.base)
    except DiffError as error:
        print(f"cast gate: {error}", file=sys.stderr)
        return 2

    added = new_lines(added, removed)
    hits = []
    existing = 0
    for hit in diagnostics:
        where = added_location(hit.locations, added, root)
        if where is None:
            existing += 1
        else:
            hits.append((where, hit))
    hits.sort(key=lambda item: (item[0][0], item[0][1], item[1].lint))

    print(
        f"cast gate: {existing} existing hit(s) on unchanged lines; only added lines "
        "are gated."
    )
    sys.stdout.flush()
    for (path, line), hit in hits:
        print(f"{path}:{line}: {hit.lint}: {hit.message}", file=sys.stderr)
        if hit.rendered:
            print(hit.rendered, file=sys.stderr)

    if problems:
        print("cast gate: the log is incomplete; the count above may be low", file=sys.stderr)
        for problem in problems:
            print(f"cast gate: {problem}", file=sys.stderr)
        return 2

    if hits:
        print(
            f"cast gate: {len(hits)} new Clippy hit(s) on added lines. Replace a lossy "
            "cast with a checked conversion or an exact float comparison with a "
            "tolerance, or allow the site with "
            '`#[allow(<lint>, reason = "...")]`.',
            file=sys.stderr,
        )
    if ungated:
        print_ungated(ungated)
    if hits or ungated:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
