#!/usr/bin/env python3
"""Ratchet the count of lossy casts and exact float comparisons that Clippy reports.

Clippy finds `as` casts that truncate, change sign or wrap, and `==` on floats. A
truncating cast of a form ID, an offset or a count silently changes data, so the
count may not rise. `baseline.json` holds the committed count per crate and lint.
A fall is reported with the new number, and `--update` lowers the baseline.

The ratchet reads the JSON log of the normal Clippy pass:

  cargo clippy --workspace --all-targets --all-features --message-format=json \\
    -- -W clippy::cast_possible_truncation -W clippy::cast_sign_loss \\
       -W clippy::cast_possible_wrap -W clippy::float_cmp > clippy.json
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json clippy.json

The pass runs no `-D warnings`. The four lints are allow-by-default, and `-W`
raises them to warnings. `-D warnings` cannot stay: it denies the four, and `-W`
cannot lower a deny from a lint group, while `--force-warn` overrides
`#[allow(..., reason = "...")]` and would hide the approved sites. Clippy exits
0 when the tree has only warnings, so this script is the gate for every
diagnostic outside the four lints: it prints each one and fails. A hard error
still makes Clippy exit nonzero, and the workflow keeps that status.

The count covers the `lib` and `bin` targets of the ratcheted crates.
`--all-targets` also builds the test harness of every lib and bin, and a message
record has no `profile` field, so unit-test casts count too. Integration tests,
benches and examples are excluded by `target.kind`. Only messages whose crate is
in `CRATES` are counted; path dependencies such as `mo2` and the vendored
parsers report their own hits, which are outside this ratchet.

The baseline comes from the Linux CI run. A cast under `#[cfg(windows)]` or
another platform `cfg` is not in the Linux log. No ratcheted cast is under a
platform `cfg` today. A cast added there will not raise the Linux count, and a
`--update` on another platform would commit a different baseline; keep the
baseline measured on Linux only.

An exception needs a reason at the site, so the reason stays with the cast:

  #[allow(clippy::cast_possible_truncation, reason = "a form ID is below 2^24")]

`reason` needs Rust 1.81 or later; `rust-toolchain.toml` pins a newer release.
No lint enforces the reason (`clippy::allow_attributes_without_reason` is off),
so an allow without one is a review finding, not a CI failure.

Usage:
  python3 tools/cast_ratchet/cast_ratchet.py --lint-flags          # flags for the Clippy run
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log     # check against the baseline
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log --update
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log --crate shared
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log --strict

Exit codes: 0 = at or below the baseline, 1 = a count rose (or a fall with
`--strict`, or a diagnostic outside the four lints), 2 = usage, log or baseline
error.
"""

import argparse
import json
import sys
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
# A log that lacks one of these undercounts the crate, so the run is an error.
REQUIRED_KINDS = {"converter": ("lib", "bin"), "shared": ("lib",)}
DEFAULT_BASELINE = Path("tools/cast_ratchet/baseline.json")
COUNTING_LEVELS = {"warning", "error"}


class LogError(Exception):
    """The Clippy log is missing, malformed or incomplete."""


class BaselineError(Exception):
    """The baseline file is missing or does not cover every crate and lint."""


def crate_of(record):
    """The workspace crate directory that produced a Cargo JSON record, or None."""
    manifest = record.get("manifest_path")
    if manifest:
        return Path(manifest).parent.name
    package = record.get("package_id", "")
    if "#" in package:
        return package.split("#", 1)[1].split("@", 1)[0]
    return None


def call_site_of(span):
    """The macro call site of a span, or an empty dict.

    Cargo puts the expansion under `expansion.span`; some Cargo versions also
    expose `expansion.call_site`.
    """
    expansion = span.get("expansion") or {}
    return expansion.get("call_site") or expansion.get("span") or {}


