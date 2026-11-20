#!/usr/bin/env python3
"""Ratchet the count of lossy casts and exact float comparisons that Clippy reports.

Clippy finds `as` casts that truncate, change sign or wrap, and `==` on floats. A
truncating cast of a form ID, an offset or a count silently changes data, so the
count may not rise. `baseline.json` holds the committed count per crate and lint.
A fall is reported with the new number, and `--update` lowers the baseline.

The ratchet reads a Clippy JSON log instead of running Clippy itself, so the
normal `-D warnings` gate stays unchanged:

  cargo clippy -p converter -p shared --lib --bins --message-format=json \\
    -- -W clippy::cast_possible_truncation -W clippy::cast_sign_loss \\
       -W clippy::cast_possible_wrap -W clippy::float_cmp > clippy.json
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json clippy.json

The count covers the `lib` and `bin` targets only. Run the command above: it
builds no test harnesses, examples or benches. A log from `--all-targets` also
holds those builds, and Cargo's message records do not identify them, so their
hits would count. Only messages whose crate is in `CRATES` are counted; path
dependencies such as `mo2` and the vendored parsers report their own hits, which
are outside this ratchet.

An exception needs a reason at the site, so the reason stays with the cast:

  #[allow(clippy::cast_possible_truncation, reason = "a form ID is below 2^24")]

`reason` needs Rust 1.81 or later; `rust-toolchain.toml` pins a newer release.

Usage:
  python3 tools/cast_ratchet/cast_ratchet.py --lint-flags          # flags for the Clippy run
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log     # check against the baseline
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log --update
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log --crate shared
  python3 tools/cast_ratchet/cast_ratchet.py --clippy-json log --strict

Exit codes: 0 = at or below the baseline, 1 = a count rose (or a fall with
`--strict`), 2 = usage, log or baseline error.
"""

import argparse
import json
import sys
from pathlib import Path

# Keep this list in step with the Clippy command: `--lint-flags` prints it.
LINTS = (
    "clippy::cast_possible_truncation",
    "clippy::cast_sign_loss",
    "clippy::cast_possible_wrap",
    "clippy::float_cmp",
)
CRATES = ("converter", "shared")
TARGET_KINDS = {"lib", "bin"}
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


def span_key(message):
    """A stable key for the primary span of a diagnostic, so duplicates collapse."""
    spans = message.get("spans") or []
    primary = [span for span in spans if span.get("is_primary")] or spans
    return tuple(
        (span.get("file_name"), span.get("line_start"), span.get("column_start"))
        for span in primary
    )


def read_log(path, expected=CRATES):
    """Count each lint per crate in a Cargo Clippy JSON log.

    Raises LogError when the log is unreadable, malformed, incomplete or has no
    record for a crate in `expected`. A diagnostic that the count cannot
    attribute is an error, not a skipped line.
    """
    counts = {crate: {lint: 0 for lint in LINTS} for crate in CRATES}
    artifacts = set()
    seen = set()
    finished = False
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
                    artifacts.add(crate)
                continue
            if reason != "compiler-message":
                continue
            message = record.get("message") or {}
            if message.get("level") == "error" and message.get("rendered"):
                print(message["rendered"], file=sys.stderr)
            code = (message.get("code") or {}).get("code")
            if code not in LINTS:
                continue
            crate = crate_of(record)
            if crate is None:
                raise LogError(f"{path}:{number}: {code} has no crate manifest path")
            if crate not in counts:
                continue
            target = record.get("target") or {}
            if not (set(target.get("kind") or []) & TARGET_KINDS):
                continue
            if message.get("level") not in COUNTING_LEVELS:
                continue
            key = (crate, code, message.get("message"), span_key(message))
            if key in seen:
                continue
            seen.add(key)
            counts[crate][code] += 1
    if not finished:
        raise LogError(
            f"{path}: no successful build-finished record; the Clippy run did not finish"
        )
    missing = [crate for crate in expected if crate not in artifacts]
    if missing:
        raise LogError(
            f"{path}: no compiler-artifact record for "
            + ", ".join(missing)
            + "; check that the Clippy command selects every ratcheted crate"
        )
    return counts


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
        measured = read_log(args.clippy_json, expected)
    except LogError as error:
        print(f"cast ratchet: {error}", file=sys.stderr)
        return 2

    if args.update:
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
        return 1

    for crate, lint, count, limit in lowered:
        print(
            f"cast ratchet: {crate} {lint} fell from {limit} to {count}. Lower the baseline "
            f"with `python3 tools/cast_ratchet/cast_ratchet.py --clippy-json <log> --update`; "
            f"it writes {count}.",
            file=sys.stderr,
        )
    if lowered and args.strict:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
