#!/bin/sh
# Fails when a Kani log does not prove what the crate claims.
#
# Kani 0.68.0 reports `VERIFICATION:- SUCCESSFUL` and exits 0 in cases that prove nothing: a
# `kani::cover!` that is `UNSATISFIABLE`, an assertion that is `UNREACHABLE` (for example after a
# contradictory `kani::assume`), and a run that verified fewer harnesses than the crate has. This
# script fails on all three. It fails closed:
#
# - The summary must report every `#[kani::proof]` in this crate's `src/`, with 0 failures.
# - An `UNREACHABLE` check fails unless its location is in the Rust or Kani library
#   (`library/core`, `alloc`, `std`, `kani_core` or `kani`). Checks are read as whole blocks, so
#   the order of the `Status` and `Location` lines does not matter, and a missing location fails.
#
# Usage: check-kani-log.sh <kani log> [expected harness count]
# Give the count only for a run that uses `--harness`.
set -eu
log=$1
src="$(dirname "$0")/src"
expected=${2:-$(grep -rho '#\[kani::proof\]' "$src" | wc -l | tr -d ' ')}
[ "$expected" -gt 0 ] || {
    echo "check-kani-log: no #[kani::proof] found in $src" >&2
    exit 1
}
summary="Complete - $expected successfully verified harnesses, 0 failures, $expected total."
grep -qxF "$summary" "$log" || {
    echo "check-kani-log: $log has no line \"$summary\"" >&2
    grep '^Complete - ' "$log" >&2 || true
    exit 1
}
awk '
    function finish() {
        if (status == "UNREACHABLE" && location !~ /(^|\/)library\/(core|alloc|std|kani_core|kani)\/src\//) {
            print "check-kani-log: unreachable check outside the libraries: " check " at " location
            bad = 1
        }
        if (status == "UNSATISFIABLE") {
            print "check-kani-log: unsatisfiable cover: " check " at " location
            bad = 1
        }
        check = ""; status = ""; location = "(no location)"
    }
    BEGIN { location = "(no location)" }
    /^Check [0-9]+:/ { finish(); check = $0; next }
    # A description can wrap onto an unindented line, so only a blank line ends a check.
    /^[[:space:]]*\r?$/ { finish(); next }
    /- Status: / { sub(/.*- Status: /, ""); sub(/\r$/, ""); status = $0; next }
    /- Location: / { sub(/.*- Location: /, ""); gsub(/\\/, "/"); location = $0; next }
    END { finish(); exit bad }
' "$log" >&2
echo "check-kani-log: $expected harnesses verified; no unsatisfiable cover; no unexpected unreachable check"