def span_key(message):
    """A stable key for the primary spans of a diagnostic, so duplicates collapse.

    A local `macro_rules!` reports every use at the definition span. The call site
    keeps those uses apart while the lib and lib-test reports of one cast still
    share a key.
    """
    spans = message.get("spans") or []
    primary = [span for span in spans if span.get("is_primary")] or spans
    key = []
    for span in primary:
        call_site = call_site_of(span)
        key.append(
            (
                span.get("file_name"),
                span.get("line_start"),
                span.get("column_start"),
                call_site.get("file_name"),
                call_site.get("line_start"),
                call_site.get("column_start"),
            )
        )
    return tuple(key)


def read_log(path, expected=CRATES):
    """Count each lint per crate in a Cargo Clippy JSON log.

    Returns the counts, a list of problems, and a list of diagnostics outside the
    four lints. A problem means the log is incomplete: no successful
    `build-finished` record, or a required crate target missing. The caller
    prints the table, then fails. A diagnostic outside the four lints is also a
    failure, because the Clippy pass no longer denies warnings in the compiler.
    A malformed record or an unattributable diagnostic raises LogError, because
    the counts are then not trustworthy at all.
    """
    counts = {crate: {lint: 0 for lint in LINTS} for crate in CRATES}
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
                    artifacts.update((crate, kind) for kind in kinds)
                continue
            if reason != "compiler-message":
                continue
            message = record.get("message") or {}
            code = (message.get("code") or {}).get("code")
            level = message.get("level")
            crate = crate_of(record)
            if level not in COUNTING_LEVELS:
                continue
            key = (crate, code, message.get("message"), level, span_key(message))
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
            if crate not in counts:
                continue
            kinds = set((record.get("target") or {}).get("kind") or [])
            if not kinds:
                raise LogError(
                    f"{path}:{number}: {code} in {crate} has no target kind, so the "
                    "count cannot attribute it; check the Cargo JSON format"
                )
            if not (kinds & TARGET_KINDS):
                continue
            key = (crate, code, message.get("message"), span_key(message))
            if key in seen:
                continue
            seen.add(key)
            counts[crate][code] += 1
    if not finished:
        problems.append(
            f"{path}: no successful build-finished record; the Clippy run did not finish"
        )
    missing = [
        f"{crate} {kind}"
        for crate in expected
        for kind in REQUIRED_KINDS[crate]
        if (crate, kind) not in artifacts
    ]
    if missing:
        problems.append(
            f"{path}: no compiler-artifact record for "
            + ", ".join(missing)
            + "; check that the Clippy command selects every ratcheted target"
        )
    return counts, problems, ungated


def read_baseline(path):
    """The per-crate, per-lint counts. Raises BaselineError when an entry is missing."""
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise BaselineError(f"cannot read {path}: {error}") from error
    except ValueError as error:
        raise BaselineError(f"{path}: not valid JSON: {error}") from error
    counts = document.get("counts") if isinstance(document, dict) else None
    if not isinstance(counts, dict):
        raise BaselineError(f"{path}: no counts object")
    for crate in counts:
        if crate not in CRATES:
            raise BaselineError(f"{path}: unknown crate {crate}")
        if not isinstance(counts[crate], dict):
            raise BaselineError(f"{path}: counts for {crate} are not an object")
        for lint in counts[crate]:
            if lint not in LINTS:
                raise BaselineError(f"{path}: unknown lint {lint}")
    for crate in CRATES:
        if crate not in counts:
            raise BaselineError(f"{path}: no counts for {crate}; run --update")
        for lint in LINTS:
            if lint not in counts[crate]:
                raise BaselineError(f"{path}: no count for {crate} {lint}; run --update")
            value = counts[crate][lint]
            if not isinstance(value, int) or isinstance(value, bool):
                raise BaselineError(
                    f"{path}: count for {crate} {lint} is not an integer"
                )
    return counts


def write_baseline(path, counts):
    """Write LF newlines on every host, so `--update` on Windows does not rewrite the file."""
    document = {
        "counts": {
            crate: {lint: counts[crate][lint] for lint in LINTS} for crate in CRATES
        }
    }
    text = json.dumps(document, indent=2, sort_keys=True) + "\n"
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)


def print_ungated(ungated):
    """Report the diagnostics outside the four lints that make the pass fail."""
    print(
        f"cast ratchet: {len(ungated)} diagnostic(s) outside the four ratchet lints. "
        "The Clippy pass does not deny warnings, so this gate does; fix them.",
        file=sys.stderr,
    )
    for label in ungated:
        print(f"cast ratchet:   {label}", file=sys.stderr)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--clippy-json", type=Path, help="Cargo Clippy JSON log")
    parser.add_argument("--baseline", type=Path, help=f"default: {DEFAULT_BASELINE}")
    parser.add_argument("--root", type=Path, default=Path("."), help="repository root")
    parser.add_argument(
        "--crate",
        action="append",
        choices=CRATES,
        help="check one crate; the default is every ratcheted crate",
    )
    parser.add_argument("--update", action="store_true", help="write the measured counts")
    parser.add_argument("--strict", action="store_true", help="also fail when a count fell")
    parser.add_argument(
        "--lint-flags",
        action="store_true",
        help="print the Clippy flags for these lints and exit",
    )
    args = parser.parse_args(argv)

    if args.lint_flags:
        print(" ".join(f"-W {lint}" for lint in LINTS))
        return 0
    if args.clippy_json is None:
        parser.error("--clippy-json is required unless --lint-flags is given")

    root = args.root.resolve()
    baseline_path = args.baseline or root / DEFAULT_BASELINE
    expected = args.crate or list(CRATES)

    try:
        measured, problems, ungated = read_log(args.clippy_json, expected)
    except LogError as error:
        print(f"cast ratchet: {error}", file=sys.stderr)
        return 2

    if args.update:
        if problems:
            for problem in problems:
                print(f"cast ratchet: {problem}", file=sys.stderr)
            print(
                "cast ratchet: baseline not written from an incomplete log",
                file=sys.stderr,
            )
            return 2
        if ungated:
            print_ungated(ungated)
            print(
                "cast ratchet: baseline not written while diagnostics outside the four "
                "lints remain",
                file=sys.stderr,
            )
            return 1
        try:
            write_baseline(baseline_path, measured)
        except OSError as error:
            print(f"cast ratchet: cannot write {baseline_path}: {error}", file=sys.stderr)
            return 2
        print(f"cast ratchet: wrote {baseline_path}")
        return 0

    try:
        baseline = read_baseline(baseline_path)
    except BaselineError as error:
        print(f"cast ratchet: {error}", file=sys.stderr)
        return 2

    raised, lowered = [], []
    if problems:
        print("cast ratchet: the log is incomplete; the counts below may be low", file=sys.stderr)
    print(f"{'crate':10} {'lint':34} {'count':>6} / {'baseline':>8}  state")
    for crate in expected:
        for lint in LINTS:
            count = measured[crate][lint]
            limit = baseline[crate][lint]
            if count > limit:
                state = "RAISED"
                raised.append((crate, lint, count, limit))
            elif count < limit:
                state = "below baseline"
                lowered.append((crate, lint, count, limit))
            else:
                state = "ok"
            print(f"{crate:10} {lint:34} {count:6} / {limit:8}  {state}")

    sys.stdout.flush()
    if problems:
        for problem in problems:
            print(f"cast ratchet: {problem}", file=sys.stderr)
        return 2

    for crate, lint, count, limit in raised:
        print(
            f"cast ratchet: {crate} {lint} rose from {limit} to {count}. Replace the lossy "
            f'cast with a checked conversion, or allow the site with '
            f'`#[allow({lint}, reason = "...")]`.',
            file=sys.stderr,
        )
    if raised:
        print(
            "cast ratchet: a justified rise also needs a baseline update with `--update` "
            "and an explanation in the pull request.",
            file=sys.stderr,
        )

    for crate, lint, count, limit in lowered:
        print(
            f"cast ratchet: {crate} {lint} fell from {limit} to {count}. Lower the baseline "
            f"with `python3 tools/cast_ratchet/cast_ratchet.py --clippy-json <log> --update`; "
            f"it writes {count}.",
            file=sys.stderr,
        )

    if ungated:
        print_ungated(ungated)

    if raised or ungated or (lowered and args.strict):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
